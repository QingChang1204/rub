use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{
    ReadinessInfo, TabInfo, TriggerActionExecutionInfo, TriggerActionKind, TriggerEvidenceInfo,
    TriggerInfo, TriggerResultInfo, TriggerStatus,
};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::debug;

use crate::router::DaemonRouter;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::runtime_refresh::{
    refresh_live_frame_runtime, refresh_live_runtime_state, refresh_live_trigger_runtime,
};
use crate::session::SessionState;

mod action;
mod condition;
mod outcome;
mod reservation;

use action::{
    fire_trigger, trigger_action_command_id, trigger_action_execution_info, trigger_action_summary,
};
#[cfg(test)]
use action::{resolve_trigger_workflow_spec, trigger_target_continuity_failure};
use condition::{
    TriggerConditionState, commit_trigger_network_progress, load_trigger_condition_state,
    reconcile_worker_state, trigger_evidence_consumption_key,
};
use outcome::record_trigger_failure;
use reservation::reserve_trigger_execution;

const TRIGGER_WORKER_INTERVAL: Duration = Duration::from_millis(500);
const TRIGGER_ACTION_BASE_TIMEOUT_MS: u64 = 30_000;
const TRIGGER_AUTOMATION_TRANSACTION_TIMEOUT_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
struct TriggerWorkerEntry {
    last_status: TriggerStatus,
    network_cursor: u64,
    observatory_drop_count: u64,
}

pub(crate) fn spawn_trigger_worker(
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
    shutdown: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TRIGGER_WORKER_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut worker_state = HashMap::<u32, TriggerWorkerEntry>::new();

        loop {
            if state.is_shutdown_requested() {
                break;
            }
            tokio::select! {
                _ = shutdown.notified() => break,
                _ = ticker.tick() => {
                    if state.is_shutdown_requested() {
                        break;
                    }
                    run_trigger_cycle(&router, &state, &mut worker_state).await;
                }
            }
        }
    })
}

async fn run_trigger_cycle(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
) {
    if state.in_flight_count.load(Ordering::SeqCst) > 0 {
        return;
    }

    let triggers = state.triggers().await;
    if triggers.is_empty() {
        worker_state.clear();
        return;
    }

    let browser = router.browser_port();
    let tabs = match refresh_live_trigger_runtime(&browser, state).await {
        Ok(tabs) => tabs,
        Err(error) => {
            debug!(error = %error, "Trigger worker skipped cycle after tab refresh failure");
            return;
        }
    };

    let active_request_cursor = state.network_request_cursor().await;
    let observatory_drop_count = state.network_request_drop_count().await;
    reconcile_worker_state(
        worker_state,
        &triggers,
        active_request_cursor,
        observatory_drop_count,
    );

    for trigger in triggers {
        if !matches!(trigger.status, TriggerStatus::Armed) || trigger.unavailable_reason.is_some() {
            continue;
        }

        process_trigger_rule(router, state, &browser, &tabs, trigger, worker_state).await;
    }
}

async fn process_trigger_rule(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    tabs: &[TabInfo],
    trigger: TriggerInfo,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
) {
    let condition = match load_trigger_condition_state(
        browser,
        state,
        tabs,
        &trigger,
        worker_state
            .get_mut(&trigger.id)
            .expect("worker_state entry should exist"),
    )
    .await
    {
        Ok(condition) => condition,
        Err(error) => {
            record_trigger_failure(
                state,
                &trigger,
                ErrorEnvelope::new(
                    ErrorCode::BrowserCrashed,
                    format!("trigger condition evaluation failed: {error}"),
                ),
                None,
                None,
            )
            .await;
            return;
        }
    };

    let triggered = match condition {
        TriggerConditionState::NotTriggered { network_progress } => {
            let _ = state.set_trigger_condition_evidence(trigger.id, None).await;
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
                commit_trigger_network_progress(worker, network_progress);
            }
            return;
        }
        TriggerConditionState::Triggered(triggered) => triggered,
    };

    let reserved = match reserve_trigger_execution(
        router,
        state,
        browser,
        &trigger,
        worker_state
            .get_mut(&trigger.id)
            .expect("worker_state entry should exist"),
        triggered,
    )
    .await
    {
        Ok(Some(reserved)) => reserved,
        Ok(None) => return,
        Err(envelope) => {
            record_trigger_failure(state, &trigger, envelope, None, None).await;
            return;
        }
    };

    let command_id = trigger_action_command_id(&reserved.trigger, &reserved.evidence);
    match fire_trigger(
        router,
        state,
        &reserved.tabs,
        &reserved.trigger,
        &reserved.evidence,
        &command_id,
    )
    .await
    {
        Err(envelope) => {
            record_trigger_failure(
                state,
                &reserved.trigger,
                envelope,
                Some(reserved.evidence.clone()),
                Some(command_id),
            )
            .await;
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
                commit_trigger_network_progress(worker, reserved.network_progress);
            }
        }
        Ok(result) => {
            let action_summary = trigger_action_summary(&reserved.trigger);
            let summary = format!(
                "trigger fired after {} and executed {} on target tab {}",
                reserved.evidence.summary, action_summary, reserved.trigger.target_tab.index
            );
            let _ = state
                .record_trigger_outcome(
                    reserved.trigger.id,
                    TriggerStatus::Fired,
                    Some(reserved.evidence),
                    TriggerResultInfo {
                        trigger_id: reserved.trigger.id,
                        status: TriggerStatus::Fired,
                        summary,
                        command_id: Some(command_id),
                        action: Some(trigger_action_execution_info(
                            &reserved.trigger,
                            &state.rub_home,
                        )),
                        result,
                        error_code: None,
                        reason: None,
                        consumed_evidence_fingerprint: Some(reserved.evidence_fingerprint),
                    },
                )
                .await;
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
                commit_trigger_network_progress(worker, reserved.network_progress);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::condition::{
        network_request_matches, parse_storage_area, readiness_matches, storage_snapshot_matches,
    };
    use super::outcome::classify_trigger_error_status;
    use super::{
        resolve_trigger_workflow_spec, trigger_action_execution_info, trigger_action_summary,
        trigger_target_continuity_failure,
    };
    use rub_core::error::ErrorCode;
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, NetworkRequestLifecycle,
        NetworkRequestRecord, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerStatus, TriggerTabBindingInfo,
    };
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs;

    fn trigger(kind: TriggerConditionKind) -> TriggerInfo {
        TriggerInfo {
            id: 1,
            status: TriggerStatus::Armed,
            mode: TriggerMode::Once,
            source_tab: TriggerTabBindingInfo {
                index: 0,
                target_id: "source".to_string(),
                url: "https://source.example".to_string(),
                title: "Source".to_string(),
            },
            target_tab: TriggerTabBindingInfo {
                index: 1,
                target_id: "target".to_string(),
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
                storage_area: Some("local".to_string()),
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
    fn parse_storage_area_accepts_local_and_session() {
        assert_eq!(
            parse_storage_area(Some("local")).expect("local"),
            Some(StorageArea::Local)
        );
        assert_eq!(
            parse_storage_area(Some("session")).expect("session"),
            Some(StorageArea::Session)
        );
        assert_eq!(parse_storage_area(None).expect("none"), None);
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

        let payload =
            serde_json::Map::from_iter([("workflow_name".to_string(), json!("reply_flow"))]);
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
}
