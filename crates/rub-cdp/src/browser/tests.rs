use super::control::{
    pending_dialog_authority, pending_dialog_target_id, resolve_close_tab_index,
    stale_pending_dialog_target_error,
};
use super::*;
use rub_core::model::{
    ConnectionTarget, DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus, DownloadState,
    IdentityProbeStatus, IdentitySelfProbeInfo, NetworkRule, NetworkRuleEffect,
    NetworkRuleEffectKind, NetworkRuleSpec, NetworkRuleStatus, TabActiveAuthority,
};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::time::{Duration, timeout};

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

fn ephemeral_options() -> BrowserLaunchOptions {
    let mut options = options();
    options.managed_profile_ephemeral = true;
    options
}

#[test]
fn close_current_tab_requires_active_authority_when_multiple_tabs_exist() {
    let targets = vec!["tab-a".to_string(), "tab-b".to_string()];
    let envelope = resolve_close_tab_index(None, &targets, None)
        .expect_err("multi-tab close current should fail closed without active authority")
        .into_envelope();

    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context["reason"].as_str()),
        Some("active_tab_authority_missing")
    );
}

#[test]
fn close_current_tab_uses_single_tab_when_authority_is_unambiguous() {
    let targets = vec!["tab-a".to_string()];
    assert_eq!(
        resolve_close_tab_index(None, &targets, None).expect("single tab is current tab"),
        0
    );
}

#[tokio::test]
async fn close_release_failure_retires_listener_generation() {
    let manager = BrowserManager::new(options());
    let before = manager.current_listener_generation();
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![],
        current_page: None,
        continuity_page: None,
        active_target_id: Some(TargetId::from("stale-tab".to_string())),
        active_target_authority: None,
    };
    manager.force_current_authority_release_failure();

    manager
        .close()
        .await
        .expect_err("forced release failure should fail close");

    assert!(
        manager.current_listener_generation() > before,
        "fail-closed release must retire old listeners even when browser release fails"
    );
    assert!(manager.browser.lock().await.is_none());
    assert!(
        manager
            .tab_projection
            .lock()
            .await
            .active_target_id
            .is_none(),
        "failed close release must not leave stale tab authority projected as usable"
    );
}

#[tokio::test]
async fn elevate_release_failure_retires_listener_generation() {
    let manager = BrowserManager::new(options());
    let before = manager.current_listener_generation();
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![],
        current_page: None,
        continuity_page: None,
        active_target_id: Some(TargetId::from("stale-tab".to_string())),
        active_target_authority: None,
    };
    manager.force_current_authority_release_failure();

    manager
        .elevate_to_visible()
        .await
        .expect_err("forced release failure should fail elevation");

    assert!(
        manager.current_listener_generation() > before,
        "fail-closed release must retire old listeners even when elevation release fails"
    );
    assert!(manager.browser.lock().await.is_none());
    assert!(
        manager
            .tab_projection
            .lock()
            .await
            .active_target_id
            .is_none(),
        "failed elevation release must not leave stale tab authority projected as usable"
    );
}

#[tokio::test]
async fn launch_policy_info_projects_connection_target_and_self_probe() {
    let manager = BrowserManager::new(options());
    manager
        .identity_coverage
        .lock()
        .await
        .record_self_probe(IdentitySelfProbeInfo {
            page_main_world: Some(IdentityProbeStatus::Passed),
            iframe_context: Some(IdentityProbeStatus::Unknown),
            worker_context: Some(IdentityProbeStatus::Passed),
            ua_consistency: Some(IdentityProbeStatus::Passed),
            webgl_surface: Some(IdentityProbeStatus::Passed),
            canvas_surface: Some(IdentityProbeStatus::Passed),
            audio_surface: Some(IdentityProbeStatus::Passed),
            permissions_surface: Some(IdentityProbeStatus::Passed),
            viewport_surface: Some(IdentityProbeStatus::Passed),
            touch_surface: Some(IdentityProbeStatus::Passed),
            window_metrics_surface: Some(IdentityProbeStatus::Passed),
            unsupported_surfaces: vec!["service_worker".to_string()],
        });
    manager
        .set_connection_target(ConnectionTarget::CdpUrl {
            url: "http://127.0.0.1:9222".to_string(),
        })
        .await;

    let launch_policy = manager.launch_policy_info();
    let connection_target = launch_policy
        .connection_target
        .expect("connection target should be projected");
    match connection_target {
        ConnectionTarget::CdpUrl { url } => assert_eq!(url, "http://127.0.0.1:9222"),
        other => panic!("unexpected connection target projection: {other:?}"),
    }
    let self_probe = launch_policy
        .stealth_coverage
        .and_then(|coverage| coverage.self_probe)
        .expect("self probe should be projected");
    assert_eq!(
        self_probe.page_main_world,
        Some(IdentityProbeStatus::Passed)
    );
    assert_eq!(self_probe.worker_context, Some(IdentityProbeStatus::Passed));
    assert_eq!(self_probe.webgl_surface, Some(IdentityProbeStatus::Passed));
    assert_eq!(
        self_probe.permissions_surface,
        Some(IdentityProbeStatus::Passed)
    );
    assert_eq!(self_probe.unsupported_surfaces, vec!["service_worker"]);
}

#[tokio::test]
async fn launch_policy_info_uses_cached_projection_when_runtime_locks_are_busy() {
    let manager = BrowserManager::new(options());
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;

    let connection_target_guard = manager.connection_target.lock().await;
    let identity_coverage_guard = manager.identity_coverage.lock().await;

    let launch_policy = manager.launch_policy_info();
    assert!(matches!(
        launch_policy.connection_target,
        Some(ConnectionTarget::Managed)
    ));
    assert!(launch_policy.stealth_coverage.is_some());

    drop(identity_coverage_guard);
    drop(connection_target_guard);
}

#[tokio::test]
async fn managed_transport_loss_rebuild_prefers_reattach_before_relaunch() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;

    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("managed browser authority")
        .websocket_address()
        .clone();
    let previous_profile = manager
        .managed_profile_authority_for_test()
        .await
        .expect("managed profile authority");
    let previous_target_id = manager
        .projected_continuity_page()
        .await
        .expect("continuity page authority")
        .target_id()
        .as_ref()
        .to_string();

    let install = manager
        .resolve_browser_authority_install(
            Some(ConnectionTarget::Managed),
            Some(previous_ws_url.clone()),
        )
        .await
        .expect("transport-only managed rebuild should reattach to the existing browser");

    assert!(
        !install.is_external,
        "managed transport-loss rebuild must preserve managed authority ownership"
    );
    assert!(matches!(
        install.connection_target,
        Some(ConnectionTarget::Managed)
    ));
    assert_eq!(install.browser.websocket_address(), &previous_ws_url);
    assert_eq!(
        install
            .managed_profile
            .as_ref()
            .map(|profile| &profile.path),
        Some(&previous_profile.path)
    );
    assert_eq!(
        install.page.target_id().as_ref(),
        previous_target_id.as_str()
    );

    drop(install.page);
    drop(install.browser);
    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn page_reattaches_managed_browser_after_transport_only_stale_handle_loss() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;

    let live_browser = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("managed browser authority");
    let previous_ws_url = live_browser.websocket_address().clone();
    let previous_profile = manager
        .managed_profile_authority_for_test()
        .await
        .expect("managed profile authority");
    let previous_target_id = manager
        .projected_continuity_page()
        .await
        .expect("continuity page authority")
        .target_id()
        .as_ref()
        .to_string();

    let (stale_browser, stale_handler) = Browser::connect(&previous_ws_url)
        .await
        .expect("test should create a second CDP client handle");
    drop(stale_handler);
    *manager.browser.lock().await = Some(Arc::new(stale_browser));

    let page = manager
        .page()
        .await
        .expect("managed transport-only loss should reattach through the real stale-handle branch");

    let recovered_browser = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("recovered browser authority");
    assert_eq!(
        recovered_browser.websocket_address(),
        &previous_ws_url,
        "transport-only loss should reattach to the original managed browser authority instead of relaunching"
    );
    assert_eq!(
        manager
            .managed_profile_authority_for_test()
            .await
            .as_ref()
            .map(|profile| &profile.path),
        Some(&previous_profile.path),
        "managed transport-only recovery must preserve managed profile ownership"
    );
    assert_eq!(
        page.target_id().as_ref(),
        previous_target_id.as_str(),
        "transport-only recovery should preserve continuity page authority"
    );

    drop(recovered_browser);
    drop(live_browser);
    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn authority_install_blocks_public_page_reads_until_commit_fence_closes() {
    let manager = Arc::new(BrowserManager::new(options()));
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;
    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("previous browser authority")
        .websocket_address()
        .clone();
    let (replacement_browser, replacement_page, _) =
        crate::runtime::attach_external_browser(&previous_ws_url)
            .await
            .expect("replacement browser authority should attach");

    manager.pause_authority_commit_after_projection();
    let replace_manager = manager.clone();
    let replace_url = previous_ws_url.clone();
    let replace_task = tokio::spawn(async move {
        replace_manager
            .replace_browser_authority(
                replacement_browser,
                replacement_page,
                true,
                Some(ConnectionTarget::CdpUrl { url: replace_url }),
                None,
            )
            .await
    });

    manager.wait_for_authority_commit_projection_pause().await;

    assert!(
        timeout(Duration::from_millis(100), manager.page())
            .await
            .is_err(),
        "public page reads should wait behind the browser-authority commit fence"
    );

    let launch_policy = manager.launch_policy_info();
    assert!(
        matches!(
            launch_policy.connection_target,
            Some(ConnectionTarget::Managed)
        ),
        "synchronous launch-policy projection should continue to reflect committed authority while install is in progress"
    );

    manager.resume_paused_authority_commit();
    replace_task
        .await
        .expect("replace task should join")
        .expect("replacement authority should commit");

    assert!(matches!(
        manager.launch_policy_info().connection_target,
        Some(ConnectionTarget::CdpUrl { .. })
    ));

    manager.close().await.expect("browser should close cleanly");
}

#[test]
fn active_page_authority_error_projects_tab_authority_gap_without_claiming_crash() {
    let envelope = active_page_authority_error(2).into_envelope();
    assert_eq!(envelope.code, ErrorCode::TabNotFound);
    let context = envelope.context.expect("active page authority context");
    assert_eq!(context["reason"], "active_tab_authority_unavailable");
    assert_eq!(context["projected_page_count"], 2);
}

#[test]
fn active_page_authority_error_preserves_browser_crash_surface_when_no_pages_exist() {
    let envelope = active_page_authority_error(0).into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserCrashed);
}

#[tokio::test]
async fn ambiguity_can_fail_closed_for_public_active_page_without_clearing_continuity_authority() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let browser = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority");
    let continuity_page = manager
        .tab_projection
        .lock()
        .await
        .continuity_page
        .clone()
        .expect("startup continuity page");
    continuity_page
        .goto("data:text/html,continuity-authority")
        .await
        .expect("continuity page should navigate");
    let second_page = Arc::new(
        browser
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );

    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![continuity_page.clone(), second_page],
        current_page: None,
        continuity_page: Some(continuity_page.clone()),
        active_target_id: None,
        active_target_authority: None,
    };

    let active_error = manager
        .projected_active_page()
        .await
        .expect_err("public active page should fail closed under ambiguity");
    let active_envelope = active_error.into_envelope();
    assert_eq!(active_envelope.code, ErrorCode::TabNotFound);
    assert_eq!(
        active_envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("active_tab_authority_unavailable")
    );

    let snapshot = manager
        .snapshot_current_browser_authority()
        .await
        .expect("continuity authority should still be snapshotable");
    drop(snapshot);
    assert_eq!(
        manager
            .projected_continuity_page()
            .await
            .as_ref()
            .expect("continuity page should remain projected")
            .target_id()
            .as_ref(),
        continuity_page.target_id().as_ref()
    );
    assert_eq!(
        manager.current_restore_url().await,
        Some("data:text/html,continuity-authority".to_string()),
        "restore continuity should keep using the committed continuity page authority"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn browser_authority_snapshot_survives_missing_continuity_projection_under_ambiguity() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let browser = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority");
    let first_page = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("startup page authority");
    let first_target_id = first_page.target_id().as_ref().to_string();
    let second_page = Arc::new(
        browser
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );

    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![first_page, second_page],
        current_page: None,
        continuity_page: None,
        active_target_id: None,
        active_target_authority: None,
    };

    assert_eq!(
        manager
            .snapshot_current_browser_authority_target_id_for_test()
            .await
            .as_deref(),
        Some(first_target_id.as_str()),
        "snapshot fallback should keep capturing the intended predecessor authority when continuity/current/active truth is absent"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn page_relaunches_managed_browser_after_stale_browser_authority_crash() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority")
        .websocket_address()
        .clone();
    let profile_dir = manager
        .current_options()
        .user_data_dir
        .expect("managed browser profile path");
    let root_pids = rub_core::process::process_snapshot()
        .expect("process snapshot")
        .into_iter()
        .filter(|process| {
            rub_core::process::is_browser_root_process(&process.command)
                && rub_core::process::extract_flag_value(&process.command, "--user-data-dir")
                    .as_deref()
                    .map(std::path::Path::new)
                    == Some(profile_dir.as_path())
        })
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    assert!(
        !root_pids.is_empty(),
        "test must locate the managed browser root process"
    );
    for pid in root_pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    manager
        .page()
        .await
        .expect("public page acquisition should relaunch a crashed managed browser");

    let current_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("recovered browser authority")
        .websocket_address()
        .clone();
    assert_ne!(
        previous_ws_url, current_ws_url,
        "managed browser recovery should replace the stale browser authority"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn failed_relaunch_after_stale_browser_authority_preserves_degraded_runtime_truth() {
    let owner = BrowserManager::new(options());
    owner
        .ensure_browser()
        .await
        .expect("owner browser should launch");
    let owner_ws_url = owner
        .browser
        .lock()
        .await
        .clone()
        .expect("owner browser authority")
        .websocket_address()
        .clone();

    let manager = BrowserManager::new(options());
    manager
        .set_connection_target(ConnectionTarget::CdpUrl {
            url: owner_ws_url.clone(),
        })
        .await;
    manager
        .ensure_browser()
        .await
        .expect("external browser authority should attach");

    let previous_target_id = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("previous page authority")
        .target_id()
        .as_ref()
        .to_string();
    manager.request_correlation.lock().await.record(
        "req-rebuild-failed".to_string(),
        None,
        "GET",
        crate::request_correlation::RequestCorrelation {
            tab_target_id: Some(previous_target_id.clone()),
            original_url: "https://example.test/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: Vec::new(),
        },
    );
    let previous_pending_registry =
        crate::runtime_observatory::new_shared_pending_request_registry();
    manager.observatory_pending_registries.lock().await.insert(
        previous_target_id.clone(),
        previous_pending_registry.clone(),
    );
    manager
        .network_rule_runtime
        .write()
        .await
        .replace_rules(vec![NetworkRule {
            id: 5,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.test/*".to_string(),
            },
        }]);
    manager
        .network_rule_runtime
        .write()
        .await
        .mark_fetch_enabled("stale-target".to_string());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Pending before crash".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-crash".to_string()),
            tab_target_id: Some(previous_target_id.clone()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }
    manager.download_runtime.write().await.set_runtime(
        11,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads-rebuild-failed".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        11,
        "guid-rebuild-failed".to_string(),
        "https://example.test/report.csv".to_string(),
        "report.csv".to_string(),
        Some("frame-crash".to_string()),
    );
    manager.download_runtime.write().await.record_progress(
        11,
        "guid-rebuild-failed".to_string(),
        DownloadState::InProgress,
        64,
        Some(128),
        None,
    );
    let delivered_download_runtime = Arc::new(StdMutex::new(Vec::<DownloadRuntimeInfo>::new()));
    *manager.download_callbacks.lock().await = crate::downloads::DownloadCallbacks {
        on_runtime: {
            let delivered_download_runtime = delivered_download_runtime.clone();
            Some(Arc::new(move |update| {
                delivered_download_runtime
                    .lock()
                    .expect("download runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_started: None,
        on_progress: None,
    };

    owner
        .close()
        .await
        .expect("owner browser should close cleanly");
    let error = timeout(Duration::from_secs(20), manager.page())
        .await
        .expect("failed relaunch should complete under a bounded browser recovery budget")
        .expect_err("failed relaunch should surface browser launch failure");
    let error = error.into_envelope();
    assert!(
        matches!(
            error.code,
            ErrorCode::BrowserLaunchFailed | ErrorCode::CdpConnectionFailed
        ),
        "failed stale-authority rebuild should surface a truthful rebuild failure, got {:?}",
        error.code
    );
    assert!(
        manager.browser.lock().await.is_none(),
        "failed stale-authority rebuild must not leave a half-installed browser handle behind"
    );
    let dialog_runtime = manager.dialog_runtime().read().await.clone();
    assert_eq!(
        dialog_runtime
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending before crash")
    );
    assert_eq!(
        dialog_runtime.degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        dialog_runtime.status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    let direct_dialog_runtime = manager
        .dialog_runtime_snapshot()
        .await
        .expect("failed rebuild should still surface degraded dialog truth on direct read");
    assert_eq!(
        direct_dialog_runtime
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending before crash")
    );
    assert_eq!(
        direct_dialog_runtime.degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        direct_dialog_runtime.status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    let handle_error = manager
        .handle_dialog(false, None)
        .await
        .expect_err("dialog actuation should preserve rebuild failure instead of masking it as stale-target loss");
    assert!(
        matches!(
            handle_error.into_envelope().code,
            ErrorCode::BrowserLaunchFailed | ErrorCode::CdpConnectionFailed
        ),
        "failed rebuild should preserve authoritative rebuild failure on dialog actuation"
    );
    assert!(
        manager
            .request_correlation
            .lock()
            .await
            .take_for_request(
                "req-rebuild-failed",
                "https://example.test/api",
                "GET",
                None,
                Some(previous_target_id.as_str())
            )
            .is_some(),
        "failed stale-authority rebuild should preserve request correlation fallback truth"
    );
    assert_eq!(
        manager
            .request_correlation
            .lock()
            .await
            .take_degraded_reasons(),
        vec![crate::request_correlation::CORRELATION_BROWSER_AUTHORITY_REBUILD_FAILED_REASON]
    );
    let restored_pending_registry = manager
        .observatory_pending_registries
        .lock()
        .await
        .get(&previous_target_id)
        .cloned()
        .expect("failed stale-authority rebuild should preserve observatory pending registries");
    assert!(Arc::ptr_eq(
        &restored_pending_registry,
        &previous_pending_registry
    ));
    let network_runtime = manager.network_rule_runtime.read().await;
    assert_eq!(network_runtime.rules_snapshot().len(), 1);
    assert!(
        !network_runtime.is_fetch_enabled_for("stale-target"),
        "failed stale-authority rebuild should preserve rule truth but clear stale fetch-installation state"
    );
    drop(network_runtime);
    let download_runtime = manager.download_runtime.read().await.projection();
    assert_eq!(download_runtime.status, DownloadRuntimeStatus::Degraded);
    assert_eq!(
        download_runtime.degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        download_runtime
            .active_downloads
            .first()
            .map(|download| download.guid.as_str()),
        Some("guid-rebuild-failed")
    );
    assert_eq!(
        download_runtime
            .active_downloads
            .first()
            .map(|download| download.received_bytes),
        Some(64)
    );
    {
        let delivered_download_runtime = delivered_download_runtime
            .lock()
            .expect("download runtime delivery lock should remain healthy");
        assert_eq!(delivered_download_runtime.len(), 1);
        assert_eq!(
            delivered_download_runtime[0].status,
            DownloadRuntimeStatus::Degraded
        );
        assert_eq!(
            delivered_download_runtime[0]
                .active_downloads
                .first()
                .map(|download| download.guid.as_str()),
            Some("guid-rebuild-failed")
        );
        assert_eq!(
            delivered_download_runtime[0]
                .active_downloads
                .first()
                .map(|download| download.received_bytes),
            Some(64)
        );
    }

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn external_attach_accepts_multi_tab_browser_when_active_tab_truth_is_unique() {
    let owner = BrowserManager::new(options());
    owner
        .ensure_browser()
        .await
        .expect("owner browser should launch");
    let owner_browser = owner
        .browser
        .lock()
        .await
        .clone()
        .expect("owner browser authority");
    let owner_ws_url = owner_browser.websocket_address().clone();

    let second_page = Arc::new(
        owner_browser
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );
    second_page
        .activate()
        .await
        .expect("second page should become active");

    let manager = BrowserManager::new(options());
    manager
        .set_connection_target(ConnectionTarget::CdpUrl {
            url: owner_ws_url.clone(),
        })
        .await;
    manager
        .ensure_browser()
        .await
        .expect("external multi-tab browser should still attach");

    let page = manager
        .page()
        .await
        .expect("active external page should resolve");
    assert_eq!(
        page.target_id().as_ref(),
        second_page.target_id().as_ref(),
        "external attach should preserve browser-truth active-tab authority across multi-tab browsers"
    );

    let tabs = manager.tab_list().await.expect("tab list should project");
    let active_tab = tabs
        .iter()
        .find(|tab| tab.active)
        .expect("exactly one active tab should project");
    assert_eq!(
        active_tab.active_authority,
        Some(TabActiveAuthority::BrowserTruth)
    );

    manager.close().await.expect("browser should close cleanly");
    owner
        .close()
        .await
        .expect("owner browser should close cleanly");
}

#[tokio::test]
async fn failed_relaunch_after_stale_browser_authority_without_page_projection_preserves_degraded_runtime_truth()
 {
    let owner = BrowserManager::new(options());
    owner
        .ensure_browser()
        .await
        .expect("owner browser should launch");
    let owner_ws_url = owner
        .browser
        .lock()
        .await
        .clone()
        .expect("owner browser authority")
        .websocket_address()
        .clone();

    let manager = BrowserManager::new(options());
    manager
        .set_connection_target(ConnectionTarget::CdpUrl {
            url: owner_ws_url.clone(),
        })
        .await;
    manager
        .ensure_browser()
        .await
        .expect("external browser authority should attach");

    let previous_target_id = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("previous page authority")
        .target_id()
        .as_ref()
        .to_string();
    manager.request_correlation.lock().await.record(
        "req-rebuild-failed-no-page".to_string(),
        None,
        "GET",
        crate::request_correlation::RequestCorrelation {
            tab_target_id: Some(previous_target_id.clone()),
            original_url: "https://example.test/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: Vec::new(),
        },
    );
    let previous_pending_registry =
        crate::runtime_observatory::new_shared_pending_request_registry();
    manager.observatory_pending_registries.lock().await.insert(
        previous_target_id.clone(),
        previous_pending_registry.clone(),
    );
    manager
        .network_rule_runtime
        .write()
        .await
        .replace_rules(vec![NetworkRule {
            id: 6,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.test/*".to_string(),
            },
        }]);
    manager
        .network_rule_runtime
        .write()
        .await
        .mark_fetch_enabled("stale-target-no-page".to_string());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Pending before crash without page".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-crash".to_string()),
            tab_target_id: Some(previous_target_id.clone()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }
    *manager.tab_projection.lock().await = crate::tab_projection::CommittedTabProjection::empty();

    owner
        .close()
        .await
        .expect("owner browser should close cleanly");
    let error = timeout(Duration::from_secs(20), manager.page())
        .await
        .expect("failed relaunch should complete under a bounded browser recovery budget")
        .expect_err("failed relaunch should surface browser launch failure");
    let error = error.into_envelope();
    assert!(
        matches!(
            error.code,
            ErrorCode::BrowserLaunchFailed | ErrorCode::CdpConnectionFailed
        ),
        "failed stale-authority rebuild should surface a truthful rebuild failure, got {:?}",
        error.code
    );
    assert!(
        manager.browser.lock().await.is_none(),
        "failed stale-authority rebuild must not leave a half-installed browser handle behind"
    );
    let dialog_runtime = manager.dialog_runtime().read().await.clone();
    assert_eq!(
        dialog_runtime
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending before crash without page")
    );
    assert_eq!(
        dialog_runtime.degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        dialog_runtime.status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    let direct_dialog_runtime = manager.dialog_runtime_snapshot().await.expect(
        "failed rebuild without page projection should still surface degraded dialog truth",
    );
    assert_eq!(
        direct_dialog_runtime
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending before crash without page")
    );
    assert_eq!(
        direct_dialog_runtime.degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        direct_dialog_runtime.status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    assert!(
        manager
            .request_correlation
            .lock()
            .await
            .take_for_request(
                "req-rebuild-failed-no-page",
                "https://example.test/api",
                "GET",
                None,
                Some(previous_target_id.as_str())
            )
            .is_some(),
        "failed stale-authority rebuild should preserve request correlation fallback truth without a page projection"
    );
    assert_eq!(
        manager
            .request_correlation
            .lock()
            .await
            .take_degraded_reasons(),
        vec![crate::request_correlation::CORRELATION_BROWSER_AUTHORITY_REBUILD_FAILED_REASON]
    );
    let restored_pending_registry = manager
        .observatory_pending_registries
        .lock()
        .await
        .get(&previous_target_id)
        .cloned()
        .expect(
            "failed stale-authority rebuild should preserve observatory pending registries without a page projection",
        );
    assert!(Arc::ptr_eq(
        &restored_pending_registry,
        &previous_pending_registry
    ));
    let network_runtime = manager.network_rule_runtime.read().await;
    assert_eq!(network_runtime.rules_snapshot().len(), 1);
    assert!(
        !network_runtime.is_fetch_enabled_for("stale-target-no-page"),
        "failed stale-authority rebuild should preserve rule truth but clear stale fetch-installation state without a page projection"
    );
    drop(network_runtime);

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn close_releases_ephemeral_managed_profile_authority() {
    let manager = BrowserManager::new(ephemeral_options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let profile_dir = manager
        .managed_profile_authority_for_test()
        .await
        .expect("managed profile authority should exist")
        .path;
    assert!(
        profile_dir.exists(),
        "ephemeral managed profile should exist while browser authority is live"
    );

    manager.close().await.expect("browser should close cleanly");

    assert!(
        !profile_dir.exists(),
        "closing the managed browser must release ephemeral profile authority"
    );
}

#[tokio::test]
async fn managed_browser_tests_serialize_managed_launch_authority() {
    let manager_one = BrowserManager::new(options());
    assert!(
        !manager_one
            .holds_managed_browser_test_permit_for_test()
            .await,
        "manager should not hold test-only managed-browser authority before launch"
    );

    manager_one
        .ensure_browser()
        .await
        .expect("first managed browser should launch");

    assert!(
        manager_one
            .holds_managed_browser_test_permit_for_test()
            .await,
        "manager should hold test-only managed-browser authority while a managed browser is live"
    );

    manager_one
        .close()
        .await
        .expect("first managed browser should close cleanly");

    assert!(
        !manager_one
            .holds_managed_browser_test_permit_for_test()
            .await,
        "manager should release test-only managed-browser authority after close"
    );
}

#[tokio::test]
async fn close_tab_at_last_tab_reuses_projection_without_relocking_launch_guard() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let page = manager
        .page()
        .await
        .expect("startup page should be available");
    page.goto("data:text/html,last-tab-close")
        .await
        .expect("page should navigate before close-tab");

    let tabs = timeout(Duration::from_secs(2), manager.close_tab_at(None))
        .await
        .expect("close_tab_at should not deadlock behind launch_lock")
        .expect("close_tab_at should succeed");

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].url, "about:blank");

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn committed_tab_projection_snapshot_preserves_consistent_active_page_authority() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .sync_tabs_projection()
        .await
        .expect("tab projection should sync");

    let projection = manager.tab_projection.lock().await.clone();
    let active_target_id = projection
        .active_target_id
        .as_ref()
        .expect("startup projection should publish one active target");
    let current_page = projection
        .current_page
        .as_ref()
        .expect("startup projection should publish one active page");
    let continuity_page = projection
        .continuity_page
        .as_ref()
        .expect("startup projection should publish one continuity page");
    assert_eq!(current_page.target_id(), active_target_id);
    assert_eq!(continuity_page.target_id(), active_target_id);
    assert!(
        projection
            .pages
            .iter()
            .any(|page| page.target_id() == active_target_id),
        "committed tab projection must not expose an active target that is absent from the same committed page set"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn failed_required_hook_install_keeps_last_good_tab_projection_uncommitted() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .sync_tabs_projection()
        .await
        .expect("initial tab projection should sync");
    let last_good_projection = manager.tab_projection.lock().await.clone();
    let last_good_active_target = last_good_projection
        .active_target_id
        .as_ref()
        .expect("initial projection should publish an active target")
        .clone();
    let last_good_page_count = last_good_projection.pages.len();

    manager.set_force_required_page_hook_install_failure(true);
    manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority")
        .new_page("about:blank")
        .await
        .expect("second page should open");
    let error = manager
        .sync_tabs_projection()
        .await
        .expect_err("required hook failure should fail the sync");
    assert!(
        error
            .to_string()
            .contains("forced required page hook install failure"),
        "sync should surface the required hook fence failure"
    );
    let projection_after_failure = manager.tab_projection.lock().await.clone();
    assert_eq!(
        projection_after_failure.active_target_id.as_ref(),
        Some(&last_good_active_target),
        "active-tab authority must stay on the last committed projection when required hooks fail"
    );
    assert_eq!(
        projection_after_failure.pages.len(),
        last_good_page_count,
        "failed hook fence must not publish the staged page set"
    );

    manager.set_force_required_page_hook_install_failure(false);
    manager
        .sync_tabs_projection()
        .await
        .expect("projection should recover once the hook fence succeeds");
    assert!(
        manager.tab_projection.lock().await.pages.len() > last_good_page_count,
        "projection should publish the newer page set after hook installation succeeds"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn page_for_target_id_does_not_depend_on_active_tab_hook_reconciliation() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .sync_tabs_projection()
        .await
        .expect("initial tab projection should sync");
    let initial_target_id = manager
        .tab_projection
        .lock()
        .await
        .active_target_id
        .as_ref()
        .expect("initial active target")
        .as_ref()
        .to_string();

    manager.set_force_required_page_hook_install_failure(true);
    let second_page = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority")
        .new_page("about:blank")
        .await
        .expect("second page should open");
    let second_target_id = second_page.target_id().as_ref().to_string();

    let initial_page = manager
        .page_for_target_id(&initial_target_id)
        .await
        .expect("stable target lookup should not depend on active-tab hook reconciliation");
    assert_eq!(initial_page.target_id().as_ref(), initial_target_id);
    let looked_up_second_page = manager.page_for_target_id(&second_target_id).await.expect(
        "background target lookup should stay available even while sync_tabs_projection would fail",
    );
    assert_eq!(looked_up_second_page.target_id().as_ref(), second_target_id);

    manager.set_force_required_page_hook_install_failure(false);
    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn close_preserves_network_rule_ssot_without_browser_handle() {
    let manager = BrowserManager::new(options());
    *manager.is_external.lock().await = true;
    *manager.connection_target.lock().await = Some(ConnectionTarget::CdpUrl {
        url: "http://127.0.0.1:9222".to_string(),
    });
    manager
        .network_rule_runtime
        .write()
        .await
        .replace_rules(vec![NetworkRule {
            id: 1,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.com/*".to_string(),
            },
        }]);
    manager.request_correlation.lock().await.record(
        "req-1".to_string(),
        None,
        "GET",
        crate::request_correlation::RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 1,
                kind: NetworkRuleEffectKind::Block,
            }],
        },
    );

    manager.close().await.expect("close should succeed");

    assert!(manager.is_external().await);
    assert!(matches!(
        manager.connection_target.lock().await.clone(),
        Some(ConnectionTarget::CdpUrl { .. })
    ));
    let rules = manager.network_rule_runtime.read().await.rules_snapshot();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, 1);
    assert!(
        manager
            .request_correlation
            .lock()
            .await
            .take_for_request(
                "req-1",
                "https://example.com/api",
                "GET",
                None,
                Some("tab-1")
            )
            .is_none()
    );
}

#[tokio::test]
async fn managed_close_preserves_managed_connection_target_without_browser_handle() {
    let manager = BrowserManager::new(options());
    *manager.connection_target.lock().await = Some(ConnectionTarget::Managed);

    manager.close().await.expect("close should succeed");

    assert!(!manager.is_external().await);
    assert!(matches!(
        manager.connection_target.lock().await.clone(),
        Some(ConnectionTarget::Managed)
    ));
}

#[test]
fn launch_policy_info_reflects_current_headless_override() {
    let manager = BrowserManager::new(options());
    assert!(manager.launch_policy_info().headless);

    manager.set_current_headless(false);

    let launch_policy = manager.launch_policy_info();
    assert!(!launch_policy.headless);
    assert_eq!(launch_policy.stealth_level.as_deref(), Some("L1"));
}

#[tokio::test]
async fn elevate_to_visible_rejects_external_sessions() {
    let manager = BrowserManager::new(options());
    *manager.is_external.lock().await = true;

    let error = manager
        .elevate_to_visible()
        .await
        .expect_err("external sessions should reject local elevation");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn runtime_state_callback_setter_surfaces_reconcile_failures() {
    let manager = BrowserManager::new(options());
    *manager.runtime_state_callbacks.lock().await = crate::runtime_state::RuntimeStateCallbacks {
        allocate_sequence: Some(std::sync::Arc::new(|| 7)),
        on_snapshot: Some(std::sync::Arc::new(|_, _, _, _| {})),
    };
    manager.force_runtime_callback_reconcile_failure();

    let error = manager
        .set_runtime_state_callbacks(crate::runtime_state::RuntimeStateCallbacks::default())
        .await
        .expect_err("runtime-state callback setter should surface reconcile failure");
    assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
    assert!(!manager.runtime_state_callbacks.lock().await.is_empty());
}

#[tokio::test]
async fn runtime_state_callback_reconfigure_replays_after_commit_fence_clears() {
    let manager = BrowserManager::new(options());

    manager
        .set_runtime_state_callbacks(crate::runtime_state::RuntimeStateCallbacks {
            allocate_sequence: Some(Arc::new(|| 1)),
            on_snapshot: Some(Arc::new(|_, _, _, _| {})),
        })
        .await
        .expect("runtime-state callback reconfigure should succeed without a live browser");

    assert!(
        !manager.runtime_callback_reconfigure_in_progress(),
        "runtime-state replay must happen after the callback reconfigure fence clears"
    );
    assert_eq!(
        manager
            .runtime_state_replay_attempt_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "runtime-state callback install must schedule a post-fence active-page replay"
    );
}

#[test]
fn runtime_state_callback_publish_requires_same_generation_and_clear_fence() {
    let manager = BrowserManager::new(options());
    let generation = manager.current_listener_generation();
    assert!(manager.runtime_state_callback_publish_allowed(generation));

    manager.set_runtime_callback_reconfigure_in_progress(true);
    assert!(!manager.runtime_state_callback_publish_allowed(generation));
    manager.set_runtime_callback_reconfigure_in_progress(false);

    manager.set_authority_commit_in_progress(true);
    assert!(!manager.runtime_state_callback_publish_allowed(generation));
    manager.set_authority_commit_in_progress(false);

    manager.bump_listener_generation();
    assert!(!manager.runtime_state_callback_publish_allowed(generation));
}

#[tokio::test]
async fn runtime_state_callback_publish_requires_same_active_target() {
    let manager = BrowserManager::new(options());
    let generation = manager.current_listener_generation();
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![],
        current_page: None,
        continuity_page: None,
        active_target_id: Some(TargetId::from("active-tab".to_string())),
        active_target_authority: None,
    };

    assert!(
        manager
            .runtime_state_callback_publish_allowed_for_active_target(
                generation,
                Some("active-tab")
            )
            .await
    );
    assert!(
        !manager
            .runtime_state_callback_publish_allowed_for_active_target(generation, Some("stale-tab"))
            .await
    );
}

#[tokio::test]
async fn runtime_state_callback_publish_holds_active_target_fence_until_publish_commits() {
    let manager = Arc::new(BrowserManager::new(options()));
    let generation = manager.current_listener_generation();
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![],
        current_page: None,
        continuity_page: None,
        active_target_id: Some(TargetId::from("active-tab".to_string())),
        active_target_authority: None,
    };

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let manager_for_publish = manager.clone();
    let publish = tokio::spawn(async move {
        manager_for_publish
            .publish_runtime_state_callback_if_active_target(
                generation,
                Some("active-tab"),
                || async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    true
                },
            )
            .await
    });
    entered_rx.await.expect("publish future should start");

    assert!(
        timeout(Duration::from_millis(25), manager.tab_projection.lock())
            .await
            .is_err(),
        "active-target projection lock must remain held until the runtime-state publish commits"
    );
    let _ = release_tx.send(());
    assert!(publish.await.expect("publish task should join"));
}

#[tokio::test]
async fn dialog_and_download_callback_setters_surface_reconcile_failures() {
    let manager = BrowserManager::new(options());
    manager.force_runtime_callback_reconcile_failure();
    let dialog_error = manager
        .set_dialog_callbacks(crate::dialogs::DialogCallbacks::default())
        .await
        .expect_err("dialog callback setter should surface reconcile failure");
    assert_eq!(dialog_error.into_envelope().code, ErrorCode::InternalError);

    manager.force_runtime_callback_reconcile_failure();
    let download_error = manager
        .set_download_callbacks(crate::downloads::DownloadCallbacks::default())
        .await
        .expect_err("download callback setter should surface reconcile failure");
    assert_eq!(
        download_error.into_envelope().code,
        ErrorCode::InternalError
    );
}

#[tokio::test]
async fn dialog_callback_reconfigure_holds_success_path_commit_fence_until_reconcile_finishes() {
    let manager = BrowserManager::new(options());
    manager.page_hook_states.lock().await.insert(
        "target-1".to_string(),
        crate::tab_projection::PageHookInstallState::completed_runtime_callback_hooks_for_test(),
    );
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: Some(Arc::new(|_| {})),
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };
    manager.force_pause_runtime_callback_reconfigure_before_reconcile();

    let reconfigure = async {
        manager
            .set_dialog_callbacks(crate::dialogs::DialogCallbacks::default())
            .await
    };
    let observe_fence = async {
        manager.wait_for_runtime_callback_reconfigure_pause().await;
        assert!(
            manager.runtime_callback_reconfigure_in_progress(),
            "success-path callback reconfigure must publish an explicit commit fence while hook bits are invalidated"
        );
        let hook_states = manager.page_hook_states.lock().await;
        let state = hook_states.get("target-1").expect("hook state");
        assert!(
            state.runtime_callback_hooks_cleared_for_test(),
            "success-path reconfigure should still clear runtime callback hook bits before reconcile"
        );
        drop(hook_states);
        manager.resume_runtime_callback_reconfigure_for_test();
    };

    let (result, ()) = tokio::join!(reconfigure, observe_fence);
    result.expect("dialog callback reconcile should succeed after resume");
    assert!(
        !manager.runtime_callback_reconfigure_in_progress(),
        "runtime callback reconfigure fence should clear after reconcile commits"
    );
}

#[tokio::test]
async fn non_dialog_callback_reconfigure_replays_failed_rebuild_runtime_fallbacks_after_fence_clears()
 {
    let manager = BrowserManager::new(options());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Pending fallback dialog".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-fallback".to_string()),
            tab_target_id: Some("tab-fallback".to_string()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }
    manager.download_runtime.write().await.set_runtime(
        13,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads-fallback".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        13,
        "guid-fallback-replay".to_string(),
        "https://example.test/report.csv".to_string(),
        "report.csv".to_string(),
        Some("frame-fallback".to_string()),
    );
    manager.download_runtime.write().await.record_progress(
        13,
        "guid-fallback-replay".to_string(),
        DownloadState::InProgress,
        96,
        Some(128),
        None,
    );
    let fallback = manager.snapshot_browser_runtime_fallback().await;

    let delivered_dialog = Arc::new(StdMutex::new(
        Vec::<rub_core::model::DialogRuntimeInfo>::new(),
    ));
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: {
            let delivered_dialog = delivered_dialog.clone();
            Some(Arc::new(move |update| {
                delivered_dialog
                    .lock()
                    .expect("dialog runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };
    let delivered_download = Arc::new(StdMutex::new(Vec::<DownloadRuntimeInfo>::new()));
    *manager.download_callbacks.lock().await = crate::downloads::DownloadCallbacks {
        on_runtime: {
            let delivered_download = delivered_download.clone();
            Some(Arc::new(move |update| {
                delivered_download
                    .lock()
                    .expect("download runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_started: None,
        on_progress: None,
    };

    manager.force_pause_runtime_callback_reconfigure_before_reconcile();
    let reconfigure = async {
        manager
            .set_runtime_state_callbacks(crate::runtime_state::RuntimeStateCallbacks::default())
            .await
    };
    let observe_fence = async {
        manager.wait_for_runtime_callback_reconfigure_pause().await;
        manager
            .restore_degraded_runtime_fallback_after_failed_authority_rebuild(fallback)
            .await;
        assert!(
            delivered_dialog
                .lock()
                .expect("dialog runtime delivery lock should remain healthy")
                .is_empty(),
            "dialog fallback replay should stay suppressed while a non-dialog callback reconfigure fence is active"
        );
        assert!(
            delivered_download
                .lock()
                .expect("download runtime delivery lock should remain healthy")
                .is_empty(),
            "download fallback replay should stay suppressed while a non-download callback reconfigure fence is active"
        );
        manager.resume_runtime_callback_reconfigure_for_test();
    };

    let (result, ()) = tokio::join!(reconfigure, observe_fence);
    result.expect("runtime-state callback reconfigure should succeed after resume");

    let delivered_dialog = delivered_dialog
        .lock()
        .expect("dialog runtime delivery lock should remain healthy");
    assert_eq!(delivered_dialog.len(), 1);
    assert_eq!(
        delivered_dialog[0]
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending fallback dialog")
    );
    assert_eq!(
        delivered_dialog[0].degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        delivered_dialog[0].status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    drop(delivered_dialog);

    let delivered_download = delivered_download
        .lock()
        .expect("download runtime delivery lock should remain healthy");
    assert_eq!(delivered_download.len(), 1);
    assert_eq!(
        delivered_download[0].status,
        DownloadRuntimeStatus::Degraded
    );
    assert_eq!(
        delivered_download[0].degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        delivered_download[0]
            .active_downloads
            .first()
            .map(|download| download.guid.as_str()),
        Some("guid-fallback-replay")
    );
    assert_eq!(
        delivered_download[0]
            .active_downloads
            .first()
            .map(|download| download.received_bytes),
        Some(96)
    );
}

#[tokio::test]
async fn failed_non_dialog_callback_reconfigure_replays_failed_rebuild_runtime_fallbacks_after_rollback()
 {
    let manager = BrowserManager::new(options());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Pending fallback dialog after rollback".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-rollback".to_string()),
            tab_target_id: Some("tab-rollback".to_string()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }
    manager.download_runtime.write().await.set_runtime(
        17,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads-rollback".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        17,
        "guid-fallback-rollback".to_string(),
        "https://example.test/report.csv".to_string(),
        "report.csv".to_string(),
        Some("frame-rollback".to_string()),
    );
    manager.download_runtime.write().await.record_progress(
        17,
        "guid-fallback-rollback".to_string(),
        DownloadState::InProgress,
        88,
        Some(128),
        None,
    );
    let fallback = manager.snapshot_browser_runtime_fallback().await;

    let delivered_dialog = Arc::new(StdMutex::new(
        Vec::<rub_core::model::DialogRuntimeInfo>::new(),
    ));
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: {
            let delivered_dialog = delivered_dialog.clone();
            Some(Arc::new(move |update| {
                delivered_dialog
                    .lock()
                    .expect("dialog runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };
    let delivered_download = Arc::new(StdMutex::new(Vec::<DownloadRuntimeInfo>::new()));
    *manager.download_callbacks.lock().await = crate::downloads::DownloadCallbacks {
        on_runtime: {
            let delivered_download = delivered_download.clone();
            Some(Arc::new(move |update| {
                delivered_download
                    .lock()
                    .expect("download runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_started: None,
        on_progress: None,
    };

    manager.force_pause_runtime_callback_reconfigure_before_reconcile();
    manager.force_runtime_callback_reconcile_failure();
    let reconfigure = async {
        manager
            .set_runtime_state_callbacks(crate::runtime_state::RuntimeStateCallbacks::default())
            .await
    };
    let observe_fence = async {
        manager.wait_for_runtime_callback_reconfigure_pause().await;
        manager
            .restore_degraded_runtime_fallback_after_failed_authority_rebuild(fallback)
            .await;
        assert!(
            delivered_dialog
                .lock()
                .expect("dialog runtime delivery lock should remain healthy")
                .is_empty(),
            "dialog fallback replay should stay suppressed while rollback-triggering reconfigure fence is active"
        );
        assert!(
            delivered_download
                .lock()
                .expect("download runtime delivery lock should remain healthy")
                .is_empty(),
            "download fallback replay should stay suppressed while rollback-triggering reconfigure fence is active"
        );
        manager.resume_runtime_callback_reconfigure_for_test();
    };

    let (result, ()) = tokio::join!(reconfigure, observe_fence);
    let error = result.expect_err(
        "runtime-state callback reconfigure should surface reconcile failure after resume",
    );
    assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
    assert!(
        !manager.runtime_callback_reconfigure_in_progress(),
        "rollback should still clear the runtime callback reconfigure fence"
    );

    let delivered_dialog = delivered_dialog
        .lock()
        .expect("dialog runtime delivery lock should remain healthy");
    assert_eq!(delivered_dialog.len(), 1);
    assert_eq!(
        delivered_dialog[0]
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("Pending fallback dialog after rollback")
    );
    assert_eq!(
        delivered_dialog[0].degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        delivered_dialog[0].status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );
    drop(delivered_dialog);

    let delivered_download = delivered_download
        .lock()
        .expect("download runtime delivery lock should remain healthy");
    assert_eq!(delivered_download.len(), 1);
    assert_eq!(
        delivered_download[0].status,
        DownloadRuntimeStatus::Degraded
    );
    assert_eq!(
        delivered_download[0].degraded_reason.as_deref(),
        Some("browser_authority_rebuild_failed")
    );
    assert_eq!(
        delivered_download[0]
            .active_downloads
            .first()
            .map(|download| download.guid.as_str()),
        Some("guid-fallback-rollback")
    );
    assert_eq!(
        delivered_download[0]
            .active_downloads
            .first()
            .map(|download| download.received_bytes),
        Some(88)
    );
}

#[tokio::test]
async fn handle_dialog_fails_closed_without_pending_dialog() {
    let manager = BrowserManager::new(options());

    let error = manager
        .handle_dialog(true, None)
        .await
        .expect_err("dialog handling should fail closed without pending dialog authority");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert_eq!(
        envelope.context.expect("context")["reason"],
        serde_json::json!("pending_dialog_missing")
    );
}

#[tokio::test]
async fn handle_dialog_fails_closed_when_pending_dialog_target_is_missing() {
    let manager = BrowserManager::new(options());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Background dialog".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-1".to_string()),
            tab_target_id: None,
        });
    }

    let error = manager
        .handle_dialog(false, None)
        .await
        .expect_err("dialog handling should reject missing tab authority");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    let context = envelope.context.expect("context");
    assert_eq!(
        context["reason"],
        serde_json::json!("pending_dialog_target_missing")
    );
    assert_eq!(
        context["dialog_url"],
        serde_json::json!("https://example.test/dialog")
    );
}

#[tokio::test]
async fn handle_dialog_fails_closed_when_pending_dialog_target_is_stale() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("browser should launch for stale dialog target coverage");
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Background dialog".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-1".to_string()),
            tab_target_id: Some("tab-stale".to_string()),
        });
    }

    let error = manager
        .handle_dialog(false, None)
        .await
        .expect_err("dialog handling should reject stale tab authority");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    let context = envelope.context.expect("context");
    assert_eq!(context["pending_dialog_target_id"], "tab-stale");
    assert!(
        context["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("tab-stale")),
        "stale target context should preserve the missing tab authority"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn dialog_runtime_snapshot_clears_stale_pending_dialog_after_tab_loss() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("browser should launch for dialog runtime snapshot coverage");
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Detached dialog".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-1".to_string()),
            tab_target_id: Some("tab-stale".to_string()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }

    let runtime = manager
        .dialog_runtime_snapshot()
        .await
        .expect("dialog runtime snapshot should resync tab authority before publish");
    assert!(runtime.pending_dialog.is_none());
    assert_eq!(
        runtime.degraded_reason.as_deref(),
        Some("pending_dialog_target_lost")
    );
    assert_eq!(
        runtime.status,
        rub_core::model::DialogRuntimeStatus::Degraded
    );

    manager.close().await.expect("browser should close cleanly");
}

#[test]
fn pending_dialog_target_error_maps_stale_target_loss_fail_closed() {
    let error = stale_pending_dialog_target_error(
        "tab-stale",
        &RubError::domain(
            ErrorCode::TabNotFound,
            "Tab target 'tab-stale' is not present in the current session",
        ),
    )
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let context = error.context.expect("context");
    assert_eq!(context["pending_dialog_target_id"], "tab-stale");
    assert!(
        context["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("tab-stale"))
    );
}

#[test]
fn pending_dialog_authority_helpers_fail_closed_without_target() {
    let pending = pending_dialog_authority(Some(rub_core::model::PendingDialogInfo {
        kind: rub_core::model::DialogKind::Alert,
        message: "Alert".to_string(),
        url: "https://example.test/dialog".to_string(),
        tab_target_id: None,
        frame_id: Some("frame-1".to_string()),
        default_prompt: None,
        has_browser_handler: false,
        opened_at: "2026-04-15T00:00:00Z".to_string(),
    }))
    .expect("pending dialog should be accepted");
    let error = pending_dialog_target_id(&pending)
        .expect_err("missing dialog target must fail closed")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(
        error.context.expect("context")["reason"],
        serde_json::json!("pending_dialog_target_missing")
    );
}

#[test]
fn dialog_intercept_mutation_surfaces_poisoned_lock() {
    let manager = BrowserManager::new(options());
    let poisoned = manager.dialog_intercept.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.lock().expect("dialog intercept lock");
        panic!("poison dialog intercept lock");
    });

    let set_error = manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: None,
            target_tab_id: Some("tab-1".to_string()),
        })
        .expect_err("poisoned dialog intercept should fail closed");
    assert_eq!(set_error.into_envelope().code, ErrorCode::InternalError);

    let clear_error = manager
        .clear_dialog_intercept()
        .expect_err("poisoned dialog intercept clear should fail closed");
    assert_eq!(clear_error.into_envelope().code, ErrorCode::InternalError);
}

#[tokio::test]
async fn authority_reset_recovers_poisoned_dialog_intercept_state() {
    let manager = BrowserManager::new(options());
    let poisoned = manager.dialog_intercept.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.lock().expect("dialog intercept lock");
        panic!("poison dialog intercept lock");
    });

    manager.clear_local_browser_authority().await;

    manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: None,
            target_tab_id: Some("tab-1".to_string()),
        })
        .expect("authority reset should recover dialog intercept state");
    manager
        .clear_dialog_intercept()
        .expect("recovered dialog intercept state should clear cleanly");
}

#[tokio::test]
async fn wildcard_dialog_intercept_binds_single_tab_authority() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let expected_target_id = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .as_ref()
        .expect("startup page authority")
        .target_id()
        .as_ref()
        .to_string();

    manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: None,
            target_tab_id: None,
        })
        .expect("single-tab wildcard should bind to the authoritative tab");

    assert_eq!(
        manager
            .dialog_intercept
            .lock()
            .expect("dialog intercept lock should remain healthy")
            .as_ref()
            .and_then(|policy| policy.target_tab_id.clone())
            .as_deref(),
        Some(expected_target_id.as_str()),
        "owner layer should bind wildcard dialog intercept to the committed single-tab authority"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn wildcard_dialog_intercept_fails_closed_when_tab_authority_is_ambiguous() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");

    let browser = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("browser authority");
    let first_page = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("startup page authority");
    let second_page = Arc::new(
        browser
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![first_page, second_page],
        current_page: None,
        continuity_page: None,
        active_target_id: None,
        active_target_authority: None,
    };

    let error = manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: None,
            target_tab_id: None,
        })
        .expect_err("multi-tab wildcard should fail closed");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert_eq!(
        envelope.context.expect("wildcard failure context")["reason"],
        serde_json::json!("dialog_intercept_target_tab_authority_ambiguous")
    );
    assert!(
        manager
            .dialog_intercept
            .lock()
            .expect("dialog intercept lock should remain healthy")
            .is_none(),
        "failed wildcard bind must not publish an intercept policy"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[test]
fn wildcard_dialog_intercept_fails_closed_while_authority_commit_is_in_progress() {
    let manager = BrowserManager::new(options());
    manager.set_authority_commit_in_progress(true);

    let error = manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: None,
            target_tab_id: None,
        })
        .expect_err("wildcard dialog intercept should fail closed during authority commit");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert_eq!(
        envelope.context.expect("wildcard failure context")["reason"],
        serde_json::json!("dialog_intercept_target_tab_authority_commit_in_progress")
    );
    assert!(
        manager
            .dialog_intercept
            .lock()
            .expect("dialog intercept lock should remain healthy")
            .is_none(),
        "failed wildcard bind during authority commit must not publish an intercept policy"
    );
}

#[tokio::test]
async fn download_callback_reconfigure_rolls_back_generation_bound_page_hooks() {
    let manager = BrowserManager::new(options());
    manager.page_hook_states.lock().await.insert(
        "target-1".to_string(),
        crate::tab_projection::PageHookInstallState::completed_runtime_callback_hooks_for_test(),
    );
    manager.force_runtime_callback_reconcile_failure();

    let error = manager
        .set_download_callbacks(crate::downloads::DownloadCallbacks::default())
        .await
        .expect_err("download callback reconcile should surface failure");
    assert_eq!(error.into_envelope().code, ErrorCode::InternalError);

    let hook_states = manager.page_hook_states.lock().await;
    let state = hook_states.get("target-1").expect("hook state");
    assert!(!state.runtime_callback_hooks_cleared_for_test());
}

#[tokio::test]
async fn download_callback_reconfigure_replays_current_runtime_projection() {
    let manager = BrowserManager::new(options());
    manager.download_runtime.write().await.set_runtime(
        7,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        7,
        "guid-replay".to_string(),
        "https://example.test/report.csv".to_string(),
        "report.csv".to_string(),
        Some("frame-main".to_string()),
    );
    manager.download_runtime.write().await.record_progress(
        7,
        "guid-replay".to_string(),
        DownloadState::InProgress,
        64,
        Some(128),
        None,
    );

    let delivered = Arc::new(StdMutex::new(Vec::<DownloadRuntimeInfo>::new()));
    manager
        .set_download_callbacks(crate::downloads::DownloadCallbacks {
            on_runtime: {
                let delivered = delivered.clone();
                Some(Arc::new(move |update| {
                    delivered
                        .lock()
                        .expect("runtime delivery lock should remain healthy")
                        .push(update.runtime);
                }))
            },
            on_started: None,
            on_progress: None,
        })
        .await
        .expect("download callback reconfigure should succeed without a live browser");

    let delivered = delivered
        .lock()
        .expect("runtime delivery lock should remain healthy");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].status, DownloadRuntimeStatus::Active);
    assert_eq!(delivered[0].active_downloads.len(), 1);
    assert_eq!(delivered[0].active_downloads[0].guid, "guid-replay");
    assert_eq!(delivered[0].active_downloads[0].received_bytes, 64);
}

#[tokio::test]
async fn download_runtime_replay_waits_for_authority_commit_fence_to_clear() {
    let manager = BrowserManager::new(options());
    manager.download_runtime.write().await.set_runtime(
        9,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        9,
        "guid-authority-fence".to_string(),
        "https://example.test/file.csv".to_string(),
        "file.csv".to_string(),
        None,
    );
    manager.download_runtime.write().await.record_progress(
        9,
        "guid-authority-fence".to_string(),
        DownloadState::Completed,
        512,
        Some(512),
        Some("/tmp/rub-downloads/guid-authority-fence".to_string()),
    );

    let delivered = Arc::new(StdMutex::new(Vec::<DownloadRuntimeInfo>::new()));
    *manager.download_callbacks.lock().await = crate::downloads::DownloadCallbacks {
        on_runtime: {
            let delivered = delivered.clone();
            Some(Arc::new(move |update| {
                delivered
                    .lock()
                    .expect("runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_started: None,
        on_progress: None,
    };

    manager
        .authority_commit_in_progress
        .store(true, std::sync::atomic::Ordering::SeqCst);
    manager
        .replay_download_runtime_projection_to_callbacks()
        .await;
    assert!(
        delivered
            .lock()
            .expect("runtime delivery lock should remain healthy")
            .is_empty(),
        "download runtime replay must stay suppressed while authority commit fence is active"
    );

    manager
        .authority_commit_in_progress
        .store(false, std::sync::atomic::Ordering::SeqCst);
    manager
        .replay_download_runtime_projection_to_callbacks()
        .await;

    let delivered = delivered
        .lock()
        .expect("runtime delivery lock should remain healthy");
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0]
            .completed_downloads
            .first()
            .map(|download| download.guid.as_str()),
        Some("guid-authority-fence")
    );
    assert_eq!(
        delivered[0]
            .last_download
            .as_ref()
            .map(|download| download.state),
        Some(DownloadState::Completed)
    );
}

#[tokio::test]
async fn dialog_runtime_replay_waits_for_authority_commit_fence_to_clear() {
    let manager = BrowserManager::new(options());
    {
        let mut runtime = manager.dialog_runtime.write().await;
        runtime.status = rub_core::model::DialogRuntimeStatus::Active;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "dialog-authority-fence".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-dialog-fence".to_string()),
            tab_target_id: Some("tab-dialog-fence".to_string()),
        });
        runtime.last_dialog = runtime.pending_dialog.clone();
    }

    let delivered = Arc::new(StdMutex::new(
        Vec::<rub_core::model::DialogRuntimeInfo>::new(),
    ));
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: {
            let delivered = delivered.clone();
            Some(Arc::new(move |update| {
                delivered
                    .lock()
                    .expect("dialog runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };

    manager
        .authority_commit_in_progress
        .store(true, std::sync::atomic::Ordering::SeqCst);
    manager
        .replay_dialog_runtime_projection_to_callbacks()
        .await;
    assert!(
        delivered
            .lock()
            .expect("dialog runtime delivery lock should remain healthy")
            .is_empty(),
        "dialog runtime replay must stay suppressed while authority commit fence is active"
    );

    manager
        .authority_commit_in_progress
        .store(false, std::sync::atomic::Ordering::SeqCst);
    manager
        .replay_dialog_runtime_projection_to_callbacks()
        .await;

    let delivered = delivered
        .lock()
        .expect("dialog runtime delivery lock should remain healthy");
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0]
            .pending_dialog
            .as_ref()
            .map(|dialog| dialog.message.as_str()),
        Some("dialog-authority-fence")
    );
    assert_eq!(
        delivered[0].status,
        rub_core::model::DialogRuntimeStatus::Active
    );
}

#[tokio::test]
async fn replace_browser_authority_rolls_back_when_previous_release_fails() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let previous_page = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("previous page authority");
    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("previous browser authority")
        .websocket_address()
        .clone();
    let previous_target = previous_page.target_id().clone();
    let previous_target_id = previous_page.target_id().as_ref().to_string();
    manager
        .network_rule_runtime
        .write()
        .await
        .replace_rules(vec![NetworkRule {
            id: 9,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.test/*".to_string(),
            },
        }]);
    manager.request_correlation.lock().await.record(
        "req-rollback".to_string(),
        None,
        "GET",
        crate::request_correlation::RequestCorrelation {
            tab_target_id: Some(previous_target_id.clone()),
            original_url: "https://example.test/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: Vec::new(),
        },
    );
    let previous_pending_registry =
        crate::runtime_observatory::new_shared_pending_request_registry();
    manager.observatory_pending_registries.lock().await.insert(
        previous_target_id.clone(),
        previous_pending_registry.clone(),
    );
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Confirm rollback".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-rollback".to_string()),
            tab_target_id: Some(previous_target_id.clone()),
        });
    }
    manager.download_runtime.write().await.set_runtime(
        17,
        DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/rub-downloads-authority-rollback".to_string()),
            ..DownloadRuntimeInfo::default()
        },
    );
    manager.download_runtime.write().await.record_started(
        17,
        "guid-authority-rollback".to_string(),
        "https://example.test/rollback.csv".to_string(),
        "rollback.csv".to_string(),
        Some("frame-rollback".to_string()),
    );
    manager.download_runtime.write().await.record_progress(
        17,
        "guid-authority-rollback".to_string(),
        DownloadState::InProgress,
        32,
        Some(64),
        None,
    );
    let delivered_dialog_runtime = Arc::new(StdMutex::new(
        Vec::<rub_core::model::DialogRuntimeInfo>::new(),
    ));
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: {
            let delivered_dialog_runtime = delivered_dialog_runtime.clone();
            Some(Arc::new(move |update| {
                delivered_dialog_runtime
                    .lock()
                    .expect("dialog runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };
    manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: Some("restore".to_string()),
            target_tab_id: Some(previous_target_id.clone()),
        })
        .expect("dialog intercept should seed snapshot state");
    {
        let mut coverage = manager.identity_coverage.lock().await;
        coverage.record_self_probe(IdentitySelfProbeInfo {
            page_main_world: Some(IdentityProbeStatus::Passed),
            iframe_context: Some(IdentityProbeStatus::Unknown),
            worker_context: Some(IdentityProbeStatus::Passed),
            ua_consistency: Some(IdentityProbeStatus::Passed),
            webgl_surface: Some(IdentityProbeStatus::Passed),
            canvas_surface: Some(IdentityProbeStatus::Passed),
            audio_surface: Some(IdentityProbeStatus::Passed),
            permissions_surface: Some(IdentityProbeStatus::Passed),
            viewport_surface: Some(IdentityProbeStatus::Passed),
            touch_surface: Some(IdentityProbeStatus::Passed),
            window_metrics_surface: Some(IdentityProbeStatus::Passed),
            unsupported_surfaces: vec!["service_worker".to_string()],
        });
    }
    let (replacement_browser, replacement_page, _) =
        crate::runtime::attach_external_browser(&previous_ws_url)
            .await
            .expect("external replacement authority should attach");
    let second_page = Arc::new(
        manager
            .browser
            .lock()
            .await
            .clone()
            .expect("previous browser authority")
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![previous_page.clone(), second_page],
        current_page: None,
        continuity_page: None,
        active_target_id: Some(previous_target.clone()),
        active_target_authority: Some(TabActiveAuthority::LocalFallback),
    };
    *manager.local_active_target_authority.lock().await = Some(
        crate::tab_projection::LocalActiveTargetAuthority::new(previous_target.clone()),
    );

    manager.force_previous_authority_release_failure();
    let error = manager
        .replace_browser_authority(
            replacement_browser,
            replacement_page,
            true,
            Some(ConnectionTarget::CdpUrl {
                url: previous_ws_url.clone(),
            }),
            None,
        )
        .await
        .expect_err("failed previous-authority release should roll back replacement");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
    let context = envelope.context.expect("rollback context");
    assert_eq!(context["new_authority_committed"], serde_json::json!(false));
    assert_eq!(
        context["replacement_authority_released"],
        serde_json::json!(true)
    );
    assert_eq!(
        context["previous_authority_restored"],
        serde_json::json!(true)
    );
    assert!(
        manager.connection_target.lock().await.is_none(),
        "rollback should restore the prior managed connection target projection"
    );
    assert!(!manager.is_external().await);
    assert!(
        manager
            .projected_continuity_page()
            .await
            .as_ref()
            .is_some_and(|page| page.target_id().as_ref() == previous_target_id.as_str()),
        "rollback should recover the previous continuity authority even when the old continuity projection was absent"
    );
    let restored_tab_projection = manager.tab_projection.lock().await.clone();
    assert_eq!(
        restored_tab_projection.active_target_id,
        Some(previous_target),
        "rollback should restore prior active-tab fallback projection"
    );
    assert_eq!(
        restored_tab_projection.active_target_authority,
        Some(TabActiveAuthority::LocalFallback),
        "rollback should not collapse local active-tab fallback into a single-page projection"
    );
    assert_eq!(
        manager
            .dialog_runtime()
            .read()
            .await
            .pending_dialog
            .as_ref()
            .and_then(|dialog| dialog.tab_target_id.clone()),
        Some(previous_target_id.clone()),
        "rollback should preserve prior dialog runtime authority"
    );
    assert_eq!(
        manager.download_runtime.read().await.projection().status,
        DownloadRuntimeStatus::Active,
        "rollback should preserve prior download runtime authority"
    );
    assert!(
        manager
            .download_runtime
            .read()
            .await
            .projection()
            .active_downloads
            .iter()
            .any(|download| download.guid == "guid-authority-rollback"),
        "rollback should restore prior download progress projection"
    );
    assert!(
        manager
            .request_correlation
            .lock()
            .await
            .take_for_request(
                "req-rollback",
                "https://example.test/api",
                "GET",
                None,
                Some(previous_target_id.as_str())
            )
            .is_some(),
        "rollback should preserve prior request-correlation authority"
    );
    let restored_pending_registry = manager
        .observatory_pending_registries
        .lock()
        .await
        .get(&previous_target_id)
        .cloned()
        .expect("rollback should restore observatory pending registry authority");
    assert!(
        Arc::ptr_eq(&restored_pending_registry, &previous_pending_registry),
        "rollback should restore the previous observatory pending-registry authority"
    );
    let restored_intercept = manager
        .dialog_intercept
        .lock()
        .expect("dialog intercept lock should remain healthy")
        .clone();
    assert_eq!(
        restored_intercept
            .as_ref()
            .and_then(|policy| policy.target_tab_id.clone()),
        Some(previous_target_id.clone()),
        "rollback should preserve dialog intercept state"
    );
    assert_eq!(
        manager
            .network_rule_runtime
            .read()
            .await
            .rules_snapshot()
            .len(),
        1,
        "rollback should preserve prior network-rule runtime state"
    );
    assert_eq!(
        manager
            .identity_coverage
            .lock()
            .await
            .project()
            .self_probe
            .as_ref()
            .and_then(|probe| probe.page_main_world),
        Some(IdentityProbeStatus::Passed),
        "rollback should preserve prior identity coverage projection"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn failed_initial_install_clears_half_installed_authority_when_cleanup_fails() {
    let owner = BrowserManager::new(options());
    owner
        .ensure_browser()
        .await
        .expect("owner browser should launch");
    let owner_ws_url = owner
        .browser
        .lock()
        .await
        .clone()
        .expect("owner browser authority")
        .websocket_address()
        .clone();

    let manager = BrowserManager::new(options());
    manager
        .set_connection_target(ConnectionTarget::CdpUrl {
            url: owner_ws_url.clone(),
        })
        .await;
    manager.force_generation_bound_runtime_reconcile_failure();
    manager.force_current_authority_release_failure();

    let error = manager
        .ensure_browser()
        .await
        .expect_err("failed install should surface browser launch failure");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
    let context = envelope.context.expect("failed install context");
    assert_eq!(
        context["runtime_state_cleanup_succeeded"],
        serde_json::json!(false)
    );
    assert_eq!(
        context["runtime_state_authority_cleared"],
        serde_json::json!(true)
    );
    assert!(
        manager.browser.lock().await.is_none(),
        "failed install must not leave half-installed browser authority behind"
    );
    let projection = manager.tab_projection.lock().await.clone();
    assert!(projection.pages.is_empty());
    assert!(projection.current_page.is_none());
    assert!(projection.continuity_page.is_none());
    assert!(projection.active_target_id.is_none());

    owner
        .close()
        .await
        .expect("owner browser should close cleanly");
}

#[tokio::test]
async fn failed_managed_profile_ownership_commit_clears_new_initial_authority() {
    let manager = BrowserManager::new(options());
    manager.force_managed_profile_ownership_commit_failure();

    let error = manager
        .ensure_browser()
        .await
        .expect_err("ownership commit failure should fail initial install");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
    let context = envelope.context.expect("ownership failure context");
    assert_eq!(
        context["managed_profile_authority_cleared"],
        serde_json::json!(true)
    );
    assert!(
        manager.browser.lock().await.is_none(),
        "failed ownership commit must not leave a committed browser authority behind"
    );
    assert!(
        manager.managed_profile_authority_for_test().await.is_none(),
        "managed-profile plane must clear alongside failed initial authority install"
    );
}

#[tokio::test]
async fn failed_replacement_install_restores_previous_authority_after_fail_closed_cleanup() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;
    let previous_current_page = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("previous page authority");
    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("previous browser authority")
        .websocket_address()
        .clone();
    let previous_target_id = previous_current_page.target_id().as_ref().to_string();
    let (replacement_browser, replacement_page, _) =
        crate::runtime::attach_external_browser(&previous_ws_url)
            .await
            .expect("replacement browser authority should attach");
    let second_page = Arc::new(
        manager
            .browser
            .lock()
            .await
            .clone()
            .expect("previous browser authority")
            .new_page("about:blank")
            .await
            .expect("second page should open"),
    );
    *manager.tab_projection.lock().await = CommittedTabProjection {
        pages: vec![previous_current_page.clone(), second_page],
        current_page: None,
        continuity_page: None,
        active_target_id: None,
        active_target_authority: None,
    };

    manager.force_generation_bound_runtime_reconcile_failure();
    manager.force_current_authority_release_failure();
    let error = manager
        .replace_browser_authority(
            replacement_browser,
            replacement_page,
            true,
            Some(ConnectionTarget::CdpUrl {
                url: previous_ws_url.clone(),
            }),
            None,
        )
        .await
        .expect_err("failed replacement install should surface browser launch failure");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
    let context = envelope.context.expect("failed replacement context");
    assert_eq!(
        context["runtime_state_cleanup_succeeded"],
        serde_json::json!(false)
    );
    assert_eq!(
        context["runtime_state_authority_cleared"],
        serde_json::json!(true)
    );
    assert!(
        manager.browser.lock().await.is_some(),
        "previous authority should be restored after failed replacement install"
    );
    assert!(
        !manager.is_external().await,
        "previous managed authority should remain the committed runtime after rollback"
    );
    assert!(
        manager
            .projected_continuity_page()
            .await
            .as_ref()
            .is_some_and(|page| page.target_id().as_ref() == previous_target_id.as_str()),
        "rollback should recover the previous continuity authority even when the old continuity projection was absent"
    );
    assert!(
        manager
            .tab_projection
            .lock()
            .await
            .pages
            .iter()
            .any(|page| page.target_id().as_ref() == previous_target_id.as_str()),
        "restored authority should still include the previous page target"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn failed_replacement_ownership_commit_restores_previous_managed_profile_authority() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    manager
        .set_connection_target(ConnectionTarget::Managed)
        .await;
    let previous_profile = manager
        .managed_profile_authority_for_test()
        .await
        .expect("previous managed-profile authority");
    let previous_continuity_page = manager
        .projected_continuity_page()
        .await
        .expect("previous continuity authority");

    let replacement_options = options();
    let replacement_profile = crate::managed_browser::resolve_managed_profile_dir(
        replacement_options.user_data_dir.clone(),
        replacement_options.profile_directory.clone(),
        replacement_options.managed_profile_ephemeral,
    );
    let replacement_identity_policy =
        crate::identity_policy::IdentityPolicy::from_options(&replacement_options);
    let (replacement_browser, replacement_page) =
        crate::runtime::launch_managed_browser(&replacement_options, &replacement_identity_policy)
            .await
            .expect("replacement managed browser should launch");

    manager.force_managed_profile_ownership_commit_failure();
    let error = manager
        .replace_browser_authority(
            replacement_browser,
            replacement_page,
            false,
            Some(ConnectionTarget::Managed),
            Some(replacement_profile.clone()),
        )
        .await
        .expect_err("replacement ownership commit failure should roll back");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
    assert_eq!(
        manager
            .managed_profile_authority_for_test()
            .await
            .as_ref()
            .map(|profile| &profile.path),
        Some(&previous_profile.path),
        "rollback must restore the previous managed-profile authority plane"
    );
    assert_eq!(
        manager
            .projected_continuity_page()
            .await
            .as_ref()
            .expect("previous continuity should be restored")
            .target_id()
            .as_ref(),
        previous_continuity_page.target_id().as_ref(),
    );
    assert!(
        !manager.is_external().await,
        "rollback after ownership-commit failure must restore prior managed authority"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[tokio::test]
async fn replace_browser_authority_resets_generation_bound_runtime_state_on_commit() {
    let manager = BrowserManager::new(options());
    manager
        .ensure_browser()
        .await
        .expect("managed browser should launch");
    let previous_ws_url = manager
        .browser
        .lock()
        .await
        .clone()
        .expect("previous browser authority")
        .websocket_address()
        .clone();
    let previous_target_id = manager
        .tab_projection
        .lock()
        .await
        .current_page
        .clone()
        .expect("previous page authority")
        .target_id()
        .as_ref()
        .to_string();
    manager.request_correlation.lock().await.record(
        "req-reset".to_string(),
        None,
        "GET",
        crate::request_correlation::RequestCorrelation {
            tab_target_id: Some(previous_target_id.clone()),
            original_url: "https://example.test/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: Vec::new(),
        },
    );
    let stale_pending_registry = crate::runtime_observatory::new_shared_pending_request_registry();
    manager
        .observatory_pending_registries
        .lock()
        .await
        .insert(previous_target_id.clone(), stale_pending_registry.clone());
    {
        let dialog_runtime = manager.dialog_runtime();
        let mut runtime = dialog_runtime.write().await;
        runtime.pending_dialog = Some(rub_core::model::PendingDialogInfo {
            kind: rub_core::model::DialogKind::Alert,
            message: "Old authority dialog".to_string(),
            default_prompt: None,
            url: "https://example.test/dialog".to_string(),
            has_browser_handler: false,
            opened_at: "2026-04-15T00:00:00Z".to_string(),
            frame_id: Some("frame-reset".to_string()),
            tab_target_id: Some(previous_target_id.clone()),
        });
    }
    let delivered_dialog_runtime = Arc::new(StdMutex::new(
        Vec::<rub_core::model::DialogRuntimeInfo>::new(),
    ));
    *manager.dialog_callbacks.lock().await = crate::dialogs::DialogCallbacks {
        on_runtime: {
            let delivered_dialog_runtime = delivered_dialog_runtime.clone();
            Some(Arc::new(move |update| {
                delivered_dialog_runtime
                    .lock()
                    .expect("dialog runtime delivery lock should remain healthy")
                    .push(update.runtime);
            }))
        },
        on_opened: None,
        on_closed: None,
        on_listener_ended: None,
    };
    manager
        .set_dialog_intercept(rub_core::model::DialogInterceptPolicy {
            accept: true,
            prompt_text: Some("stale".to_string()),
            target_tab_id: Some(previous_target_id.clone()),
        })
        .expect("dialog intercept should seed stale state");
    manager
        .network_rule_runtime
        .write()
        .await
        .replace_rules(vec![NetworkRule {
            id: 7,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.test/*".to_string(),
            },
        }]);
    manager
        .network_rule_runtime
        .write()
        .await
        .mark_fetch_enabled("stale-target".to_string());
    manager
        .identity_coverage
        .lock()
        .await
        .record_target("stale-shared-worker", "shared_worker");

    let (replacement_browser, replacement_page, _) =
        crate::runtime::attach_external_browser(&previous_ws_url)
            .await
            .expect("replacement browser authority should attach");

    manager
        .replace_browser_authority(
            replacement_browser,
            replacement_page,
            true,
            Some(ConnectionTarget::CdpUrl {
                url: previous_ws_url.clone(),
            }),
            None,
        )
        .await
        .expect("replacement authority should commit");

    assert!(
        manager
            .runtime_state_replay_attempt_count
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1,
        "replacement authority commit must schedule a post-fence runtime-state replay"
    );
    assert!(
        manager
            .dialog_runtime()
            .read()
            .await
            .pending_dialog
            .is_none(),
        "replacement authority should not inherit stale pending dialog state"
    );
    {
        let delivered_dialog_runtime = delivered_dialog_runtime
            .lock()
            .expect("dialog runtime delivery lock should remain healthy");
        assert_eq!(delivered_dialog_runtime.len(), 1);
        assert!(
            delivered_dialog_runtime[0].pending_dialog.is_none(),
            "replacement authority should replay cleared dialog runtime to callback-backed consumers"
        );
        assert_eq!(
            delivered_dialog_runtime[0].status,
            rub_core::model::DialogRuntimeStatus::Inactive
        );
    }
    assert!(
        manager
            .request_correlation
            .lock()
            .await
            .take_for_request(
                "req-reset",
                "https://example.test/api",
                "GET",
                None,
                Some(previous_target_id.as_str())
            )
            .is_none(),
        "replacement authority should not inherit stale request correlation state"
    );
    if let Some(committed_pending_registry) = manager
        .observatory_pending_registries
        .lock()
        .await
        .get(&previous_target_id)
        .cloned()
    {
        assert!(
            !Arc::ptr_eq(&committed_pending_registry, &stale_pending_registry),
            "replacement authority should not retain stale observatory pending-registry authority on commit"
        );
    }
    assert!(
        manager
            .dialog_intercept
            .lock()
            .expect("dialog intercept lock should remain healthy")
            .is_none(),
        "replacement authority should clear stale dialog intercept state"
    );
    let network_runtime = manager.network_rule_runtime.read().await;
    assert!(
        !network_runtime.is_fetch_enabled_for("stale-target"),
        "replacement authority should clear stale fetch-installation state"
    );
    assert_eq!(network_runtime.rules_snapshot().len(), 1);
    drop(network_runtime);

    let projected_coverage = manager.identity_coverage.lock().await.project();
    assert!(
        projected_coverage
            .observed_target_types
            .iter()
            .all(|target_type| target_type != "shared_worker"),
        "replacement authority should not inherit stale identity-coverage targets"
    );

    manager.close().await.expect("browser should close cleanly");
}

#[test]
fn browser_manager_derives_unique_ephemeral_profile_authority_when_unset() {
    let manager_a = BrowserManager::new(BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    });
    let manager_b = BrowserManager::new(BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    });

    assert!(manager_a.options.managed_profile_ephemeral);
    assert!(manager_b.options.managed_profile_ephemeral);
    assert_ne!(
        manager_a.options.user_data_dir,
        manager_b.options.user_data_dir
    );
    assert!(
        manager_a
            .options
            .user_data_dir
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rub-chrome-hex-"))
    );
}

#[test]
fn retire_uncommitted_listener_generation_advances_generation_once() {
    let manager = BrowserManager::new(options());
    let generation = manager.bump_listener_generation();
    assert_eq!(manager.current_listener_generation(), generation);

    manager.retire_uncommitted_listener_generation(generation);
    let retired = manager.current_listener_generation();
    assert!(retired > generation);

    manager.retire_uncommitted_listener_generation(generation);
    assert_eq!(manager.current_listener_generation(), retired);
}
