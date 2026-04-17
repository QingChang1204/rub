use super::condition::{network_request_matches, readiness_matches, storage_snapshot_matches};
use super::outcome::classify_trigger_error_status;
use super::{
    PendingTriggerConditionPolicy, PendingTriggerReservation, TriggerReservationCompletion,
    TriggerReservationCoordinator, TriggerWorkerEntry, drain_trigger_reservation_completions,
    process_trigger_rule, reconcile_pending_trigger_reservations, resolve_trigger_workflow_spec,
    run_trigger_cycle, trigger_action_execution_info, trigger_action_summary,
    trigger_rule_semantics_fingerprint, trigger_target_continuity_failure,
};
use rub_core::error::ErrorCode;
use rub_core::locator::CanonicalLocator;
use rub_core::model::{
    FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, NetworkRequestLifecycle,
    NetworkRequestRecord, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
    TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec, TriggerInfo,
    TriggerMode, TriggerStatus, TriggerTabBindingInfo,
};
use rub_core::storage::{StorageArea, StorageSnapshot};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
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
    std::env::temp_dir().join(format!("rub-trigger-worker-{label}-{}", Uuid::now_v7()))
}

fn trigger(kind: TriggerConditionKind) -> TriggerInfo {
    TriggerInfo {
        id: 1,
        status: TriggerStatus::Armed,
        mode: TriggerMode::Once,
        source_tab: TriggerTabBindingInfo {
            index: 0,
            target_id: "source".to_string(),
            frame_id: None,
            url: "https://source.example".to_string(),
            title: "Source".to_string(),
        },
        target_tab: TriggerTabBindingInfo {
            index: 1,
            target_id: "target".to_string(),
            frame_id: None,
            url: "https://target.example".to_string(),
            title: "Target".to_string(),
        },
        condition: TriggerConditionSpec {
            kind,
            locator: Some(CanonicalLocator::Selector {
                css: "#ready".to_string(),
                selection: None,
            }),
            text: Some("Approved".to_string()),
            url_pattern: Some("/events".to_string()),
            readiness_state: Some("stable".to_string()),
            method: Some("POST".to_string()),
            status_code: Some(200),
            storage_area: Some(StorageArea::Local),
            key: Some("token".to_string()),
            value: Some("abc".to_string()),
        },
        action: TriggerActionSpec {
            kind: TriggerActionKind::BrowserCommand,
            command: Some("click".to_string()),
            payload: Some(json!({ "selector": "#continue" })),
        },
        last_condition_evidence: None,
        consumed_evidence_fingerprint: None,
        last_action_result: None,
        unavailable_reason: None,
    }
}

#[test]
fn readiness_matches_accepts_ready_alias_and_document_state() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Stable,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::None,
        document_ready_state: Some("complete".to_string()),
        blocking_signals: Vec::new(),
        degraded_reason: None,
    };

    assert!(readiness_matches(&readiness, "ready"));
    assert!(readiness_matches(&readiness, "stable"));
    assert!(readiness_matches(&readiness, "complete"));
    assert!(!readiness_matches(&readiness, "loading"));
}

#[test]
fn network_request_matcher_respects_url_method_and_status() {
    let trigger = trigger(TriggerConditionKind::NetworkRequest);
    let record = NetworkRequestRecord {
        request_id: "req-1".to_string(),
        sequence: 2,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api/events".to_string(),
        method: "POST".to_string(),
        tab_target_id: Some(trigger.source_tab.target_id.clone()),
        status: Some(200),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: None,
        resource_type: None,
        mime_type: None,
    };

    assert!(network_request_matches(&record, &trigger));
}

#[test]
fn network_request_matcher_rejects_other_tabs() {
    let trigger = trigger(TriggerConditionKind::NetworkRequest);
    let record = NetworkRequestRecord {
        request_id: "req-2".to_string(),
        sequence: 3,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api/events".to_string(),
        method: "POST".to_string(),
        tab_target_id: Some("background".to_string()),
        status: Some(200),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: None,
        resource_type: None,
        mime_type: None,
    };

    assert!(!network_request_matches(&record, &trigger));
}

#[test]
fn network_request_matcher_rejects_other_frames_when_trigger_is_frame_bound() {
    let mut trigger = trigger(TriggerConditionKind::NetworkRequest);
    trigger.source_tab.frame_id = Some("source-frame".to_string());
    let record = NetworkRequestRecord {
        request_id: "req-3".to_string(),
        sequence: 4,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api/events".to_string(),
        method: "POST".to_string(),
        tab_target_id: Some(trigger.source_tab.target_id.clone()),
        status: Some(200),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: Some("sibling-frame".to_string()),
        resource_type: None,
        mime_type: None,
    };

    assert!(!network_request_matches(&record, &trigger));
}

#[test]
fn storage_snapshot_matcher_respects_area_and_value() {
    let trigger = trigger(TriggerConditionKind::StorageValue);
    let snapshot = StorageSnapshot {
        origin: "https://example.test".to_string(),
        local_storage: BTreeMap::from([("token".to_string(), "abc".to_string())]),
        session_storage: BTreeMap::from([("token".to_string(), "session".to_string())]),
    };

    assert!(storage_snapshot_matches(&snapshot, &trigger).expect("storage match"));
}

#[test]
fn resolve_trigger_workflow_spec_loads_named_asset() {
    let home = std::env::temp_dir().join(format!(
        "rub-trigger-worker-workflow-{}",
        std::process::id()
    ));
    let workflows_dir = home.join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();
    let asset_path = workflows_dir.join("reply_flow.json");
    fs::write(&asset_path, r#"{"steps":[{"command":"doctor","args":{}}]}"#).unwrap();

    let payload = serde_json::Map::from_iter([("workflow_name".to_string(), json!("reply_flow"))]);
    let (spec, source) = resolve_trigger_workflow_spec(&payload, &home).unwrap();
    assert_eq!(spec, r#"{"steps":[{"command":"doctor","args":{}}]}"#);
    assert_eq!(source["kind"], "workflow");
    assert_eq!(source["name"], "reply_flow");
    assert_eq!(source["path"], asset_path.display().to_string());
    assert!(source.get("vars").is_none());

    let _ = fs::remove_dir_all(home);
}

#[test]
fn resolve_trigger_workflow_spec_projects_inline_steps() {
    let payload = serde_json::Map::from_iter([(
        "steps".to_string(),
        json!([
            { "command": "click", "args": { "selector": "#continue" } },
            { "command": "type", "args": { "selector": "#name", "text": "Ada" } }
        ]),
    )]);
    let (spec, source) =
        resolve_trigger_workflow_spec(&payload, std::path::Path::new("/tmp")).unwrap();
    let spec_json: serde_json::Value = serde_json::from_str(&spec).unwrap();
    assert_eq!(spec_json.as_array().unwrap().len(), 2);
    assert_eq!(source["kind"], "trigger_inline_workflow");
    assert_eq!(source["step_count"], 2);
    assert!(source.get("vars").is_none());
}

#[test]
fn trigger_action_summary_prefers_named_workflow_projection() {
    let mut trigger = trigger(TriggerConditionKind::TextPresent);
    trigger.action.kind = TriggerActionKind::Workflow;
    trigger.action.command = None;
    trigger.action.payload = Some(json!({
        "workflow_name": "reply_flow"
    }));

    assert_eq!(trigger_action_summary(&trigger), "workflow 'reply_flow'");

    trigger.action.payload = Some(json!({
        "steps": [{ "command": "doctor", "args": {} }]
    }));
    assert_eq!(trigger_action_summary(&trigger), "inline workflow");
}

#[test]
fn trigger_action_execution_info_projects_workflow_metadata() {
    let mut trigger = trigger(TriggerConditionKind::TextPresent);
    trigger.action.kind = TriggerActionKind::Workflow;
    trigger.action.command = None;
    trigger.action.payload = Some(json!({
        "workflow_name": "reply_flow",
        "vars": {
            "reply_name": "Ada",
            "target_url": "https://example.com"
        },
        "source_vars": {
            "prompt_text": {
                "kind": "text",
                "selector": "#prompt"
            }
        }
    }));
    let info = trigger_action_execution_info(&trigger, std::path::Path::new("/tmp/rub-home"));
    assert_eq!(info.kind, TriggerActionKind::Workflow);
    assert_eq!(info.workflow_name.as_deref(), Some("reply_flow"));
    assert_eq!(
        info.workflow_path.as_deref(),
        Some("/tmp/rub-home/workflows/reply_flow.json")
    );
    assert_eq!(
        info.workflow_path_state
            .as_ref()
            .map(|state| state.path_authority.as_str()),
        Some("automation.action.workflow_path")
    );
    assert_eq!(
        info.workflow_path_state
            .as_ref()
            .map(|state| state.upstream_truth.as_str()),
        Some("trigger_action_payload.workflow_name")
    );
    assert_eq!(info.inline_step_count, None);
    assert_eq!(
        info.vars,
        vec!["reply_name".to_string(), "target_url".to_string()]
    );
    assert_eq!(info.source_vars, vec!["prompt_text".to_string()]);
}

#[test]
fn resolve_trigger_workflow_spec_marks_named_workflow_asset_path() {
    let home =
        std::env::temp_dir().join(format!("rub-trigger-workflow-spec-{}", std::process::id()));
    let workflows = home.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let workflow_path = workflows.join("reply_flow.json");
    std::fs::write(&workflow_path, r#"[{"command":"doctor","args":{}}]"#).unwrap();

    let (_, spec_source) = resolve_trigger_workflow_spec(
        &serde_json::json!({
            "workflow_name": "reply_flow",
        })
        .as_object()
        .unwrap()
        .clone(),
        &home,
    )
    .expect("named workflow should resolve");

    assert_eq!(spec_source["path"], workflow_path.display().to_string());
    assert_eq!(
        spec_source["path_state"]["path_authority"],
        "trigger.workflow.spec_source.path"
    );
    assert_eq!(
        spec_source["path_state"]["upstream_truth"],
        "trigger_workflow_payload.workflow_name"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn classify_trigger_error_status_preserves_blocked_vs_degraded_boundary() {
    assert_eq!(
        classify_trigger_error_status(ErrorCode::AutomationPaused),
        TriggerStatus::Blocked
    );
    assert_eq!(
        classify_trigger_error_status(ErrorCode::BrowserCrashed),
        TriggerStatus::Degraded
    );
}

#[test]
fn target_continuity_fails_when_frame_runtime_is_stale() {
    let frame_runtime = FrameRuntimeInfo {
        status: FrameContextStatus::Stale,
        current_frame: Some(FrameContextInfo {
            frame_id: "missing-frame".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: None,
            url: None,
            depth: 0,
            same_origin_accessible: None,
        }),
        primary_frame: None,
        frame_lineage: vec!["missing-frame".to_string()],
        degraded_reason: Some("selected_frame_not_found".to_string()),
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Stable,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::None,
        document_ready_state: Some("complete".to_string()),
        blocking_signals: Vec::new(),
        degraded_reason: None,
    };

    assert_eq!(
        trigger_target_continuity_failure("tab-target", &frame_runtime, &readiness),
        Some((
            "continuity_frame_unavailable",
            "Trigger target continuity fence failed: frame context became unavailable",
        ))
    );
}

#[test]
fn target_continuity_fails_when_readiness_is_degraded() {
    let frame_runtime = FrameRuntimeInfo {
        status: FrameContextStatus::Top,
        current_frame: Some(FrameContextInfo {
            frame_id: "main-frame".to_string(),
            name: Some("main".to_string()),
            parent_frame_id: None,
            target_id: Some("tab-target".to_string()),
            url: Some("https://example.test".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }),
        primary_frame: Some(FrameContextInfo {
            frame_id: "main-frame".to_string(),
            name: Some("main".to_string()),
            parent_frame_id: None,
            target_id: Some("tab-target".to_string()),
            url: Some("https://example.test".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }),
        frame_lineage: vec!["main-frame".to_string()],
        degraded_reason: None,
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Degraded,
        route_stability: RouteStability::Transitioning,
        loading_present: true,
        skeleton_present: false,
        overlay_state: OverlayState::None,
        document_ready_state: Some("interactive".to_string()),
        blocking_signals: vec!["probe_timeout".to_string()],
        degraded_reason: Some("probe_timeout".to_string()),
    };

    assert_eq!(
        trigger_target_continuity_failure("tab-target", &frame_runtime, &readiness),
        Some((
            "continuity_readiness_degraded",
            "Trigger target continuity fence failed: readiness surface degraded",
        ))
    );
}

#[tokio::test]
async fn trigger_cycle_uses_queue_authority_even_with_foreground_in_flight() {
    let router = test_router();
    let state = Arc::new(SessionState::new("default", temp_home("fairness"), None));
    state
        .in_flight_count
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let mut worker_state = std::collections::HashMap::new();
    let mut pending_reservations = std::collections::HashMap::new();
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<TriggerReservationCompletion>();
    let mut next_reservation_attempt_id = 0_u64;

    run_trigger_cycle(
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
    assert_eq!(metrics["trigger_worker"]["cycle_count"], json!(1));
    assert_eq!(
        metrics["authority_inventory"]["trigger_worker_pre_queue_gate"],
        json!("none")
    );
}

#[tokio::test]
async fn ready_trigger_reservation_completion_releases_idle_queue_permit() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("reservation-completion-release"),
        None,
    ));
    let reserved = router
        .begin_automation_reservation_transaction_owned(&state, "queued_trigger")
        .await
        .expect("queued trigger reservation should acquire immediately in test");
    let mut worker_state = std::collections::HashMap::new();
    let mut pending_reservations = std::collections::HashMap::from([(
        7_u32,
        PendingTriggerReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingTriggerConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: true,
                rule_semantics_fingerprint: String::new(),
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, mut reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<TriggerReservationCompletion>();
    reservation_tx
        .send(TriggerReservationCompletion {
            trigger_id: 7,
            attempt_id: 1,
            result: Ok(reserved),
        })
        .expect("reservation completion should enqueue");

    drain_trigger_reservation_completions(
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
async fn pending_network_request_trigger_is_not_re_evaluated_during_queue_wait() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("pending-network-request"),
        None,
    ));
    let browser = router.browser_port();
    let trigger = trigger(TriggerConditionKind::NetworkRequest);
    let tabs: Vec<rub_core::model::TabInfo> = Vec::new();
    let mut worker_state = std::collections::HashMap::from([(
        trigger.id,
        TriggerWorkerEntry {
            last_status: TriggerStatus::Armed,
            network_cursor: 0,
            observatory_drop_count: 0,
        },
    )]);
    let mut pending_reservations = std::collections::HashMap::from([(
        trigger.id,
        PendingTriggerReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingTriggerConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: trigger_rule_semantics_fingerprint(&trigger),
            },
            task: tokio::spawn(async {}),
        },
    )]);
    let (reservation_tx, _reservation_rx) =
        tokio::sync::mpsc::unbounded_channel::<TriggerReservationCompletion>();
    let mut next_reservation_attempt_id = 0_u64;
    let mut reservation_coordinator = TriggerReservationCoordinator {
        pending_reservations: &mut pending_reservations,
        reservation_tx: &reservation_tx,
        next_reservation_attempt_id: &mut next_reservation_attempt_id,
    };

    process_trigger_rule(
        &router,
        &state,
        &browser,
        &tabs,
        trigger.clone(),
        &mut worker_state,
        &mut reservation_coordinator,
    )
    .await;

    assert!(pending_reservations.contains_key(&trigger.id));
    assert_eq!(next_reservation_attempt_id, 0);
}

#[tokio::test]
async fn reconcile_pending_network_request_trigger_reservation_drops_semantics_drift() {
    let mut stale_trigger = trigger(TriggerConditionKind::NetworkRequest);
    stale_trigger.condition.url_pattern = Some("/old".to_string());
    let mut live_trigger = stale_trigger.clone();
    live_trigger.condition.url_pattern = Some("/new".to_string());

    let mut pending_reservations = std::collections::HashMap::from([(
        live_trigger.id,
        PendingTriggerReservation {
            attempt_id: 1,
            fallback_network_progress: None,
            condition_policy: PendingTriggerConditionPolicy {
                preserved_triggered: None,
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: trigger_rule_semantics_fingerprint(&stale_trigger),
            },
            task: tokio::spawn(async {}),
        },
    )]);

    reconcile_pending_trigger_reservations(&[live_trigger], &mut pending_reservations);

    assert!(pending_reservations.is_empty());
}
