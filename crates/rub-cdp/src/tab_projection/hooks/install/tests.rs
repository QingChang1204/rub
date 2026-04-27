use super::{
    invalidate_page_hook_if_current_generation, page_hook_install_poll_delay,
    restore_existing_page_hook_installation_baseline, wait_for_required_page_hook_installation,
};
use crate::listener_generation::new_listener_generation_channel;
use crate::tab_projection::PageHookInstallState;
use crate::tab_projection::hooks::{
    BACKGROUND_DIALOG_HOOK_MASK, BACKGROUND_NETWORK_RULES_HOOK_MASK,
    BACKGROUND_OBSERVATORY_HOOK_MASK, PageHookFlag,
};
use rub_core::error::ErrorCode;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[test]
fn page_hook_install_poll_delay_uses_bounded_backoff() {
    assert_eq!(page_hook_install_poll_delay(0), Duration::from_millis(25));
    assert_eq!(page_hook_install_poll_delay(1), Duration::from_millis(50));
    assert_eq!(page_hook_install_poll_delay(2), Duration::from_millis(75));
    assert_eq!(page_hook_install_poll_delay(3), Duration::from_millis(100));
    assert_eq!(page_hook_install_poll_delay(9), Duration::from_millis(100));
}

#[tokio::test]
async fn stale_generation_restores_existing_page_hook_installation_baseline() {
    let page_hook_states = Mutex::new(HashMap::from([(
        "tab-1".to_string(),
        PageHookInstallState {
            installing: true,
            installation_recorded: true,
            hook_bits: 0b1111,
        },
    )]));

    restore_existing_page_hook_installation_baseline(
        "tab-1",
        PageHookInstallState {
            installing: false,
            installation_recorded: true,
            hook_bits: 0b0011,
        },
        &page_hook_states,
    )
    .await;

    let restored = page_hook_states
        .lock()
        .await
        .get("tab-1")
        .cloned()
        .expect("baseline should be restored");
    assert!(!restored.installing);
    assert_eq!(restored.hook_bits, 0b0011);
    assert!(restored.installation_recorded);
}

#[tokio::test]
async fn stale_generation_does_not_reinsert_pruned_page_hook_state() {
    let page_hook_states = Mutex::new(HashMap::new());

    restore_existing_page_hook_installation_baseline(
        "tab-1",
        PageHookInstallState {
            installing: false,
            installation_recorded: true,
            hook_bits: 0b0011,
        },
        &page_hook_states,
    )
    .await;

    assert!(
        page_hook_states.lock().await.get("tab-1").is_none(),
        "stale installers must not recreate hook state after a newer generation pruned it"
    );
}

#[tokio::test]
async fn listener_end_invalidates_committed_page_hook_for_current_generation() {
    let (generation_tx, generation_rx) = new_listener_generation_channel();
    let page_hook_states = Arc::new(Mutex::new(HashMap::from([(
        "tab-1".to_string(),
        PageHookInstallState::completed_runtime_callback_hooks_for_test(),
    )])));

    invalidate_page_hook_if_current_generation(
        page_hook_states.clone(),
        generation_rx,
        *generation_tx.borrow(),
        "tab-1".to_string(),
        PageHookFlag::Dialogs,
    )
    .await;

    let state = page_hook_states
        .lock()
        .await
        .get("tab-1")
        .cloned()
        .expect("state should remain present");
    assert!(
        !state.contains(PageHookFlag::Dialogs),
        "ended listener must clear the committed hook bit so sync can reinstall"
    );
}

#[tokio::test]
async fn stale_listener_end_does_not_invalidate_new_generation_hook_state() {
    let (generation_tx, generation_rx) = new_listener_generation_channel();
    let stale_generation = *generation_tx.borrow();
    generation_tx
        .send(1)
        .expect("generation update should succeed");
    let page_hook_states = Arc::new(Mutex::new(HashMap::from([(
        "tab-1".to_string(),
        PageHookInstallState::completed_runtime_callback_hooks_for_test(),
    )])));

    invalidate_page_hook_if_current_generation(
        page_hook_states.clone(),
        generation_rx,
        stale_generation,
        "tab-1".to_string(),
        PageHookFlag::Dialogs,
    )
    .await;

    let state = page_hook_states
        .lock()
        .await
        .get("tab-1")
        .cloned()
        .expect("state should remain present");
    assert!(
        state.contains(PageHookFlag::Dialogs),
        "stale listener end must not clear a newer generation hook bit"
    );
}

#[tokio::test]
async fn background_required_hook_wait_accepts_subset_commit_without_full_runtime_hooks() {
    let mut state = PageHookInstallState::default();
    state.mark(PageHookFlag::Dialogs);
    let page_hook_states = Mutex::new(HashMap::from([("tab-1".to_string(), state)]));

    wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect("background page should commit once the required dialog authority is installed");
}

#[tokio::test]
async fn background_required_hook_wait_accepts_dialog_and_observatory_subset_commit() {
    let mut state = PageHookInstallState::default();
    state.mark(PageHookFlag::Dialogs);
    state.mark(PageHookFlag::Observatory);
    let page_hook_states = Mutex::new(HashMap::from([("tab-1".to_string(), state)]));

    wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_OBSERVATORY_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect("background page should commit once required observatory authority is installed");
}

#[tokio::test]
async fn background_required_hook_wait_accepts_dialog_and_network_rule_subset_commit() {
    let mut state = PageHookInstallState::default();
    state.mark(PageHookFlag::Dialogs);
    state.mark(PageHookFlag::NetworkRules);
    let page_hook_states = Mutex::new(HashMap::from([("tab-1".to_string(), state)]));

    wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_NETWORK_RULES_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect("background page should commit once required network-rule authority is installed");
}

#[tokio::test]
async fn background_required_hook_wait_fails_closed_when_required_authority_is_missing() {
    let page_hook_states = Mutex::new(HashMap::from([(
        "tab-1".to_string(),
        PageHookInstallState::default(),
    )]));

    let error = wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect_err("missing required background hook must fail closed");

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserCrashed);
    let context = envelope
        .context
        .expect("background hook failure should explain why");
    assert_eq!(
        context.get("reason").and_then(serde_json::Value::as_str),
        Some("background_page_runtime_hooks_incomplete")
    );
}

#[tokio::test]
async fn background_required_hook_wait_fails_closed_when_observatory_authority_is_missing() {
    let mut state = PageHookInstallState::default();
    state.mark(PageHookFlag::Dialogs);
    let page_hook_states = Mutex::new(HashMap::from([("tab-1".to_string(), state)]));

    let error = wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_OBSERVATORY_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect_err("missing required observatory hook must fail closed");

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserCrashed);
    assert_eq!(
        envelope
            .context
            .and_then(|context| context.get("reason").cloned())
            .and_then(|value| value.as_str().map(str::to_owned))
            .as_deref(),
        Some("background_page_runtime_hooks_incomplete")
    );
}

#[tokio::test]
async fn background_required_hook_wait_fails_closed_when_network_rule_authority_is_missing() {
    let mut state = PageHookInstallState::default();
    state.mark(PageHookFlag::Dialogs);
    let page_hook_states = Mutex::new(HashMap::from([("tab-1".to_string(), state)]));

    let error = wait_for_required_page_hook_installation(
        "tab-1",
        BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_NETWORK_RULES_HOOK_MASK,
        &page_hook_states,
    )
    .await
    .expect_err("missing required network-rule hook must fail closed");

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserCrashed);
    assert_eq!(
        envelope
            .context
            .and_then(|context| context.get("reason").cloned())
            .and_then(|value| value.as_str().map(str::to_owned))
            .as_deref(),
        Some("background_page_runtime_hooks_incomplete")
    );
}
