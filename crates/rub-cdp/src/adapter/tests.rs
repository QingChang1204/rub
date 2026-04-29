use super::ChromiumAdapter;
use crate::browser::{BrowserLaunchOptions, BrowserManager};
use crate::humanize::{HumanizeConfig, HumanizeSpeed};
use chromiumoxide::Page;
use rub_core::error::ErrorCode;
use rub_core::model::{InteractionConfirmationStatus, KeyCombo};
use rub_core::port::BrowserPort;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::time::{Duration, Instant, sleep};

const TEST_BROWSER_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_BROWSER_POLL_INTERVAL: Duration = Duration::from_millis(20);

fn options() -> BrowserLaunchOptions {
    let unique = format!("{}-{}", std::process::id(), uuid::Uuid::now_v7());
    BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: Some(std::env::temp_dir().join(format!("rub-profile-{unique}"))),
        managed_profile_ephemeral: false,
        download_dir: Some(std::env::temp_dir().join(format!("rub-downloads-{unique}"))),
        profile_directory: Some("Default".to_string()),
        hide_infobars: true,
        stealth: true,
    }
}

fn test_adapter(manager: Arc<BrowserManager>) -> ChromiumAdapter {
    test_adapter_with_humanize(
        manager,
        HumanizeConfig {
            enabled: false,
            speed: HumanizeSpeed::Normal,
        },
    )
}

fn test_adapter_with_humanize(
    manager: Arc<BrowserManager>,
    humanize: HumanizeConfig,
) -> ChromiumAdapter {
    ChromiumAdapter::new(manager, Arc::new(AtomicU64::new(0)), humanize)
}

async fn open_second_tab(manager: &BrowserManager, opener: &Arc<chromiumoxide::Page>, url: &str) {
    let script = format!(
        "window.open({}, '_blank'); null",
        serde_json::to_string(url).unwrap()
    );
    opener
        .evaluate(script)
        .await
        .expect("window.open should succeed");
    let deadline = Instant::now() + TEST_BROWSER_SETTLE_TIMEOUT;
    loop {
        let tabs = manager
            .tab_list()
            .await
            .expect("tab list should be available");
        if tabs.len() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second tab should appear before timeout"
        );
        sleep(TEST_BROWSER_POLL_INTERVAL).await;
    }
}

async fn child_frame_id(page: &Arc<chromiumoxide::Page>) -> String {
    let main = page
        .mainframe()
        .await
        .expect("main frame should be readable")
        .expect("main frame should exist")
        .as_ref()
        .to_string();
    let deadline = Instant::now() + TEST_BROWSER_SETTLE_TIMEOUT;
    loop {
        let frames = page
            .frames()
            .await
            .expect("frame inventory should be readable");
        if let Some(child) = frames
            .into_iter()
            .map(|frame| frame.as_ref().to_string())
            .find(|frame_id| frame_id != &main)
        {
            return child;
        }
        assert!(
            Instant::now() < deadline,
            "child frame should appear before timeout"
        );
        sleep(TEST_BROWSER_POLL_INTERVAL).await;
    }
}

async fn wait_for_frame_context_resolved(page: &Arc<Page>, frame_id: &str) {
    let deadline = Instant::now() + TEST_BROWSER_SETTLE_TIMEOUT;
    loop {
        match crate::frame_runtime::resolve_frame_context(page, Some(frame_id)).await {
            Ok(_) => return,
            Err(error) => {
                let last_error = error.to_string();
                assert!(
                    Instant::now() < deadline,
                    "child frame context should resolve before timeout; last_error={last_error}"
                );
            }
        }
        sleep(TEST_BROWSER_POLL_INTERVAL).await;
    }
}

async fn set_test_html(page: &Arc<Page>, html: &str) {
    page.goto("data:text/html,<html><body></body></html>")
        .await
        .expect("blank page should load");
    page.evaluate(format!(
        "document.body.innerHTML = {}; null",
        serde_json::to_string(html).unwrap()
    ))
    .await
    .expect("test html should install");
}

async fn seed_child_frame_document(page: &Arc<Page>, html: &str) {
    page.evaluate(format!(
        "(() => {{ const frame = document.querySelector('iframe'); const doc = frame && frame.contentWindow && frame.contentWindow.document; if (!doc) return null; doc.open(); doc.write({}); doc.close(); return null; }})()",
        serde_json::to_string(html).unwrap()
    ))
    .await
    .expect("child frame document should install");
}

async fn focus_child_input(page: &Arc<Page>, input_id: &str) {
    let deadline = Instant::now() + TEST_BROWSER_SETTLE_TIMEOUT;
    loop {
        let active_id = crate::js::evaluate_returning_string_in_context(
            page,
            None,
            &format!(
                "(() => {{ const frame = document.querySelector('iframe'); const el = frame && frame.contentWindow && frame.contentWindow.document.getElementById({}); if (!el) return ''; el.focus(); return frame && frame.contentWindow && frame.contentWindow.document.activeElement && frame.contentWindow.document.activeElement.id || ''; }})()",
                serde_json::to_string(input_id).unwrap()
            ),
        )
        .await
        .expect("child input focus should succeed");
        if active_id == input_id {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "child input should become focusable before timeout"
        );
        sleep(TEST_BROWSER_POLL_INTERVAL).await;
    }
}

async fn top_input_value(page: &Arc<Page>, input_id: &str) -> String {
    crate::js::evaluate_returning_string_in_context(
        page,
        None,
        &format!(
            "(() => {{ const el = document.getElementById({}); return el ? String(el.value || '') : ''; }})()",
            serde_json::to_string(input_id).unwrap()
        ),
    )
    .await
    .expect("input value should be readable")
}

async fn child_input_value(page: &Arc<Page>, input_id: &str) -> String {
    crate::js::evaluate_returning_string_in_context(
        page,
        None,
        &format!(
            "(() => {{ const frame = document.querySelector('iframe'); const el = frame && frame.contentWindow && frame.contentWindow.document.getElementById({}); return el ? String(el.value || '') : ''; }})()",
            serde_json::to_string(input_id).unwrap()
        ),
    )
    .await
    .expect("child input value should be readable")
}

async fn arm_top_mutation(page: &Arc<Page>) {
    page.evaluate(
        r#"(() => {
            const started = Date.now();
            const handle = setInterval(() => {
                const counter = document.getElementById('counter');
                if (counter) {
                    counter.value = String(Number(counter.value || '0') + 1);
                }
                if (Date.now() - started > 150) clearInterval(handle);
            }, 5);
        })();
        null"#,
    )
    .await
    .expect("top mutation should arm");
}

async fn arm_top_focus_theft(page: &Arc<Page>) {
    page.evaluate(
        r#"setTimeout(() => {
            const top = document.getElementById('top');
            if (!top) return;
            const started = Date.now();
            const handle = setInterval(() => {
                top.focus();
                if (Date.now() - started > 250) clearInterval(handle);
            }, 5);
        }, 20);
        null"#,
    )
    .await
    .expect("top focus theft should arm");
}

#[test]
fn projected_launch_policy_reports_l2_when_humanize_enabled() {
    let manager = Arc::new(BrowserManager::new(BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    }));
    let adapter = ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        HumanizeConfig {
            enabled: true,
            speed: HumanizeSpeed::Slow,
        },
    );

    let launch_policy = adapter.projected_launch_policy();
    assert_eq!(launch_policy.stealth_level.as_deref(), Some("L2"));
    assert_eq!(launch_policy.humanize_enabled, Some(true));
    assert_eq!(launch_policy.humanize_speed.as_deref(), Some("slow"));
}

#[tokio::test]
async fn snapshot_selector_replay_uses_snapshot_tab_authority_after_tab_switch() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let adapter = test_adapter(manager.clone());
    let first_page = manager.page().await.expect("first page authority");
    first_page
        .goto("data:text/html,<button id='save'>Save</button>")
        .await
        .expect("first page should load");

    let snapshot = adapter
        .snapshot(Some(10))
        .await
        .expect("snapshot should build");

    open_second_tab(
        &manager,
        &first_page,
        "data:text/html,<button id='other'>Other</button>",
    )
    .await;
    manager
        .switch_to_tab(1)
        .await
        .expect("second tab should become active");

    let matches = adapter
        .find_snapshot_elements_by_selector(&snapshot, "#save")
        .await
        .expect("selector replay should stay bound to the snapshot tab");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].text, "Save");

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn snapshot_bound_read_uses_element_tab_authority_after_tab_switch() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let adapter = test_adapter(manager.clone());
    let first_page = manager.page().await.expect("first page authority");
    first_page
        .goto("data:text/html,<button id='save'>Save</button>")
        .await
        .expect("first page should load");

    let snapshot = adapter
        .snapshot(Some(10))
        .await
        .expect("snapshot should build");
    let save = snapshot
        .elements
        .iter()
        .find(|element| element.text == "Save")
        .cloned()
        .expect("snapshot should capture save button");

    open_second_tab(
        &manager,
        &first_page,
        "data:text/html,<button id='other'>Other</button>",
    )
    .await;
    manager
        .switch_to_tab(1)
        .await
        .expect("second tab should become active");

    let text = adapter
        .get_text(&save)
        .await
        .expect("snapshot-bound read should stay bound to the element tab");
    assert_eq!(text, "Save");

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn child_frame_snapshot_replay_preserves_snapshot_tab_authority_after_tab_switch() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let adapter = test_adapter(manager.clone());
    let first_page = manager.page().await.expect("first page authority");
    first_page
        .goto(
            "data:text/html,<iframe srcdoc=\"<button id='child-save'>Child Save</button>\"></iframe>",
        )
        .await
        .expect("first page should load");

    let frame_id = child_frame_id(&first_page).await;
    let snapshot = adapter
        .snapshot_for_frame(Some(frame_id.as_str()), Some(10))
        .await
        .expect("child frame snapshot should build");
    let save = snapshot
        .elements
        .iter()
        .find(|element| element.text == "Child Save")
        .cloned()
        .expect("snapshot should contain the child button");

    open_second_tab(
        &manager,
        &first_page,
        "data:text/html,<button id='other'>Other</button>",
    )
    .await;
    manager
        .switch_to_tab(1)
        .await
        .expect("second tab should become active");

    let matches = adapter
        .find_snapshot_elements_by_selector(&snapshot, "#child-save")
        .await
        .expect("child frame replay should stay bound to the snapshot tab");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].text, "Child Save");
    let text = adapter
        .get_text(&save)
        .await
        .expect("child frame reads should stay bound to the snapshot tab");
    assert_eq!(text, "Child Save");

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn send_keys_in_frame_confirmation_ignores_unrelated_top_page_mutation() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let adapter = test_adapter(manager.clone());
    let page = manager.page().await.expect("page authority");
    set_test_html(
        &page,
        r#"
        <input id="top" value="">
        <input id="counter" value="0" readonly>
        <iframe></iframe>
        "#,
    )
    .await;
    seed_child_frame_document(&page, r#"<input id="child" value="">"#).await;

    let frame_id = child_frame_id(&page).await;
    wait_for_frame_context_resolved(&page, &frame_id).await;
    focus_child_input(&page, "child").await;
    arm_top_mutation(&page).await;

    let outcome = adapter
        .send_keys_in_frame(Some(frame_id.as_str()), &KeyCombo::parse("Enter").unwrap())
        .await
        .expect("frame-scoped key send should complete");
    let confirmation = outcome
        .confirmation
        .expect("frame-scoped key send should publish confirmation");
    assert_ne!(
        confirmation.status,
        InteractionConfirmationStatus::Confirmed,
        "top-page mutation must not confirm a frame-scoped key combo"
    );
    sleep(Duration::from_millis(120)).await;
    assert_ne!(top_input_value(&page, "counter").await, "");
    assert_ne!(top_input_value(&page, "counter").await, "0");

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn type_text_in_frame_fails_closed_when_focus_is_stolen_before_dispatch() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let adapter = test_adapter_with_humanize(
        manager.clone(),
        HumanizeConfig {
            enabled: true,
            speed: HumanizeSpeed::Slow,
        },
    );
    let page = manager.page().await.expect("page authority");
    set_test_html(
        &page,
        r#"
        <input id="top" value="">
        <iframe></iframe>
        "#,
    )
    .await;
    seed_child_frame_document(&page, r#"<input id="child" value="">"#).await;

    let frame_id = child_frame_id(&page).await;
    wait_for_frame_context_resolved(&page, &frame_id).await;
    focus_child_input(&page, "child").await;
    arm_top_focus_theft(&page).await;

    let error = adapter
        .type_text_in_frame(Some(frame_id.as_str()), "abc")
        .await
        .expect_err("frame-scoped typing must fail closed if focus drifts before dispatch");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::ElementNotInteractable);
    assert_eq!(top_input_value(&page, "top").await, "");
    assert_eq!(child_input_value(&page, "child").await, "");

    manager.close().await.expect("browser should close cleanly");
}
