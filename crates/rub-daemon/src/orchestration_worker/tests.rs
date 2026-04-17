use super::condition::{
    orchestration_evidence_key, persisted_latched_orchestration_evidence_key,
    reconcile_worker_state, skip_latched_orchestration_evidence,
};
use super::{
    CompletedOrchestrationReservation, OrchestrationNetworkProgress, OrchestrationWorkerEntry,
    PendingOrchestrationConditionPolicy, PendingOrchestrationReservation,
    TriggeredOrchestrationCondition, complete_orchestration_reservation,
    drain_orchestration_reservation_completions, orchestration_rule_semantics_fingerprint,
    process_orchestration_rule, reconcile_pending_orchestration_reservations,
    run_orchestration_cycle,
};
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
use crate::session::SessionState;

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
        11,
        0,
        "source-session",
    );
    let entry = worker_state.get(&7).expect("entry should exist");
    assert_eq!(entry.network_cursor, 11);
    assert!(entry.network_cursor_primed);
    assert_eq!(entry.latched_evidence_key, None);
}

#[test]
fn reconcile_worker_state_leaves_remote_network_rules_unprimed_until_remote_cursor_is_read() {
    let mut worker_state = HashMap::new();
    let mut remote_rule = rule(8, OrchestrationRuleStatus::Armed);
    remote_rule.condition.kind = TriggerConditionKind::NetworkRequest;

    reconcile_worker_state(&mut worker_state, &[remote_rule], 17, 0, "current-session");

    let entry = worker_state.get(&8).expect("entry should exist");
    assert_eq!(entry.network_cursor, 0);
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
    });

    reconcile_worker_state(&mut worker_state, &[repeat_rule], 17, 0, "source-session");

    let entry = worker_state.get(&10).expect("entry should exist");
    assert_eq!(
        entry.latched_evidence_key.as_deref(),
        Some("source_tab_text_present:Ready::Ready")
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
