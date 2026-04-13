use super::*;
use rub_core::model::{
    ConnectionTarget, IdentityProbeStatus, IdentitySelfProbeInfo, NetworkRule, NetworkRuleEffect,
    NetworkRuleEffectKind, NetworkRuleSpec, NetworkRuleStatus,
};

fn options() -> BrowserLaunchOptions {
    BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: Some(PathBuf::from("/tmp/rub-profile")),
        download_dir: Some(PathBuf::from("/tmp/rub-downloads")),
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

#[test]
fn managed_close_uses_daemon_pid_derived_temp_profile_when_unset() {
    let profile = resolve_managed_profile_dir(None);
    assert!(profile.ephemeral);
    assert_eq!(
        profile.path,
        std::env::temp_dir().join(format!("rub-chrome-{}", std::process::id()))
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
