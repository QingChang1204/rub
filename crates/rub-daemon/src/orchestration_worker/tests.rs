use super::condition::{
    load_orchestration_condition_state, orchestration_evidence_key,
    persisted_latched_orchestration_evidence_key, reconcile_worker_state,
    record_orchestration_probe_failure, skip_latched_orchestration_evidence,
};
use super::{
    CompletedOrchestrationReservation, OrchestrationNetworkProgress, OrchestrationWorkerEntry,
    PendingOrchestrationConditionPolicy, PendingOrchestrationReservation,
    ReservedOrchestrationExecution, TriggeredOrchestrationCondition,
    commit_orchestration_execution, complete_orchestration_reservation,
    drain_orchestration_reservation_completions, orchestration_rule_semantics_fingerprint,
    orchestration_worker_command_identity_key, process_orchestration_rule,
    reconcile_pending_orchestration_reservations, run_orchestration_cycle,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
    OrchestrationRuleInfo, OrchestrationRuleStatus, TriggerConditionKind, TriggerConditionSpec,
    TriggerEvidenceInfo,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use uuid::Uuid;

use crate::router::DaemonRouter;
use crate::rub_paths::RubPaths;
use crate::session::{NetworkRequestBaseline, RegistryEntry, SessionState, write_registry};
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;

fn test_router() -> Arc<DaemonRouter> {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        },
    ));
    let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        rub_cdp::humanize::HumanizeConfig {
            enabled: false,
            speed: rub_cdp::humanize::HumanizeSpeed::Normal,
        },
    ));
    Arc::new(DaemonRouter::new(adapter))
}

fn temp_home(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rub-orchestration-worker-{label}-{}",
        Uuid::now_v7()
    ))
}

fn rule(id: u32, status: OrchestrationRuleStatus) -> OrchestrationRuleInfo {
    OrchestrationRuleInfo {
        id,
        status,
        lifecycle_generation: 1,
        source: OrchestrationAddressInfo {
            session_id: "source-session".to_string(),
            session_name: "source".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("source-tab".to_string()),
            frame_id: None,
        },
        target: OrchestrationAddressInfo {
            session_id: "target-session".to_string(),
            session_name: "target".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("target-tab".to_string()),
            frame_id: None,
        },
        mode: OrchestrationMode::Repeat,
        execution_policy: OrchestrationExecutionPolicyInfo {
            cooldown_ms: 1000,
            max_retries: 0,
            cooldown_until_ms: None,
        },
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
        actions: vec![rub_core::model::TriggerActionSpec {
            kind: rub_core::model::TriggerActionKind::BrowserCommand,
            command: Some("click".to_string()),
            payload: Some(json!({"selector":"#apply"})),
        }],
        correlation_key: "corr".to_string(),
        idempotency_key: format!("idem-{id}"),
        unavailable_reason: None,
        last_condition_evidence: None,
        last_result: None,
    }
}

#[test]
fn orchestration_evidence_key_prefers_fingerprint_when_present() {
    let evidence = TriggerEvidenceInfo {
        summary: "source_tab_text_present:Ready".to_string(),
        fingerprint: Some("doc-1".to_string()),
    };
    assert_eq!(
        orchestration_evidence_key(&evidence),
        "source_tab_text_present:Ready::doc-1"
    );
}

#[test]
fn worker_command_identity_uses_revalidated_evidence_key_for_revalidated_conditions() {
    assert_eq!(
        orchestration_worker_command_identity_key(true, "source_tab_text_present:Ready::Ready"),
        Some("source_tab_text_present:Ready::Ready")
    );
}

#[test]
fn worker_command_identity_uses_evidence_key_for_preserved_network_conditions() {
    assert_eq!(
        orchestration_worker_command_identity_key(false, "network_request:/api/save::42"),
        Some("network_request:/api/save::42")
    );
}

#[test]
fn reconcile_worker_state_clears_latched_evidence_when_rule_rearms() {
    let mut worker_state = HashMap::from([(
        7,
        OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Blocked,
            network_cursor: 4,
            network_cursor_primed: true,
            observatory_drop_count: 0,
            latched_evidence_key: Some("source_tab_text_present:Ready".to_string()),
        },
    )]);
    reconcile_worker_state(
        &mut worker_state,
        &[rule(7, OrchestrationRuleStatus::Armed)],
        &HashMap::new(),
    );
    let entry = worker_state.get(&7).expect("entry should exist");
    assert_eq!(entry.network_cursor, 4);
    assert!(entry.network_cursor_primed);
    assert_eq!(entry.latched_evidence_key, None);
}

#[test]
fn reconcile_worker_state_seeds_remote_network_rules_from_committed_baseline() {
    let mut worker_state = HashMap::new();
    let mut remote_rule = rule(8, OrchestrationRuleStatus::Armed);
    remote_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    let committed_baselines = HashMap::from([(
        8_u32,
        NetworkRequestBaseline {
            cursor: 17,
            observed_ingress_drop_count: 3,
            primed: true,
        },
    )]);

    reconcile_worker_state(&mut worker_state, &[remote_rule], &committed_baselines);

    let entry = worker_state.get(&8).expect("entry should exist");
    assert_eq!(entry.network_cursor, 17);
    assert_eq!(entry.observatory_drop_count, 3);
    assert!(entry.network_cursor_primed);
}

#[test]
fn reconcile_worker_state_leaves_network_request_rules_unprimed_without_committed_baseline() {
    let mut worker_state = HashMap::new();
    let mut local_rule = rule(11, OrchestrationRuleStatus::Armed);
    local_rule.source.session_id = "current-session".to_string();
    local_rule.condition.kind = TriggerConditionKind::NetworkRequest;

    reconcile_worker_state(&mut worker_state, &[local_rule], &HashMap::new());

    let entry = worker_state.get(&11).expect("entry should exist");
    assert_eq!(entry.network_cursor, 0);
    assert_eq!(entry.observatory_drop_count, 0);
    assert!(!entry.network_cursor_primed);
}

#[test]
fn reconcile_worker_state_clears_orchestration_network_request_priming_when_baseline_disappears() {
    let mut local_rule = rule(12, OrchestrationRuleStatus::Armed);
    local_rule.source.session_id = "current-session".to_string();
    local_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    let mut worker_state = HashMap::from([(
        12_u32,
        OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 17,
            network_cursor_primed: true,
            observatory_drop_count: 5,
            latched_evidence_key: None,
        },
    )]);

    reconcile_worker_state(&mut worker_state, &[local_rule], &HashMap::new());

    let entry = worker_state.get(&12).expect("entry should exist");
    assert_eq!(entry.network_cursor, 17);
    assert_eq!(entry.observatory_drop_count, 5);
    assert!(!entry.network_cursor_primed);
}

#[test]
fn latched_evidence_still_commits_network_progress() {
    let mut worker = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 4,
        network_cursor_primed: true,
        observatory_drop_count: 1,
        latched_evidence_key: Some("same-evidence".to_string()),
    };

    assert!(skip_latched_orchestration_evidence(
        &mut worker,
        "same-evidence",
        Some(OrchestrationNetworkProgress {
            next_cursor: 9,
            observed_drop_count: 3,
        })
    ));
    assert_eq!(worker.network_cursor, 9);
    assert_eq!(worker.observatory_drop_count, 3);
    assert!(worker.network_cursor_primed);
}

#[test]
fn persisted_repeat_evidence_latch_survives_manual_cooldown_projection() {
    let mut repeat_rule = rule(9, OrchestrationRuleStatus::Armed);
    repeat_rule.last_condition_evidence = Some(TriggerEvidenceInfo {
        summary: "source_tab_text_present:Ready".to_string(),
        fingerprint: Some("Ready".to_string()),
    });
    repeat_rule.last_result = Some(rub_core::model::OrchestrationResultInfo {
        rule_id: 9,
        status: OrchestrationRuleStatus::Blocked,
        next_status: OrchestrationRuleStatus::Armed,
        summary: "orchestration cooldown active".to_string(),
        committed_steps: 0,
        total_steps: 1,
        steps: Vec::new(),
        cooldown_until_ms: Some(1234),
        error_code: None,
        reason: Some("orchestration_cooldown_active".to_string()),
        error_context: None,
    });

    assert_eq!(
        persisted_latched_orchestration_evidence_key(&repeat_rule),
        Some("source_tab_text_present:Ready::Ready".to_string())
    );
}

#[test]
fn reconcile_worker_state_seeds_latch_from_persisted_repeat_evidence() {
    let mut worker_state = HashMap::new();
    let mut repeat_rule = rule(10, OrchestrationRuleStatus::Armed);
    repeat_rule.last_condition_evidence = Some(TriggerEvidenceInfo {
        summary: "source_tab_text_present:Ready".to_string(),
        fingerprint: Some("Ready".to_string()),
    });
    repeat_rule.last_result = Some(rub_core::model::OrchestrationResultInfo {
        rule_id: 10,
        status: OrchestrationRuleStatus::Fired,
        next_status: OrchestrationRuleStatus::Armed,
        summary: "repeat orchestration rule 10 committed 1/1 action(s)".to_string(),
        committed_steps: 1,
        total_steps: 1,
        steps: Vec::new(),
        cooldown_until_ms: Some(1234),
        error_code: None,
        reason: None,
        error_context: None,
    });

    reconcile_worker_state(&mut worker_state, &[repeat_rule], &HashMap::new());

    let entry = worker_state.get(&10).expect("entry should exist");
    assert_eq!(
        entry.latched_evidence_key.as_deref(),
        Some("source_tab_text_present:Ready::Ready")
    );
}

#[tokio::test]
async fn commit_orchestration_execution_does_not_latch_uncommitted_first_step_failure() {
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("orchestration-no-latch-on-step-zero-failure"),
        None,
    ));
    let mut rule_info = rule(13, OrchestrationRuleStatus::Armed);
    rule_info.mode = OrchestrationMode::Repeat;
    let live_rule = state
        .register_orchestration_rule(rule_info)
        .await
        .expect("rule should register");
    let evidence = TriggerEvidenceInfo {
        summary: "source_tab_text_present:Ready".to_string(),
        fingerprint: Some("Ready".to_string()),
    };
    let reserved = ReservedOrchestrationExecution {
        runtime: state.orchestration_runtime().await,
        rule: live_rule,
        evidence,
        evidence_key: "source_tab_text_present:Ready::Ready".to_string(),
        network_progress: None,
        _transaction: None,
    };
    let mut worker = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: false,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };

    commit_orchestration_execution(
        &state,
        &mut worker,
        reserved,
        rub_core::model::OrchestrationResultInfo {
            rule_id: 13,
            status: OrchestrationRuleStatus::Blocked,
            next_status: OrchestrationRuleStatus::Armed,
            summary: "first step blocked before any action committed".to_string(),
            committed_steps: 0,
            total_steps: 1,
            steps: Vec::new(),
            cooldown_until_ms: None,
            error_code: Some(ErrorCode::AutomationPaused),
            reason: Some("orchestration_worker_handoff_blocked".to_string()),
            error_context: None,
        },
    )
    .await;

    assert_eq!(
        worker.latched_evidence_key, None,
        "step-zero transient failures must not latch evidence ahead of durable truth"
    );
}

#[tokio::test]
async fn load_orchestration_condition_state_fails_closed_when_committed_baseline_is_missing() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "source-session",
        temp_home("orchestration-network-baseline-missing"),
        None,
    ));
    let mut network_rule = rule(12, OrchestrationRuleStatus::Armed);
    network_rule.source.session_id = "source-session".to_string();
    network_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    let live_rule = state
        .register_orchestration_rule(network_rule)
        .await
        .expect("rule should register");
    let mut worker = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: false,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };

    let error =
        match load_orchestration_condition_state(&router, &state, &live_rule, &mut worker).await {
            Err(error) => error,
            Ok(_) => panic!("missing committed baseline must fail orchestration evidence closed"),
        };

    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("orchestration_network_request_baseline_missing")
    );
}

#[tokio::test]
async fn orchestration_cycle_uses_queue_authority_even_with_foreground_in_flight() {
    let router = test_router();
    let state = Arc::new(SessionState::new("default", temp_home("fairness"), None));
    state
        .in_flight_count
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let mut worker_state = HashMap::new();
    let mut pending_reservations = HashMap::new();
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
    let mut next_reservation_attempt_id = 0_u64;

    run_orchestration_cycle(
        &router,
        &state,
        &mut worker_state,
        &mut pending_reservations,
        &mut reservation_rx,
        &reservation_tx,
        &mut next_reservation_attempt_id,
    )
    .await;

    let metrics = state.automation_scheduler_metrics().await;
    assert_eq!(
        metrics["orchestration_worker"]["metrics"]["cycle_count"],
        json!(1)
    );
    assert_eq!(
        metrics["authority_inventory"]["orchestration_worker_pre_queue_gate"],
        json!("none")
    );
}

#[tokio::test]
async fn ready_orchestration_reservation_completion_releases_idle_queue_permit() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-completion-release"),
        None,
    ));
    let reserved = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("queued orchestration reservation should acquire immediately in test");
    let mut worker_state = HashMap::new();
    let mut pending_reservations = HashMap::from([(
        7_u32,
        PendingOrchestrationReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingOrchestrationConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: true,
                rule_semantics_fingerprint: String::new(),
                rule_lifecycle_generation: 1,
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
    reservation_tx
        .send(CompletedOrchestrationReservation {
            rule_id: 7,
            attempt_id: 1,
            result: Ok(reserved),
        })
        .expect("reservation completion should enqueue");

    drain_orchestration_reservation_completions(
        &router,
        &state,
        &mut worker_state,
        &mut pending_reservations,
        &mut reservation_rx,
    )
    .await;

    assert!(pending_reservations.is_empty());
    let foreground = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        router.begin_automation_transaction_with_wait_budget(
            &state,
            "foreground_after_completion",
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(5),
        ),
    )
    .await
    .expect("foreground request should not remain blocked behind drained completion")
    .expect("foreground request should acquire after drained completion");
    drop(foreground);
}

#[tokio::test]
async fn pending_network_request_orchestration_is_not_re_evaluated_during_queue_wait() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("pending-network-request"),
        None,
    ));
    let mut worker_entry = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: true,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };
    let mut network_rule = rule(7, OrchestrationRuleStatus::Armed);
    network_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    let mut pending_reservations = HashMap::from([(
        7_u32,
        PendingOrchestrationReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingOrchestrationConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&network_rule),
                rule_lifecycle_generation: network_rule.lifecycle_generation,
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, _reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
    let mut next_reservation_attempt_id = 0_u64;

    process_orchestration_rule(
        &router,
        &state,
        network_rule.clone(),
        &mut worker_entry,
        &mut pending_reservations,
        &reservation_tx,
        &mut next_reservation_attempt_id,
    )
    .await;

    assert!(pending_reservations.contains_key(&network_rule.id));
    assert_eq!(next_reservation_attempt_id, 0);
}

#[tokio::test]
async fn reconcile_pending_network_request_orchestration_drops_semantics_drift() {
    let mut stale_rule = rule(7, OrchestrationRuleStatus::Armed);
    stale_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    stale_rule.condition.url_pattern = Some("/old".to_string());
    let mut live_rule = stale_rule.clone();
    live_rule.condition.url_pattern = Some("/new".to_string());

    let mut pending_reservations = HashMap::from([(
        live_rule.id,
        PendingOrchestrationReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingOrchestrationConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&stale_rule),
                rule_lifecycle_generation: stale_rule.lifecycle_generation,
            },
            task: tokio::spawn(async {}),
        },
    )]);

    reconcile_pending_orchestration_reservations(&[live_rule], &mut pending_reservations);

    assert!(pending_reservations.is_empty());
}

#[tokio::test]
async fn complete_network_request_orchestration_reservation_fails_closed_on_semantics_drift() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-semantics-drift"),
        None,
    ));
    let mut live_rule = rule(7, OrchestrationRuleStatus::Armed);
    live_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    live_rule.condition.url_pattern = Some("/new".to_string());
    let live_rule = state
        .register_orchestration_rule(live_rule)
        .await
        .expect("rule should register");
    let transaction = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("reservation should acquire");
    let mut worker_entry = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: true,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };
    let mut stale_rule = live_rule.clone();
    stale_rule.condition.url_pattern = Some("/old".to_string());

    let reserved = complete_orchestration_reservation(
        &router,
        &state,
        live_rule.id,
        &mut worker_entry,
        transaction,
        None,
        PendingOrchestrationConditionPolicy {
            preserved_triggered: Some(TriggeredOrchestrationCondition {
                evidence: TriggerEvidenceInfo {
                    summary: "network_request_matched:req-1".to_string(),
                    fingerprint: Some("req-1".to_string()),
                },
                evidence_key: "network_request_matched:req-1::req-1".to_string(),
                network_progress: None,
            }),
            requires_revalidation_after_queue: false,
            rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&stale_rule),
            rule_lifecycle_generation: stale_rule.lifecycle_generation,
        },
    )
    .await
    .expect("reservation completion should fail closed, not error");

    assert!(reserved.is_none());
}

#[tokio::test]
async fn complete_orchestration_reservation_fails_closed_on_lifecycle_generation_drift() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-generation-drift"),
        None,
    ));
    let live_rule = state
        .register_orchestration_rule(rule(7, OrchestrationRuleStatus::Armed))
        .await
        .expect("rule should register");
    state
        .set_orchestration_rule_status(live_rule.id, OrchestrationRuleStatus::Paused)
        .await
        .expect("pause should update rule");
    state
        .set_orchestration_rule_status(live_rule.id, OrchestrationRuleStatus::Armed)
        .await
        .expect("resume should update rule");

    let transaction = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("reservation should acquire");
    let mut worker_entry = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: true,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };

    let reserved = complete_orchestration_reservation(
        &router,
        &state,
        live_rule.id,
        &mut worker_entry,
        transaction,
        None,
        PendingOrchestrationConditionPolicy {
            preserved_triggered: Some(TriggeredOrchestrationCondition {
                evidence: TriggerEvidenceInfo {
                    summary: "source_tab_text_present:Ready".to_string(),
                    fingerprint: Some("Ready".to_string()),
                },
                evidence_key: "source_tab_text_present:Ready::Ready".to_string(),
                network_progress: None,
            }),
            requires_revalidation_after_queue: false,
            rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&live_rule),
            rule_lifecycle_generation: live_rule.lifecycle_generation,
        },
    )
    .await
    .expect("reservation completion should fail closed, not error");

    assert!(reserved.is_none());
}

#[tokio::test]
async fn probe_failure_preserves_outcome_truth_across_lifecycle_generation_drift() {
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("probe-failure-generation-drift"),
        None,
    ));
    let stale_rule = state
        .register_orchestration_rule(rule(7, OrchestrationRuleStatus::Armed))
        .await
        .expect("rule should register");
    state
        .set_orchestration_rule_status(stale_rule.id, OrchestrationRuleStatus::Paused)
        .await
        .expect("pause should update rule");
    state
        .set_orchestration_rule_status(stale_rule.id, OrchestrationRuleStatus::Armed)
        .await
        .expect("resume should update rule");

    record_orchestration_probe_failure(
        &state,
        &stale_rule,
        ErrorEnvelope::new(
            ErrorCode::SessionBusy,
            "source observatory baseline is missing",
        )
        .with_context(json!({
            "reason": "orchestration_network_request_baseline_missing",
        })),
    )
    .await;

    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .into_iter()
        .find(|candidate| candidate.id == stale_rule.id)
        .expect("rule should remain present");
    assert_eq!(
        rule.last_result
            .as_ref()
            .and_then(|result| result.reason.as_deref()),
        Some("orchestration_network_request_baseline_missing")
    );
    assert_eq!(
        runtime
            .last_rule_result
            .as_ref()
            .and_then(|result| result.reason.as_deref()),
        Some("orchestration_network_request_baseline_missing")
    );
    let trace = state.orchestration_trace(8).await;
    assert!(
        trace.events.iter().any(|event| {
            event.kind == rub_core::model::OrchestrationEventKind::Degraded
                && event.reason.as_deref() == Some("orchestration_lifecycle_generation_stale")
        }),
        "{trace:?}"
    );
}

#[tokio::test]
async fn remote_orchestration_reservation_retains_active_execution_fence_until_commit() {
    let router = test_router();
    let home = temp_home("remote-execution-fence");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let source_runtime = RubPaths::new(&home).session_runtime("source", "source-session");
    let target_runtime = RubPaths::new(&home).session_runtime("target", "target-session");
    let source_projection = RubPaths::new(&home).session("source");
    let target_projection = RubPaths::new(&home).session("target");
    std::fs::create_dir_all(source_runtime.session_dir()).expect("create source runtime dir");
    std::fs::create_dir_all(target_runtime.session_dir()).expect("create target runtime dir");
    std::fs::create_dir_all(source_projection.projection_dir())
        .expect("create source projection dir");
    std::fs::create_dir_all(target_projection.projection_dir())
        .expect("create target projection dir");
    std::fs::write(source_runtime.pid_path(), std::process::id().to_string())
        .expect("write source pid");
    std::fs::write(target_runtime.pid_path(), std::process::id().to_string())
        .expect("write target pid");
    std::fs::write(source_projection.startup_committed_path(), "source-session")
        .expect("write source committed marker");
    std::fs::write(target_projection.startup_committed_path(), "target-session")
        .expect("write target committed marker");
    if let Some(parent) = source_runtime.socket_path().parent() {
        std::fs::create_dir_all(parent).expect("create source socket parent");
    }
    if let Some(parent) = target_runtime.socket_path().parent() {
        std::fs::create_dir_all(parent).expect("create target socket parent");
    }
    std::fs::write(source_runtime.socket_path(), b"socket").expect("write source socket");
    std::fs::write(target_runtime.socket_path(), b"socket").expect("write target socket");
    crate::session::force_live_registry_socket_probe_once_for_test(&source_runtime.socket_path());
    crate::session::force_live_registry_socket_probe_once_for_test(&target_runtime.socket_path());
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "source-session".to_string(),
                    session_name: "source".to_string(),
                    pid: std::process::id(),
                    socket_path: source_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-20T00:00:00Z".to_string(),
                    ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "target-session".to_string(),
                    session_name: "target".to_string(),
                    pid: std::process::id(),
                    socket_path: target_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-20T00:00:01Z".to_string(),
                    ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .expect("write registry");
    let mut live_rule = rule(7, OrchestrationRuleStatus::Armed);
    live_rule.condition.kind = TriggerConditionKind::NetworkRequest;
    let live_rule = state
        .register_orchestration_rule(live_rule)
        .await
        .expect("rule should register");
    let transaction = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("reservation should acquire");
    let mut worker_entry = OrchestrationWorkerEntry {
        last_status: OrchestrationRuleStatus::Armed,
        network_cursor: 0,
        network_cursor_primed: true,
        observatory_drop_count: 0,
        latched_evidence_key: None,
    };

    let reserved = complete_orchestration_reservation(
        &router,
        &state,
        live_rule.id,
        &mut worker_entry,
        transaction,
        None,
        PendingOrchestrationConditionPolicy {
            preserved_triggered: Some(TriggeredOrchestrationCondition {
                evidence: TriggerEvidenceInfo {
                    summary: "network_request_matched:req-1".to_string(),
                    fingerprint: Some("req-1".to_string()),
                },
                evidence_key: "network_request_matched:req-1::req-1".to_string(),
                network_progress: None,
            }),
            requires_revalidation_after_queue: false,
            rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&live_rule),
            rule_lifecycle_generation: live_rule.lifecycle_generation,
        },
    )
    .await
    .expect("reservation completion should succeed")
    .expect("reservation should remain live");

    let blocked = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        router.begin_automation_transaction_with_wait_budget(
            &state,
            "foreground_behind_remote_orchestration",
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(5),
        ),
    )
    .await
    .expect("queue attempt should complete");
    let blocked = match blocked {
        Ok(_) => panic!("remote orchestration reservation must retain the execution fence"),
        Err(error) => error,
    };
    assert_eq!(blocked.code, rub_core::error::ErrorCode::IpcTimeout);

    drop(reserved);

    let foreground = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        router.begin_automation_transaction_with_wait_budget(
            &state,
            "foreground_after_remote_orchestration_release",
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(5),
        ),
    )
    .await
    .expect("foreground request should complete")
    .expect("foreground request should acquire after remote reservation drops");
    drop(foreground);
}

#[tokio::test]
async fn shutdown_orchestration_reservation_completion_drops_owned_transaction_without_executing() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-completion-shutdown"),
        None,
    ));
    let mut rule_info = rule(7, OrchestrationRuleStatus::Armed);
    rule_info.condition.kind = TriggerConditionKind::NetworkRequest;
    rule_info.source.session_id = state.session_id.clone();
    rule_info.source.session_name = state.session_name.clone();
    rule_info.target.session_id = state.session_id.clone();
    rule_info.target.session_name = state.session_name.clone();
    let live_rule = state
        .register_orchestration_rule(rule_info)
        .await
        .expect("rule should register");
    let reserved = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("queued orchestration reservation should acquire immediately in test");
    let mut worker_state = std::collections::HashMap::from([(
        live_rule.id,
        OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 0,
            network_cursor_primed: false,
            observatory_drop_count: 0,
            latched_evidence_key: None,
        },
    )]);
    let mut pending_reservations = std::collections::HashMap::from([(
        live_rule.id,
        PendingOrchestrationReservation {
            attempt_id: 1,
            fallback_network_progress: Some(OrchestrationNetworkProgress {
                next_cursor: 9,
                observed_drop_count: 2,
            }),
            condition_policy: PendingOrchestrationConditionPolicy {
                preserved_triggered: Some(TriggeredOrchestrationCondition {
                    evidence: TriggerEvidenceInfo {
                        summary: "network_request_matched:req-1".to_string(),
                        fingerprint: Some("req-1".to_string()),
                    },
                    evidence_key: "network_request_matched:req-1::req-1".to_string(),
                    network_progress: None,
                }),
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&live_rule),
                rule_lifecycle_generation: live_rule.lifecycle_generation,
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
    state.request_shutdown();
    reservation_tx
        .send(CompletedOrchestrationReservation {
            rule_id: live_rule.id,
            attempt_id: 1,
            result: Ok(reserved),
        })
        .expect("reservation completion should enqueue");

    drain_orchestration_reservation_completions(
        &router,
        &state,
        &mut worker_state,
        &mut pending_reservations,
        &mut reservation_rx,
    )
    .await;

    assert!(pending_reservations.is_empty());
    let worker = worker_state
        .get(&live_rule.id)
        .expect("worker entry should remain present");
    assert_eq!(worker.network_cursor, 9);
    assert_eq!(worker.observatory_drop_count, 2);
    assert!(worker.network_cursor_primed);
    assert_eq!(worker.latched_evidence_key, None);
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .into_iter()
        .find(|candidate| candidate.id == live_rule.id)
        .expect("rule should still exist");
    assert_eq!(rule.last_result, None);
    assert_eq!(
        state
            .in_flight_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn handoff_orchestration_reservation_completion_blocks_execution_after_queue() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-completion-handoff"),
        None,
    ));
    let mut rule_info = rule(7, OrchestrationRuleStatus::Armed);
    rule_info.condition.kind = TriggerConditionKind::NetworkRequest;
    rule_info.source.session_id = state.session_id.clone();
    rule_info.source.session_name = state.session_name.clone();
    rule_info.target.session_id = state.session_id.clone();
    rule_info.target.session_name = state.session_name.clone();
    let live_rule = state
        .register_orchestration_rule(rule_info)
        .await
        .expect("rule should register");
    let reserved = router
        .begin_automation_reservation_transaction_owned(&state, "queued_orchestration")
        .await
        .expect("queued orchestration reservation should acquire immediately in test");
    let mut worker_state = std::collections::HashMap::from([(
        live_rule.id,
        OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 0,
            network_cursor_primed: false,
            observatory_drop_count: 0,
            latched_evidence_key: None,
        },
    )]);
    let mut pending_reservations = std::collections::HashMap::from([(
        live_rule.id,
        PendingOrchestrationReservation {
            attempt_id: 1,
            fallback_network_progress: Some(OrchestrationNetworkProgress {
                next_cursor: 9,
                observed_drop_count: 2,
            }),
            condition_policy: PendingOrchestrationConditionPolicy {
                preserved_triggered: Some(TriggeredOrchestrationCondition {
                    evidence: TriggerEvidenceInfo {
                        summary: "network_request_matched:req-1".to_string(),
                        fingerprint: Some("req-1".to_string()),
                    },
                    evidence_key: "network_request_matched:req-1::req-1".to_string(),
                    network_progress: None,
                }),
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&live_rule),
                rule_lifecycle_generation: live_rule.lifecycle_generation,
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
    state.set_handoff_available(true).await;
    state.activate_handoff().await;
    reservation_tx
        .send(CompletedOrchestrationReservation {
            rule_id: live_rule.id,
            attempt_id: 1,
            result: Ok(reserved),
        })
        .expect("reservation completion should enqueue");

    drain_orchestration_reservation_completions(
        &router,
        &state,
        &mut worker_state,
        &mut pending_reservations,
        &mut reservation_rx,
    )
    .await;

    assert!(pending_reservations.is_empty());
    let worker = worker_state
        .get(&live_rule.id)
        .expect("worker entry should remain present");
    assert_eq!(worker.network_cursor, 9);
    assert_eq!(worker.observatory_drop_count, 2);
    assert!(worker.network_cursor_primed);
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .into_iter()
        .find(|candidate| candidate.id == live_rule.id)
        .expect("rule should still exist");
    assert_eq!(
        rule.last_result
            .as_ref()
            .and_then(|result| result.error_code),
        Some(ErrorCode::AutomationPaused)
    );
    assert_eq!(
        rule.last_condition_evidence
            .as_ref()
            .and_then(|evidence| evidence.fingerprint.as_deref()),
        Some("req-1")
    );
    assert_eq!(
        state
            .in_flight_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}
