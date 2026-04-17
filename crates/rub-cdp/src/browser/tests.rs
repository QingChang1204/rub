use super::control::{
    pending_dialog_authority, pending_dialog_target_id, stale_pending_dialog_target_error,
};
use super::*;
use rub_core::model::{
    ConnectionTarget, IdentityProbeStatus, IdentitySelfProbeInfo, NetworkRule, NetworkRuleEffect,
    NetworkRuleEffectKind, NetworkRuleSpec, NetworkRuleStatus,
};
use std::sync::Arc;
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
        on_snapshot: Some(std::sync::Arc::new(|_, _| {})),
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
    assert_eq!(
        manager
            .tab_projection
            .lock()
            .await
            .current_page
            .as_ref()
            .expect("restored page authority")
            .target_id()
            .as_ref(),
        previous_page.target_id().as_ref()
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
    let previous_continuity_page = manager
        .projected_continuity_page()
        .await
        .expect("previous continuity authority");
    let previous_target_id = previous_continuity_page.target_id().as_ref().to_string();
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
    assert_eq!(
        manager
            .projected_continuity_page()
            .await
            .as_ref()
            .expect("continuity authority should be restored")
            .target_id()
            .as_ref(),
        previous_target_id,
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
            .dialog_runtime()
            .read()
            .await
            .pending_dialog
            .is_none(),
        "replacement authority should not inherit stale pending dialog state"
    );
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
            .is_some_and(|name| name.starts_with("rub-chrome-manager-"))
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
