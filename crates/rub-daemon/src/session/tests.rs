use super::{
    BrowserSessionEvent, BrowserSessionEventSink, ReplayCommandClaim, ReplayFenceState,
    SessionState,
};
use crate::session::protocol::BROWSER_EVENT_CRITICAL_SOFT_LIMIT;
use rub_core::model::{
    AuthState, ConsoleErrorEvent, DownloadMode, DownloadRuntimeStatus, DownloadState,
    FrameContextStatus, HumanVerificationHandoffInfo, HumanVerificationHandoffStatus,
    IntegrationMode, IntegrationRuntimeInfo, IntegrationRuntimeStatus, IntegrationSurface,
    InterferenceKind, InterferenceMode, InterferenceObservation, InterferenceRuntimeInfo,
    InterferenceRuntimeStatus, NetworkRequestLifecycle, NetworkRequestRecord, NetworkRuleSpec,
    NetworkRuleStatus, OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo,
    OrchestrationMode, OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeStatus,
    OverlayState, ReadinessInfo, ReadinessStatus, RequestSummaryEvent, RouteStability,
    RuntimeObservatoryStatus, RuntimeStateSnapshot, Snapshot, StateInspectorInfo,
    StateInspectorStatus, TriggerActionKind, TriggerActionSpec, TriggerConditionKind,
    TriggerConditionSpec,
};
use rub_core::storage::{StorageArea, StorageMutationKind, StorageRuntimeStatus, StorageSnapshot};
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::oneshot;

fn sample_orchestration_rule(id: u32) -> OrchestrationRuleInfo {
    OrchestrationRuleInfo {
        id,
        status: OrchestrationRuleStatus::Armed,
        source: OrchestrationAddressInfo {
            session_id: "sess-source".to_string(),
            session_name: "source".to_string(),
            tab_index: None,
            tab_target_id: None,
            frame_id: None,
        },
        target: OrchestrationAddressInfo {
            session_id: "sess-target".to_string(),
            session_name: "target".to_string(),
            tab_index: None,
            tab_target_id: None,
            frame_id: None,
        },
        mode: OrchestrationMode::Once,
        execution_policy: OrchestrationExecutionPolicyInfo::default(),
        condition: TriggerConditionSpec {
            kind: TriggerConditionKind::TextPresent,
            locator: None,
            text: Some("Ready".to_string()),
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        },
        actions: vec![TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(serde_json::json!({ "workflow_name": "reply_flow" })),
        }],
        correlation_key: format!("corr-{id}"),
        idempotency_key: format!("idem-{id}"),
        unavailable_reason: None,
        last_condition_evidence: None,
        last_result: None,
    }
}

#[tokio::test]
async fn session_starts_with_inactive_normal_integration_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let integration = state.integration_runtime().await;

    assert_eq!(integration.mode, IntegrationMode::Normal);
    assert_eq!(integration.status, IntegrationRuntimeStatus::Inactive);
    assert_eq!(integration.request_rule_count, 0);
    assert!(integration.request_rules.is_empty());
    assert!(integration.active_surfaces.is_empty());
    assert!(integration.degraded_surfaces.is_empty());
    assert!(!integration.observatory_ready);
    assert!(!integration.readiness_ready);
    assert!(!integration.state_inspector_ready);
}

#[tokio::test]
async fn request_shutdown_notifies_waiters_and_marks_draining() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let notifier = state.shutdown_notifier();
    let waiter = notifier.notified();
    tokio::pin!(waiter);

    state.request_shutdown();

    waiter.as_mut().await;
    assert!(state.is_shutdown_requested());
}

#[tokio::test]
async fn session_starts_with_inactive_normal_interference_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let interference = state.interference_runtime().await;

    assert_eq!(interference.mode, InterferenceMode::Normal);
    assert_eq!(interference.status, InterferenceRuntimeStatus::Inactive);
    assert!(interference.current_interference.is_none());
    assert!(interference.last_interference.is_none());
    assert!(interference.active_policies.is_empty());
    assert!(!interference.recovery_in_progress);
    assert!(interference.last_recovery_action.is_none());
    assert!(interference.last_recovery_result.is_none());
    assert!(!interference.handoff_required);
}

#[tokio::test]
async fn session_starts_with_inactive_download_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let downloads = state.download_runtime().await;

    assert_eq!(downloads.status, DownloadRuntimeStatus::Inactive);
    assert_eq!(downloads.mode, DownloadMode::ObserveOnly);
    assert!(downloads.active_downloads.is_empty());
    assert!(downloads.completed_downloads.is_empty());
}

#[tokio::test]
async fn session_starts_with_inactive_storage_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let storage = state.storage_runtime().await;

    assert_eq!(storage.status, StorageRuntimeStatus::Inactive);
    assert!(storage.current_origin.is_none());
    assert!(storage.local_storage_keys.is_empty());
    assert!(storage.session_storage_keys.is_empty());
    assert!(storage.recent_mutations.is_empty());
}

#[tokio::test]
async fn session_starts_with_inactive_trigger_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let trigger = state.trigger_runtime().await;

    assert_eq!(
        trigger.status,
        rub_core::model::TriggerRuntimeStatus::Inactive
    );
    assert!(trigger.triggers.is_empty());
    assert_eq!(trigger.active_count, 0);
    assert_eq!(trigger.degraded_count, 0);
}

#[tokio::test]
async fn automation_scheduler_metrics_track_queue_owned_worker_cycles() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);

    tokio::time::sleep(Duration::from_millis(2)).await;
    state.record_trigger_worker_cycle_started();
    state.record_orchestration_worker_cycle_started();

    let metrics = state.automation_scheduler_metrics().await;

    assert_eq!(metrics["slice"], "shared_fifo_scheduler_policy");
    assert_eq!(
        metrics["authority_inventory"]["trigger_worker_cycle_entry"],
        "trigger_worker.run_trigger_cycle"
    );
    assert_eq!(
        metrics["authority_inventory"]["accepted_connection_fence_authority"],
        "session.connected_client_count"
    );
    assert_eq!(
        metrics["authority_inventory"]["pre_request_response_fence_authority"],
        "session.pre_request_response_fence_count"
    );
    assert_eq!(
        metrics["authority_inventory"]["trigger_worker_pre_queue_gate"],
        "none"
    );
    assert_eq!(
        metrics["trigger_worker"]["rule_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["trigger_worker"]["cycle_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["reservation_wait_policy"]["worker_cycle"]["mode"],
        serde_json::json!("bounded_worker_cycle_budget")
    );
    assert_eq!(
        metrics["reservation_wait_policy"]["worker_cycle"]["wait_budget_ms"],
        serde_json::json!(500)
    );
    assert_eq!(
        metrics["reservation_wait_policy"]["active_orchestration_step"]["mode"],
        serde_json::json!("action_timeout_budget")
    );
    assert_eq!(
        metrics["reservation_wait_policy"]["active_orchestration_step"]["timeout_authority"],
        serde_json::json!("orchestration_action_request.timeout_ms")
    );
    assert!(
        metrics["trigger_worker"]["last_cycle_uptime_ms"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        metrics["orchestration_worker"]["metrics"]["rule_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["orchestration_worker"]["metrics"]["cycle_count"],
        serde_json::json!(1)
    );
}

#[tokio::test]
async fn automation_scheduler_metrics_track_queue_pressure_and_shutdown_drain() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);

    tokio::time::sleep(Duration::from_millis(2)).await;
    state.record_in_flight_count_observation(3);
    state.record_queue_pressure_timeout();
    state.record_shutdown_drain_wait(2, 1, 3);
    state.record_shutdown_drain_soft_timeout(4, 2, 5);

    let metrics = state.automation_scheduler_metrics().await;

    assert_eq!(
        metrics["queue_pressure"]["max_in_flight_count"],
        serde_json::json!(3)
    );
    assert_eq!(
        metrics["queue_pressure"]["queue_timeout_count"],
        serde_json::json!(1)
    );
    assert!(
        metrics["queue_pressure"]["last_queue_timeout_uptime_ms"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        metrics["shutdown_drain"]["wait_loop_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["shutdown_drain"]["soft_timeout_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["shutdown_drain"]["connected_only_soft_release_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_in_flight_count"],
        serde_json::json!(4)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_connected_client_count"],
        serde_json::json!(2)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_pre_request_response_fence_count"],
        serde_json::json!(5)
    );
}

#[tokio::test]
async fn browser_event_ingress_metrics_track_metered_critical_pressure() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let sink = BrowserSessionEventSink::metered_critical_for_test(&state);

    tokio::time::sleep(Duration::from_millis(2)).await;
    for _ in 0..=BROWSER_EVENT_CRITICAL_SOFT_LIMIT {
        sink.enqueue(BrowserSessionEvent::DialogRuntime {
            browser_sequence: state.allocate_browser_event_sequence(),
            generation: 1,
            status: rub_core::model::DialogRuntimeStatus::Active,
            degraded_reason: None,
        });
    }

    let metrics = state.browser_event_ingress_metrics().await;

    assert_eq!(
        metrics["critical"]["mode"],
        serde_json::json!("lossless_metered_unbounded")
    );
    assert_eq!(
        metrics["critical"]["soft_limit"],
        serde_json::json!(BROWSER_EVENT_CRITICAL_SOFT_LIMIT)
    );
    assert_eq!(
        metrics["critical"]["pending_count"],
        serde_json::json!(BROWSER_EVENT_CRITICAL_SOFT_LIMIT + 1)
    );
    assert_eq!(
        metrics["critical"]["max_pending_count"],
        serde_json::json!(BROWSER_EVENT_CRITICAL_SOFT_LIMIT + 1)
    );
    assert_eq!(
        metrics["critical"]["pressure_active"],
        serde_json::json!(true)
    );
    assert_eq!(
        metrics["critical"]["soft_limit_cross_count"],
        serde_json::json!(1)
    );
    assert!(
        metrics["critical"]["last_soft_limit_cross_uptime_ms"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        metrics["progress"]["mode"],
        serde_json::json!("bounded_drop_with_degraded_marker")
    );
}

#[tokio::test]
async fn session_records_storage_snapshot_and_mutation_history() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .set_storage_snapshot(StorageSnapshot {
            origin: "https://example.test".to_string(),
            local_storage: BTreeMap::from([("token".to_string(), "abc".to_string())]),
            session_storage: BTreeMap::from([("csrf".to_string(), "def".to_string())]),
        })
        .await;
    state
        .record_storage_mutation(
            StorageMutationKind::Set,
            "https://example.test".to_string(),
            Some(StorageArea::Local),
            Some("token".to_string()),
        )
        .await;

    let storage = state.storage_runtime().await;
    assert_eq!(storage.status, StorageRuntimeStatus::Active);
    assert_eq!(
        storage.current_origin.as_deref(),
        Some("https://example.test")
    );
    assert_eq!(storage.local_storage_keys, vec!["token"]);
    assert_eq!(storage.session_storage_keys, vec!["csrf"]);
    assert_eq!(storage.recent_mutations.len(), 1);
    assert_eq!(storage.recent_mutations[0].kind, StorageMutationKind::Set);
    assert_eq!(storage.recent_mutations[0].area, Some(StorageArea::Local));
}

#[tokio::test]
async fn session_flushes_post_commit_projection_into_history_and_workflow_capture() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let request = IpcRequest::new(
        "pipe",
        serde_json::json!({
            "spec": "[]",
            "spec_source": {
                "kind": "file",
                "path": "/tmp/workflow.json",
                "path_state": {
                    "truth_level": "input_path_reference",
                    "path_authority": "cli.pipe.spec_source.path",
                    "upstream_truth": "cli_pipe_file_option",
                    "path_kind": "workflow_spec_file",
                    "control_role": "display_only"
                }
            }
        }),
        30_000,
    )
    .with_command_id("cmd-1")
    .expect("static command_id must be valid");
    let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}))
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");

    state.submit_post_commit_projection(&request, &response);
    assert_eq!(state.pending_post_commit_projection_count(), 1);

    let history = state.command_history(5).await;
    assert_eq!(state.pending_post_commit_projection_count(), 0);
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].command, "pipe");
    assert!(history.entries[0].summary.is_some());
    assert_eq!(history.oldest_retained_sequence, Some(1));
    assert_eq!(history.newest_retained_sequence, Some(1));
    assert_eq!(history.dropped_before_retention, 0);

    let capture = state.workflow_capture(5).await;
    assert_eq!(capture.entries.len(), 1);
    assert_eq!(capture.entries[0].command, "pipe");
    assert_eq!(capture.entries[0].command_id.as_deref(), Some("cmd-1"));
    assert_eq!(
        capture.entries[0].args["spec_source"]["path"],
        serde_json::json!("/tmp/workflow.json")
    );
    assert_eq!(
        capture.entries[0].args["spec_source"]["path_state"]["path_authority"],
        serde_json::json!("cli.pipe.spec_source.path")
    );
}

#[tokio::test]
async fn session_background_projection_drain_applies_pending_entries() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        30_000,
    );
    let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));

    state.submit_post_commit_projection(&request, &response);
    state.spawn_post_commit_projection_drain();

    tokio::time::timeout(std::time::Duration::from_millis(100), async {
        loop {
            if state.pending_post_commit_projection_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background projection drain should empty the queue");

    let history = state.command_history(5).await;
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].command, "open");

    let capture = state.workflow_capture(5).await;
    assert_eq!(capture.entries.len(), 1);
    assert_eq!(capture.entries[0].command, "open");
}

#[tokio::test]
async fn session_records_redacted_post_commit_journal_entry() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let home = std::env::temp_dir().join(format!(
            "rub-post-commit-journal-redact-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let secrets = home.join("secrets.env");
        std::fs::write(&secrets, "RUB_TOKEN=token-123\n").expect("write secrets");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600))
            .expect("set permissions");

        let state = SessionState::new("default", home.clone(), None);
        let request = IpcRequest::new(
            "type",
            serde_json::json!({ "selector": "#password", "text": "token-123", "clear": true }),
            1_000,
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "echo": "token-123",
            }),
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");

        state
            .record_post_commit_journal(&request, &response)
            .await
            .expect("journal append succeeds");
        let journal = state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal");

        assert_eq!(journal.len(), 1);
        assert_eq!(
            journal[0]["journal_state"]["surface"],
            serde_json::json!("post_commit_journal")
        );
        assert_eq!(
            journal[0]["journal_state"]["visibility"],
            serde_json::json!("internal_only")
        );
        assert_eq!(
            journal[0]["journal_state"]["recovery_role"],
            serde_json::json!("daemon_recovery_writer")
        );
        assert_eq!(
            journal[0]["journal_state"]["upstream_commit_truth"],
            serde_json::json!("daemon_response_committed")
        );
        assert_eq!(
            journal[0]["journal_state"]["retention_scope"],
            serde_json::json!("session_runtime_cleanup")
        );
        assert_eq!(
            journal[0]["request"]["args"]["text"],
            serde_json::json!("$RUB_TOKEN")
        );
        assert_eq!(
            journal[0]["response"]["data"]["echo"],
            serde_json::json!("$RUB_TOKEN")
        );
        assert_eq!(journal[0]["command_id"], serde_json::json!("cmd-1"));
        assert_eq!(
            journal[0]["request_redaction_lossy"],
            serde_json::json!(false)
        );
        assert_eq!(
            journal[0]["response_redaction_lossy"],
            serde_json::json!(false)
        );

        let _ = std::fs::remove_dir_all(home);
    }
}

#[tokio::test]
async fn session_redacts_post_commit_journal_error_response_context() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let home = std::env::temp_dir().join(format!(
            "rub-post-commit-journal-error-redact-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let secrets = home.join("secrets.env");
        std::fs::write(&secrets, "RUB_TOKEN=token-123\n").expect("write secrets");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600))
            .expect("set permissions");

        let state = SessionState::new("default", home.clone(), None);
        let request = IpcRequest::new("open", serde_json::json!({}), 1_000);
        let response = rub_ipc::protocol::IpcResponse::error(
            "req-err",
            rub_core::error::ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "token-123 is not allowed",
            )
            .with_context(serde_json::json!({
                "secret": "token-123",
            })),
        );

        state
            .record_post_commit_journal(&request, &response)
            .await
            .expect("journal append succeeds");
        let journal = state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal");

        assert_eq!(journal.len(), 1);
        assert_eq!(
            journal[0]["journal_state"]["reader_contract"],
            serde_json::json!("no_public_api")
        );
        assert_eq!(
            journal[0]["response"]["error"]["message"],
            serde_json::json!("$RUB_TOKEN is not allowed")
        );
        assert_eq!(
            journal[0]["response"]["error"]["context"]["secret"],
            serde_json::json!("$RUB_TOKEN")
        );

        let _ = std::fs::remove_dir_all(home);
    }
}

#[tokio::test]
async fn session_tracks_post_commit_journal_append_failures() {
    let home = std::env::temp_dir().join(format!(
        "rub-post-commit-journal-failure-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create home");

    let state = SessionState::new("default", home.clone(), None);
    let request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        1_000,
    );
    let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));

    state.force_post_commit_journal_failure_once();
    let error = state
        .record_post_commit_journal(&request, &response)
        .await
        .expect_err("forced failure should surface");
    assert!(
        error
            .to_string()
            .contains("forced post-commit journal failure")
    );
    assert_eq!(state.post_commit_journal_failure_count(), 1);
    assert!(
        state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn session_reopens_same_session_id_and_appends_to_existing_journal() {
    let home = std::env::temp_dir().join(format!(
        "rub-post-commit-journal-reopen-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create home");

    let session_id = "sess-fixed";
    let first = SessionState::new_with_id("default", session_id, home.clone(), None);
    let request_one = IpcRequest::new(
        "open",
        serde_json::json!({"url": "https://one.test"}),
        1_000,
    );
    let response_one =
        rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({"ok": true}));
    first
        .record_post_commit_journal(&request_one, &response_one)
        .await
        .expect("first append succeeds");

    let second = SessionState::new_with_id("default", session_id, home.clone(), None);
    let request_two = IpcRequest::new(
        "open",
        serde_json::json!({"url": "https://two.test"}),
        1_000,
    );
    let response_two =
        rub_ipc::protocol::IpcResponse::success("req-2", serde_json::json!({"ok": true}));
    second
        .record_post_commit_journal(&request_two, &response_two)
        .await
        .expect("second append succeeds");

    let journal = second
        .read_post_commit_journal_entries_for_tests()
        .expect("read journal");
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0]["request_id"], serde_json::json!("req-1"));
    assert_eq!(journal[1]["request_id"], serde_json::json!("req-2"));

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn session_ignores_torn_post_commit_journal_tail() {
    let home = std::env::temp_dir().join(format!(
        "rub-post-commit-journal-tail-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create home");

    let state = SessionState::new("default", home.clone(), None);
    let request = IpcRequest::new(
        "open",
        serde_json::json!({"url": "https://example.test"}),
        1_000,
    );
    let response =
        rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({"ok": true}));
    state
        .record_post_commit_journal(&request, &response)
        .await
        .expect("append succeeds");

    let journal_path = crate::rub_paths::RubPaths::new(&home)
        .session_runtime(&state.session_name, &state.session_id)
        .post_commit_journal_path();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&journal_path)
        .expect("open journal");
    use std::io::Write as _;
    file.write_all(b"{\"command\":\"broken\"")
        .expect("append torn tail");
    file.sync_all().expect("sync torn tail");

    let journal = state
        .read_post_commit_journal_entries_for_tests()
        .expect("read journal");
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["request_id"], serde_json::json!("req-1"));

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn repeated_projection_drain_spawns_coalesce_to_one_task() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let _drain_guard = state.post_commit_projection_drain.lock().await;
    let request = IpcRequest::new("open", serde_json::json!({}), 30_000);
    let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));

    state.submit_post_commit_projection(&request, &response);
    state.spawn_post_commit_projection_drain();
    state.spawn_post_commit_projection_drain();
    state.spawn_post_commit_projection_drain();

    assert_eq!(
        state
            .post_commit_projection_drain_spawn_count
            .load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn history_and_workflow_capture_surface_dropped_post_commit_records() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);

    for index in 0..300u64 {
        let request = IpcRequest::new(
            "open",
            serde_json::json!({
                "url": format!("https://example.test/{index}")
            }),
            30_000,
        );
        let response =
            rub_ipc::protocol::IpcResponse::success(format!("req-{index}"), serde_json::json!({}));
        state.submit_post_commit_projection(&request, &response);
    }

    let history = state.command_history(5).await;
    assert!(history.dropped_before_projection > 0);
    assert!(history.dropped_before_retention > 0);

    let capture = state.workflow_capture(5).await;
    assert!(capture.dropped_before_projection > 0);
}

#[tokio::test]
async fn session_replay_claims_wait_until_owner_releases_and_can_be_reclaimed() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-1".to_string()),
        ReplayCommandClaim::Owner
    ));
    let mut waiter = match state.claim_replay_command("cmd-1", "fingerprint-1".to_string()) {
        ReplayCommandClaim::Wait(notify) => notify,
        ReplayCommandClaim::Owner => panic!("second replay claim should wait on the owner"),
        ReplayCommandClaim::Cached(_) => panic!("second replay claim should not hit cache"),
        ReplayCommandClaim::SpentWithoutCachedResponse => {
            panic!("second replay claim should wait before any execution is spent")
        }
        ReplayCommandClaim::Conflict => panic!("matching replay fingerprint should not conflict"),
    };

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let waited = tokio::spawn(async move {
        let _ = ready_tx.send(());
        tokio::time::timeout(std::time::Duration::from_millis(100), async move {
            if *waiter.borrow() == ReplayFenceState::Released {
                return;
            }
            loop {
                waiter
                    .changed()
                    .await
                    .expect("replay waiter should stay open");
                if *waiter.borrow() == ReplayFenceState::Released {
                    return;
                }
            }
        })
        .await
        .is_ok()
    });
    ready_rx.await.expect("waiter readiness should be signaled");
    state.release_replay_command("cmd-1");
    assert!(
        waited.await.expect("waiter task should join"),
        "waiter should be notified when replay owner releases"
    );
    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-1".to_string()),
        ReplayCommandClaim::Owner
    ));
}

#[tokio::test]
async fn session_spent_replay_claim_survives_cached_response_eviction() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state.mark_replay_command_spent("cmd-1", "fingerprint-1");
    state
        .cache_response(
            "cmd-1".to_string(),
            "fingerprint-1".to_string(),
            IpcResponse::success("req-1", serde_json::json!({ "ok": true })),
        )
        .await;

    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-1".to_string()),
        ReplayCommandClaim::Cached(_)
    ));

    state.evict_cached_replay_response_for_test("cmd-1");

    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-1".to_string()),
        ReplayCommandClaim::SpentWithoutCachedResponse
    ));
    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-2".to_string()),
        ReplayCommandClaim::Conflict
    ));
}

#[tokio::test]
async fn session_replay_claim_rejects_conflicting_fingerprint() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-1".to_string()),
        ReplayCommandClaim::Owner
    ));
    assert!(matches!(
        state.claim_replay_command("cmd-1", "fingerprint-2".to_string()),
        ReplayCommandClaim::Conflict
    ));
}

#[tokio::test]
async fn session_records_download_lifecycle_events() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .set_download_runtime(
            0,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-test/downloads".to_string()),
        )
        .await;
    state
        .record_download_started(
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            Some("main-frame".to_string()),
        )
        .await;
    state
        .record_download_progress(
            "guid-1",
            DownloadState::Completed,
            128,
            Some(128),
            Some("/tmp/rub-test/downloads/guid-1".to_string()),
        )
        .await;

    let downloads = state.download_runtime().await;
    assert!(downloads.active_downloads.is_empty());
    assert_eq!(downloads.completed_downloads.len(), 1);
    assert_eq!(
        downloads
            .last_download
            .as_ref()
            .map(|entry| entry.guid.as_str()),
        Some("guid-1")
    );
    assert_eq!(state.download_events_after(0).await.len(), 2);
}

#[tokio::test]
async fn stale_browser_generation_download_events_do_not_pollute_current_projection() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .set_download_runtime(
            1,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-test/downloads-a".to_string()),
        )
        .await;
    state
        .record_download_started_sequenced(
            1,
            1,
            "guid-a".to_string(),
            "https://example.test/a".to_string(),
            "a.txt".to_string(),
            None,
        )
        .await;

    state
        .set_download_runtime(
            2,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-test/downloads-b".to_string()),
        )
        .await;
    state
        .record_download_started_sequenced(
            1,
            2,
            "guid-stale".to_string(),
            "https://example.test/stale".to_string(),
            "stale.txt".to_string(),
            None,
        )
        .await;

    let downloads = state.download_runtime().await;
    assert_eq!(downloads.active_downloads.len(), 0);
    assert_eq!(
        downloads.download_dir.as_deref(),
        Some("/tmp/rub-test/downloads-b")
    );
}

#[tokio::test]
async fn browser_event_quiescence_waits_for_committed_callbacks() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let baseline = state.browser_event_cursor();
    let sequence = state.allocate_browser_event_sequence();
    let delayed = state.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        delayed.record_browser_event_commit(sequence);
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(100),
            Duration::from_millis(5),
        )
        .await;

    assert!(state.committed_browser_event_cursor() >= sequence);
}

#[tokio::test]
async fn browser_event_quiescence_waits_for_late_arriving_callbacks_within_quiet_period() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let baseline = state.browser_event_cursor();
    let delayed = state.clone();
    let (sequence_tx, sequence_rx) = oneshot::channel();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let sequence = delayed.allocate_browser_event_sequence();
        let _ = sequence_tx.send(sequence);
        tokio::time::sleep(Duration::from_millis(10)).await;
        delayed.record_browser_event_commit(sequence);
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(100),
            Duration::from_millis(25),
        )
        .await;

    let sequence = sequence_rx.await.expect("late browser event sequence");
    assert!(state.committed_browser_event_cursor() >= sequence);
}

#[tokio::test]
async fn browser_event_enqueue_failure_commits_dropped_sequence() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let sink = BrowserSessionEventSink::closed_for_test(&state);
    let baseline = state.browser_event_cursor();
    let sequence = state.allocate_browser_event_sequence();

    sink.enqueue(BrowserSessionEvent::DownloadRuntime {
        browser_sequence: sequence,
        generation: 1,
        status: DownloadRuntimeStatus::Active,
        mode: DownloadMode::Managed,
        download_dir: Some("/tmp/rub-test/downloads".to_string()),
        degraded_reason: None,
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .await;

    assert_eq!(state.browser_event_cursor(), sequence);
    assert_eq!(state.committed_browser_event_cursor(), sequence);
}

#[test]
fn browser_event_bounded_progress_ingress_only_applies_to_in_progress_updates() {
    assert!(
        BrowserSessionEvent::DownloadProgress {
            browser_sequence: 1,
            generation: 0,
            guid: "guid-1".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 64,
            total_bytes: Some(128),
            final_path: None,
        }
        .uses_bounded_progress_ingress()
    );
    assert!(
        !BrowserSessionEvent::DownloadProgress {
            browser_sequence: 2,
            generation: 0,
            guid: "guid-1".to_string(),
            state: DownloadState::Completed,
            received_bytes: 128,
            total_bytes: Some(128),
            final_path: Some("/tmp/file.txt".to_string()),
        }
        .uses_bounded_progress_ingress()
    );
}

#[tokio::test]
async fn browser_event_worker_preserves_download_started_before_progress_across_split_ingress() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let sink = BrowserSessionEventSink::with_progress_capacity_for_test(&state, 1);
    let baseline = state.browser_event_cursor();
    let started_sequence = state.allocate_browser_event_sequence();
    sink.enqueue(BrowserSessionEvent::DownloadStarted {
        browser_sequence: started_sequence,
        generation: 1,
        guid: "guid-ordered".to_string(),
        url: "https://example.test/file.txt".to_string(),
        suggested_filename: "file.txt".to_string(),
        frame_id: Some("frame-main".to_string()),
    });
    let progress_sequence = state.allocate_browser_event_sequence();
    sink.enqueue(BrowserSessionEvent::DownloadProgress {
        browser_sequence: progress_sequence,
        generation: 1,
        guid: "guid-ordered".to_string(),
        state: DownloadState::InProgress,
        received_bytes: 128,
        total_bytes: Some(256),
        final_path: None,
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(100),
            Duration::from_millis(5),
        )
        .await;

    let downloads = state.download_runtime().await;
    let entry = downloads
        .active_downloads
        .iter()
        .find(|entry| entry.guid == "guid-ordered")
        .expect("download should be present");
    assert_eq!(entry.url.as_deref(), Some("https://example.test/file.txt"));
    assert_eq!(entry.suggested_filename.as_deref(), Some("file.txt"));
    assert_eq!(entry.frame_id.as_deref(), Some("frame-main"));
    assert_eq!(entry.received_bytes, 128);
    assert_eq!(entry.total_bytes, Some(256));
    assert_eq!(state.committed_browser_event_cursor(), progress_sequence);
}

#[tokio::test]
async fn browser_event_worker_backfills_late_download_started_without_regressing_state() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let sink = BrowserSessionEventSink::with_progress_capacity_for_test(&state, 1);
    let baseline = state.browser_event_cursor();
    let progress_sequence = state.allocate_browser_event_sequence();
    sink.enqueue(BrowserSessionEvent::DownloadProgress {
        browser_sequence: progress_sequence,
        generation: 1,
        guid: "guid-late-start".to_string(),
        state: DownloadState::InProgress,
        received_bytes: 128,
        total_bytes: Some(256),
        final_path: None,
    });
    let started_sequence = state.allocate_browser_event_sequence();
    sink.enqueue(BrowserSessionEvent::DownloadStarted {
        browser_sequence: started_sequence,
        generation: 1,
        guid: "guid-late-start".to_string(),
        url: "https://example.test/file.txt".to_string(),
        suggested_filename: "file.txt".to_string(),
        frame_id: Some("frame-main".to_string()),
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(100),
            Duration::from_millis(5),
        )
        .await;

    let downloads = state.download_runtime().await;
    let entry = downloads
        .active_downloads
        .iter()
        .find(|entry| entry.guid == "guid-late-start")
        .expect("download should be present");
    assert_eq!(entry.state, DownloadState::InProgress);
    assert_eq!(entry.received_bytes, 128);
    assert_eq!(entry.total_bytes, Some(256));
    assert_eq!(entry.url.as_deref(), Some("https://example.test/file.txt"));
    assert_eq!(entry.suggested_filename.as_deref(), Some("file.txt"));
    assert_eq!(entry.frame_id.as_deref(), Some("frame-main"));
    assert_eq!(state.committed_browser_event_cursor(), started_sequence);
}

#[tokio::test]
async fn browser_event_progress_overflow_commits_sequence_and_marks_download_runtime_degraded() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    let sink = BrowserSessionEventSink::saturated_progress_for_test(&state);
    let baseline = state.browser_event_cursor();
    let sequence = state.allocate_browser_event_sequence();

    sink.enqueue(BrowserSessionEvent::DownloadProgress {
        browser_sequence: sequence,
        generation: 7,
        guid: "guid-overflow".to_string(),
        state: DownloadState::InProgress,
        received_bytes: 64,
        total_bytes: Some(128),
        final_path: None,
    });

    state
        .wait_for_browser_event_quiescence_since(
            baseline,
            Duration::from_millis(50),
            Duration::from_millis(1),
        )
        .await;

    assert_eq!(state.browser_event_cursor(), sequence);
    assert_eq!(state.committed_browser_event_cursor(), sequence);
    assert_eq!(state.browser_event_ingress_drop_count(), 1);
    assert_eq!(
        state.download_runtime().await.degraded_reason.as_deref(),
        Some("browser_event_ingress_overflow:download_progress")
    );
    let window = state.download_event_window_after(0, 0).await;
    assert!(!window.authoritative);
    assert_eq!(
        window.degraded_reason.as_deref(),
        Some("browser_event_ingress_overflow:download_progress")
    );
    assert!(window.events.is_empty());
}

#[tokio::test]
async fn bounded_download_window_respects_preexisting_runtime_degradation() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    state
        .mark_download_runtime_degraded(0, "download_behavior_failed")
        .await;
    state
        .record_download_started_sequenced(
            0,
            1,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        )
        .await;

    let window = state
        .download_event_window_between(
            0,
            state.download_cursor().await,
            0,
            state.download_event_ingress_drop_count(),
            Some("download_behavior_failed"),
            Some("download_behavior_failed"),
        )
        .await;

    assert!(!window.authoritative);
    assert_eq!(
        window.degraded_reason.as_deref(),
        Some("download_behavior_failed")
    );
    assert_eq!(window.events.len(), 1);
}

#[tokio::test]
async fn bounded_download_window_reports_timeline_eviction_truthfully() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    for index in 0..140 {
        state
            .record_download_started_sequenced(
                0,
                index + 1,
                format!("guid-{index}"),
                format!("https://example.test/{index}.csv"),
                format!("{index}.csv"),
                None,
            )
            .await;
    }

    let window = state
        .download_event_window_between(
            0,
            state.download_cursor().await,
            0,
            state.download_event_ingress_drop_count(),
            None,
            None,
        )
        .await;

    assert!(!window.authoritative);
    assert_eq!(
        window.degraded_reason.as_deref(),
        Some("download_event_timeline_overflow")
    );
    assert!(!window.events.is_empty());
}

#[test]
fn browser_event_commit_cursor_advances_only_after_contiguous_sequences_land() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let first = state.allocate_browser_event_sequence();
    let second = state.allocate_browser_event_sequence();

    state.record_browser_event_commit(second);
    assert_eq!(state.committed_browser_event_cursor(), 0);

    state.record_browser_event_commit(first);
    assert_eq!(state.committed_browser_event_cursor(), second);
}

#[tokio::test]
async fn session_starts_with_unknown_frame_runtime() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let frame_runtime = state.frame_runtime().await;

    assert_eq!(frame_runtime.status, FrameContextStatus::Unknown);
    assert!(frame_runtime.current_frame.is_none());
    assert!(frame_runtime.primary_frame.is_none());
    assert!(frame_runtime.frame_lineage.is_empty());
    assert!(frame_runtime.degraded_reason.is_none());
}

#[tokio::test]
async fn set_interference_runtime_replaces_projection() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .set_interference_runtime(InterferenceRuntimeInfo {
            mode: InterferenceMode::PublicWebStable,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::UnknownNavigationDrift,
                summary: "unexpected top-level drift".to_string(),
                current_url: Some("https://example.test/interstitial".to_string()),
                primary_url: Some("https://example.test/app".to_string()),
            }),
            active_policies: vec!["observe_only".to_string()],
            ..InterferenceRuntimeInfo::default()
        })
        .await;

    let interference = state.interference_runtime().await;
    assert_eq!(interference.mode, InterferenceMode::PublicWebStable);
    assert_eq!(interference.status, InterferenceRuntimeStatus::Active);
    assert_eq!(
        interference
            .current_interference
            .as_ref()
            .map(|current| current.kind),
        Some(InterferenceKind::UnknownNavigationDrift)
    );
    assert_eq!(interference.active_policies, vec!["observe_only"]);
}

#[tokio::test]
async fn network_rule_registration_assigns_stable_ids_and_syncs_count() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let first = state
        .register_network_rule(NetworkRuleSpec::Block {
            url_pattern: "https://api.example.com/*".to_string(),
        })
        .await;
    let second = state
        .register_network_rule(NetworkRuleSpec::HeaderOverride {
            url_pattern: "https://app.example.com/*".to_string(),
            headers: BTreeMap::from([("x-debug".to_string(), "1".to_string())]),
        })
        .await;

    assert_eq!(first.id, 1);
    assert_eq!(second.id, 2);

    let integration = state.integration_runtime().await;
    assert_eq!(integration.request_rule_count, 2);
    assert_eq!(integration.request_rules.len(), 2);
    assert_eq!(integration.request_rules[0], first);
    assert_eq!(integration.request_rules[1], second);
    assert!(
        integration
            .active_surfaces
            .contains(&IntegrationSurface::RequestRules)
    );
}

#[tokio::test]
async fn set_integration_runtime_resyncs_request_rule_count_to_canonical_rules() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let mut integration = IntegrationRuntimeInfo {
        request_rule_count: 99,
        request_rules: vec![
            state
                .register_network_rule(NetworkRuleSpec::Allow {
                    url_pattern: "https://static.example.com/*".to_string(),
                })
                .await,
        ],
        ..IntegrationRuntimeInfo::default()
    };
    integration.request_rule_count = 99;

    state.set_integration_runtime(integration).await;
    let projected = state.integration_runtime().await;
    assert_eq!(projected.request_rule_count, 1);
    assert_eq!(projected.request_rules.len(), 1);
}

#[tokio::test]
async fn removing_network_rules_updates_projection_count() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let first = state
        .register_network_rule(NetworkRuleSpec::Rewrite {
            url_pattern: "https://api.example.com/*".to_string(),
            target_base: "http://127.0.0.1:3000".to_string(),
        })
        .await;
    let second = state
        .register_network_rule(NetworkRuleSpec::Block {
            url_pattern: "https://cdn.example.com/*".to_string(),
        })
        .await;

    let removed = state.remove_network_rule(first.id).await;
    assert_eq!(removed, Some(first));
    let integration = state.integration_runtime().await;
    assert_eq!(integration.request_rule_count, 1);
    assert_eq!(integration.request_rules, vec![second.clone()]);

    let cleared = state.clear_network_rules().await;
    assert_eq!(cleared, vec![second]);
    let final_projection = state.integration_runtime().await;
    assert_eq!(final_projection.request_rule_count, 0);
    assert!(final_projection.request_rules.is_empty());
}

#[tokio::test]
async fn observatory_projection_tracks_recent_events_and_readiness() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .record_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "boom".to_string(),
            source: Some("app.js".to_string()),
        })
        .await;
    state
        .record_request_summary(RequestSummaryEvent {
            request_id: "req-1".to_string(),
            url: "https://api.example.com/data".to_string(),
            method: "GET".to_string(),
            status: Some(200),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
        })
        .await;

    let observatory = state.observatory().await;
    assert_eq!(observatory.status, RuntimeObservatoryStatus::Active);
    assert_eq!(observatory.recent_console_errors.len(), 1);
    assert_eq!(observatory.recent_requests.len(), 1);

    let integration = state.integration_runtime().await;
    assert!(integration.observatory_ready);
    assert_eq!(integration.status, IntegrationRuntimeStatus::Active);
    assert!(
        integration
            .active_surfaces
            .contains(&IntegrationSurface::RuntimeObservatory)
    );
}

#[tokio::test]
async fn state_inspector_projection_drives_integration_readiness() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .publish_runtime_state_snapshot(
            state.allocate_runtime_state_sequence(),
            RuntimeStateSnapshot {
                state_inspector: StateInspectorInfo {
                    status: StateInspectorStatus::Active,
                    auth_state: AuthState::Authenticated,
                    cookie_count: 2,
                    local_storage_keys: vec!["token".to_string()],
                    session_storage_keys: vec!["csrf".to_string()],
                    auth_signals: vec![
                        "cookies_present".to_string(),
                        "local_storage_present".to_string(),
                        "session_storage_present".to_string(),
                        "auth_like_storage_key_present".to_string(),
                    ],
                    degraded_reason: None,
                },
                readiness_state: ReadinessInfo::default(),
            },
        )
        .await;

    let inspector = state.state_inspector().await;
    assert_eq!(inspector.status, StateInspectorStatus::Active);
    assert_eq!(inspector.auth_state, AuthState::Authenticated);
    assert_eq!(inspector.cookie_count, 2);

    let integration = state.integration_runtime().await;
    assert!(integration.state_inspector_ready);
    assert_eq!(integration.status, IntegrationRuntimeStatus::Active);
    assert!(
        integration
            .active_surfaces
            .contains(&IntegrationSurface::StateInspector)
    );
}

#[tokio::test]
async fn readiness_projection_drives_integration_readiness() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .publish_runtime_state_snapshot(
            state.allocate_runtime_state_sequence(),
            RuntimeStateSnapshot {
                state_inspector: StateInspectorInfo::default(),
                readiness_state: ReadinessInfo {
                    status: ReadinessStatus::Active,
                    route_stability: RouteStability::Stable,
                    loading_present: false,
                    skeleton_present: true,
                    overlay_state: OverlayState::Development,
                    document_ready_state: Some("complete".to_string()),
                    blocking_signals: vec![
                        "skeleton_present".to_string(),
                        "overlay:development".to_string(),
                    ],
                    degraded_reason: None,
                },
            },
        )
        .await;

    let readiness = state.readiness_state().await;
    assert_eq!(readiness.status, ReadinessStatus::Active);
    assert_eq!(readiness.route_stability, RouteStability::Stable);
    assert!(readiness.skeleton_present);
    assert_eq!(readiness.overlay_state, OverlayState::Development);

    let integration = state.integration_runtime().await;
    assert!(integration.readiness_ready);
    assert_eq!(integration.status, IntegrationRuntimeStatus::Active);
    assert!(
        integration
            .active_surfaces
            .contains(&IntegrationSurface::Readiness)
    );
}

#[tokio::test]
async fn degraded_live_surface_drives_integration_runtime_status() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .mark_observatory_degraded("observatory_ingress_overflow")
        .await;

    let integration = state.integration_runtime().await;
    assert_eq!(integration.status, IntegrationRuntimeStatus::Degraded);
    assert!(integration.observatory_ready);
    assert!(
        integration
            .degraded_surfaces
            .contains(&IntegrationSurface::RuntimeObservatory)
    );

    let observatory = state.observatory().await;
    assert_eq!(
        observatory.degraded_reason.as_deref(),
        Some("observatory_ingress_overflow")
    );
}

#[tokio::test]
async fn observatory_projection_reports_ingress_drop_count() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert_eq!(state.record_observatory_ingress_overflow(), 1);
    assert_eq!(state.record_observatory_ingress_overflow(), 2);

    let observatory = state.observatory().await;
    assert_eq!(observatory.dropped_event_count, 2);
}

#[tokio::test]
async fn network_request_authority_is_not_degraded_by_best_effort_observatory_overflow() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert_eq!(state.record_observatory_ingress_overflow(), 1);
    state
        .upsert_network_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("target-1".to_string()),
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: Some("main".to_string()),
            resource_type: Some("xhr".to_string()),
            mime_type: Some("application/json".to_string()),
        })
        .await;

    let window = state.network_request_window_after(0, 0).await;
    assert!(window.authoritative);
    assert_eq!(window.records.len(), 1);
}

#[tokio::test]
async fn network_request_ingress_overflow_fails_request_window_closed() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert_eq!(state.record_network_request_ingress_overflow(), 1);
    state
        .mark_observatory_degraded("network_request_ingress_overflow")
        .await;

    let window = state.network_request_window_after(0, 0).await;
    assert!(!window.authoritative);
    assert_eq!(
        window.degraded_reason.as_deref(),
        Some("network_request_ingress_overflow")
    );
}

#[tokio::test]
async fn snapshot_cache_and_clear_use_consistent_lock_order() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let snapshot = Snapshot {
        snapshot_id: "snap-1".to_string(),
        dom_epoch: 1,
        frame_context: rub_core::model::FrameContextInfo {
            frame_id: "main".to_string(),
            name: Some("main".to_string()),
            parent_frame_id: None,
            target_id: Some("target-1".to_string()),
            url: Some("https://example.com".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        },
        frame_lineage: vec!["main".to_string()],
        url: "https://example.com".to_string(),
        title: "Example".to_string(),
        timestamp: "2026-04-01T00:00:00Z".to_string(),
        elements: Vec::new(),
        total_count: 0,
        truncated: false,
        scroll: rub_core::model::ScrollPosition {
            x: 0.0,
            y: 0.0,
            at_bottom: false,
        },
        projection: rub_core::model::SnapshotProjection {
            verified: true,
            js_traversal_count: 0,
            backend_traversal_count: 0,
            resolved_ref_count: 0,
            warning: None,
        },
        viewport_filtered: None,
        viewport_count: None,
    };

    let cache = async {
        state.cache_snapshot(snapshot).await;
    };
    let clear = async {
        state.clear_all_snapshots().await;
    };
    let read = async {
        let _ = state.get_snapshot("snap-1").await;
    };
    tokio::time::timeout(std::time::Duration::from_millis(200), async {
        tokio::join!(cache, clear, read);
    })
    .await
    .expect("snapshot cache + clear + read should not deadlock");
}

#[tokio::test]
async fn runtime_state_probe_failure_marks_surfaces_degraded() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state
        .mark_runtime_state_probe_degraded(
            state.allocate_runtime_state_sequence(),
            "page_unavailable",
        )
        .await;

    let inspector = state.state_inspector().await;
    assert_eq!(inspector.status, StateInspectorStatus::Degraded);
    assert_eq!(
        inspector.degraded_reason.as_deref(),
        Some("live_probe_failed:page_unavailable")
    );

    let readiness = state.readiness_state().await;
    assert_eq!(readiness.status, ReadinessStatus::Degraded);
    assert_eq!(
        readiness.degraded_reason.as_deref(),
        Some("live_probe_failed:page_unavailable")
    );

    let integration = state.integration_runtime().await;
    assert_eq!(integration.status, IntegrationRuntimeStatus::Degraded);
    assert!(integration.state_inspector_ready);
    assert!(integration.readiness_ready);
    assert!(
        integration
            .degraded_surfaces
            .contains(&IntegrationSurface::StateInspector)
    );
    assert!(
        integration
            .degraded_surfaces
            .contains(&IntegrationSurface::Readiness)
    );
}

#[tokio::test]
async fn stale_runtime_state_sequence_does_not_override_newer_snapshot() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let newer = RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 5,
            local_storage_keys: vec!["token".to_string()],
            session_storage_keys: Vec::new(),
            auth_signals: vec!["cookies_present".to_string()],
            degraded_reason: None,
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: None,
        },
    };
    state.publish_runtime_state_snapshot(2, newer.clone()).await;

    let stale = RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            cookie_count: 1,
            ..StateInspectorInfo::default()
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Transitioning,
            ..ReadinessInfo::default()
        },
    };
    state.publish_runtime_state_snapshot(1, stale).await;

    let snapshot = state.runtime_state_snapshot().await;
    assert_eq!(snapshot, newer);
}

#[tokio::test]
async fn stale_orchestration_runtime_sequence_does_not_override_newer_projection() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let current_session_id = state.session_id.clone();
    let current_session_name = state.session_name.clone();
    let newer_sessions = vec![
        crate::orchestration_runtime::projected_orchestration_session(
            current_session_id.clone(),
            current_session_name.clone(),
            42,
            "/tmp/rub-current.sock".to_string(),
            true,
            "1.0".to_string(),
            None,
        ),
    ];

    state
        .set_orchestration_runtime(2, newer_sessions.clone(), None)
        .await;
    state
        .mark_orchestration_runtime_degraded(1, "stale_registry_error")
        .await;

    let runtime = state.orchestration_runtime().await;
    assert_eq!(runtime.status, OrchestrationRuntimeStatus::Active);
    assert_eq!(runtime.degraded_reason, None);
    assert_eq!(runtime.known_sessions, newer_sessions);
}

#[tokio::test]
async fn degraded_orchestration_runtime_clears_stale_known_sessions() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let current_session_id = state.session_id.clone();
    let current_session_name = state.session_name.clone();
    let sessions = vec![
        crate::orchestration_runtime::projected_orchestration_session(
            current_session_id,
            current_session_name,
            42,
            "/tmp/rub-current.sock".to_string(),
            true,
            "1.0".to_string(),
            None,
        ),
    ];

    state.set_orchestration_runtime(1, sessions, None).await;
    state
        .mark_orchestration_runtime_degraded(2, "registry_read_failed:test")
        .await;

    let runtime = state.orchestration_runtime().await;
    assert_eq!(
        runtime.degraded_reason.as_deref(),
        Some("registry_read_failed:test")
    );
    assert!(!runtime.addressing_supported);
    assert!(!runtime.execution_supported);
    assert!(runtime.known_sessions.is_empty());
    assert_eq!(runtime.session_count, 0);
}

#[tokio::test]
async fn handoff_projection_defaults_to_unavailable_and_drives_readiness_flag() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let initial = state.human_verification_handoff().await;
    assert_eq!(initial.status, HumanVerificationHandoffStatus::Unavailable);
    assert_eq!(
        initial.unavailable_reason.as_deref(),
        Some("not_configured")
    );

    state
        .set_human_verification_handoff(HumanVerificationHandoffInfo {
            status: HumanVerificationHandoffStatus::Available,
            automation_paused: false,
            resume_supported: true,
            unavailable_reason: None,
        })
        .await;

    let handoff = state.human_verification_handoff().await;
    assert_eq!(handoff.status, HumanVerificationHandoffStatus::Available);
    assert!(handoff.resume_supported);

    let integration = state.integration_runtime().await;
    assert!(integration.handoff_ready);
    assert!(
        integration
            .active_surfaces
            .contains(&IntegrationSurface::HumanVerificationHandoff)
    );
}

#[tokio::test]
async fn active_human_control_blocks_idle_for_upgrade() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert!(state.is_idle_for_upgrade().await);

    state
        .set_human_verification_handoff(HumanVerificationHandoffInfo {
            status: HumanVerificationHandoffStatus::Active,
            automation_paused: true,
            resume_supported: true,
            unavailable_reason: None,
        })
        .await;

    assert!(state.has_active_human_control().await);
    assert!(!state.is_idle_for_upgrade().await);
}

#[tokio::test]
async fn active_orchestration_count_ignores_cooldown_and_unavailable_rules() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);

    let active = sample_orchestration_rule(1);
    let mut cooling = sample_orchestration_rule(2);
    cooling.execution_policy.cooldown_until_ms = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_millis() as u64
            + 60_000,
    );
    let mut unavailable = sample_orchestration_rule(3);
    unavailable.unavailable_reason = Some("source_session_missing".to_string());

    state
        .register_orchestration_rule(active)
        .await
        .expect("active rule should register");
    state
        .register_orchestration_rule(cooling)
        .await
        .expect("cooldown rule should register");
    state
        .register_orchestration_rule(unavailable)
        .await
        .expect("unavailable rule should register");

    assert_eq!(state.active_orchestration_count().await, 1);
    assert_eq!(state.resident_orchestration_count().await, 2);
    assert!(state.has_active_orchestrations().await);
}

#[tokio::test]
async fn degraded_request_rules_drive_integration_runtime_status_and_surface() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    let mut integration = IntegrationRuntimeInfo {
        request_rules: vec![rub_core::model::NetworkRule {
            id: 1,
            status: NetworkRuleStatus::Degraded,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://api.example.com/*".to_string(),
            },
        }],
        ..IntegrationRuntimeInfo::default()
    };
    integration.sync_request_rule_count();

    state.set_integration_runtime(integration).await;

    let projected = state.integration_runtime().await;
    assert_eq!(projected.status, IntegrationRuntimeStatus::Degraded);
    assert!(
        projected
            .active_surfaces
            .contains(&IntegrationSurface::RequestRules)
    );
    assert!(
        projected
            .degraded_surfaces
            .contains(&IntegrationSurface::RequestRules)
    );
}

#[test]
fn external_dom_change_respects_transaction_fence() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    assert_eq!(state.observe_external_dom_change(Some("target-1")), Some(1));
    state.in_flight_count.store(1, Ordering::SeqCst);
    assert_eq!(state.observe_external_dom_change(Some("target-1")), None);
    assert_eq!(state.current_epoch(), 1);
    assert!(state.take_pending_external_dom_change());
    assert!(!state.take_pending_external_dom_change());
}

#[test]
fn external_dom_change_tracks_target_scoped_pending_authority() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    state.in_flight_count.store(1, Ordering::SeqCst);
    assert_eq!(state.observe_external_dom_change(Some("target-1")), None);
    assert!(state.pending_external_dom_change_affects_target(Some("target-1")));
    assert!(!state.pending_external_dom_change_affects_target(Some("target-2")));

    state.mark_pending_external_dom_change();
    assert!(state.pending_external_dom_change_affects_target(Some("target-2")));

    let pending_scope = state.take_pending_external_dom_change_scope();
    assert!(pending_scope.affects_target(Some("target-1")));
    assert!(pending_scope.affects_target(Some("target-2")));
    assert!(!state.has_pending_external_dom_change());
}

#[tokio::test]
async fn snapshot_cache_evicts_oldest_entries() {
    let state = SessionState::new("default", PathBuf::from("/tmp/rub-test"), None);
    for index in 0..130 {
        state
            .cache_snapshot(Snapshot {
                snapshot_id: format!("snap-{index}"),
                dom_epoch: index,
                frame_context: rub_core::model::FrameContextInfo {
                    frame_id: "main".to_string(),
                    name: Some("main".to_string()),
                    parent_frame_id: None,
                    target_id: Some("target-1".to_string()),
                    url: Some("https://example.com".to_string()),
                    depth: 0,
                    same_origin_accessible: Some(true),
                },
                frame_lineage: vec!["main".to_string()],
                url: "https://example.com".to_string(),
                title: "Example".to_string(),
                elements: Vec::new(),
                total_count: 0,
                truncated: false,
                scroll: rub_core::model::ScrollPosition {
                    x: 0.0,
                    y: 0.0,
                    at_bottom: false,
                },
                timestamp: "2026-03-29T00:00:00Z".to_string(),
                projection: rub_core::model::SnapshotProjection {
                    verified: true,
                    js_traversal_count: 0,
                    backend_traversal_count: 0,
                    resolved_ref_count: 0,
                    warning: None,
                },
                viewport_filtered: None,
                viewport_count: None,
            })
            .await;
    }

    assert!(state.get_snapshot("snap-0").await.is_none());
    assert!(state.get_snapshot("snap-1").await.is_none());
    assert!(state.get_snapshot("snap-2").await.is_some());
    assert!(state.get_snapshot("snap-129").await.is_some());
}
