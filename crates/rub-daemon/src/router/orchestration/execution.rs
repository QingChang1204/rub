use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
    OrchestrationSessionInfo, TriggerConditionKind, TriggerEvidenceInfo,
};

use crate::orchestration_executor::execute_orchestration_rule;
use crate::orchestration_executor::orchestration_non_authoritative_evidence_error;
use crate::orchestration_executor::run_orchestration_future_with_outer_deadline;
use crate::orchestration_probe::{
    dispatch_remote_orchestration_probe, evaluate_orchestration_probe_for_tab,
};
use crate::orchestration_runtime::{
    OrchestrationOutcomeCommit, orchestration_session_not_addressable_error,
};
use crate::orchestration_worker::orchestration_evidence_key;
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;
use crate::session::{NetworkRequestBaseline, SessionState};

use super::DaemonRouter;
use super::projection::{orchestration_payload, orchestration_rule_subject};
use super::rule::{
    blocked_cooldown_result, orchestration_rule_in_cooldown, orchestration_status_name,
};
use crate::router::{RouterFenceDisposition, TransactionDeadline};

pub(super) async fn cmd_orchestration_execute(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_orchestration_execute_with_router_fence_disposition(
        router,
        id,
        deadline,
        state,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await
}

pub(super) async fn capture_orchestration_network_request_baseline(
    runtime: &OrchestrationRuntimeInfo,
    state: &Arc<SessionState>,
    source: &rub_core::model::OrchestrationAddressInfo,
    condition: &rub_core::model::TriggerConditionSpec,
    deadline: TransactionDeadline,
) -> Result<NetworkRequestBaseline, RubError> {
    if !matches!(condition.kind, TriggerConditionKind::NetworkRequest) {
        return Err(RubError::Internal(
            "capture_orchestration_network_request_baseline requires a network_request condition"
                .to_string(),
        ));
    }

    if source.session_id == state.session_id {
        return Ok(state.current_network_request_baseline().await);
    }

    let tab_target_id = source.tab_target_id.as_deref().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Orchestration source address is missing source.tab_target_id",
            serde_json::json!({
                "reason": "orchestration_source_tab_target_missing",
                "source_session_id": source.session_id,
                "source_session_name": source.session_name,
            }),
        )
    })?;
    let source_session = resolve_orchestration_source_session_by_identity(
        runtime,
        &source.session_id,
        &source.session_name,
    )
    .map_err(RubError::Domain)?;
    let result = dispatch_remote_orchestration_probe(
        source_session,
        tab_target_id,
        source.frame_id.as_deref(),
        condition,
        u64::MAX,
        u64::MAX,
        Some(deadline),
    )
    .await
    .map_err(RubError::Domain)?;
    Ok(NetworkRequestBaseline {
        cursor: result.next_network_cursor,
        observed_ingress_drop_count: result.observed_drop_count,
        primed: true,
    })
}

async fn cmd_orchestration_execute_with_router_fence_disposition(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let mut runtime = state.orchestration_runtime().await;
    ensure_orchestration_execution_available(&runtime)?;
    let mut rule = state.orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    if !matches!(rule.status, OrchestrationRuleStatus::Armed) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Orchestration rule id {id} must be 'armed' before execute; current status is '{}'",
                orchestration_status_name(rule.status),
            ),
        ));
    }
    if orchestration_rule_in_cooldown(&rule) {
        let result = blocked_cooldown_result(&rule);
        let outcome_commit = state
            .record_orchestration_outcome(
                id,
                Some(rule.lifecycle_generation),
                rule.last_condition_evidence.clone(),
                result.clone(),
            )
            .await;
        let runtime = state.orchestration_runtime().await;
        let rule = runtime
            .rules
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .or_else(|| match &outcome_commit {
                OrchestrationOutcomeCommit::Applied(Some(rule))
                | OrchestrationOutcomeCommit::Stale(Some(rule)) => Some(rule.clone()),
                OrchestrationOutcomeCommit::Applied(None)
                | OrchestrationOutcomeCommit::Stale(None) => None,
            })
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while recording cooldown outcome"
                ))
            })?;
        let lifecycle_commit = matches!(outcome_commit, OrchestrationOutcomeCommit::Stale(_))
            .then_some("stale_rule_generation");

        return Ok(orchestration_payload(
            orchestration_rule_subject(id),
            serde_json::json!({
                "rule": rule,
                "execution": result,
                "lifecycle_commit": lifecycle_commit,
            }),
            &runtime,
        ));
    }
    if rule.unavailable_reason.is_some() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is currently unavailable"),
            serde_json::json!({
                "reason": "orchestration_rule_unavailable",
                "unavailable_reason": rule.unavailable_reason,
            }),
        ));
    }

    let _active_execution_fence = acquire_manual_orchestration_execution_fence_if_remote(
        router,
        state,
        &rule,
        deadline,
        router_fence_disposition,
    )
    .await?;
    refresh_orchestration_runtime(state).await;
    runtime = state.orchestration_runtime().await;
    ensure_orchestration_execution_available(&runtime)?;
    rule = state.orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    if !matches!(rule.status, OrchestrationRuleStatus::Armed) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Orchestration rule id {id} must be 'armed' before execute; current status is '{}'",
                orchestration_status_name(rule.status),
            ),
        ));
    }
    if orchestration_rule_in_cooldown(&rule) {
        let result = blocked_cooldown_result(&rule);
        let outcome_commit = state
            .record_orchestration_outcome(
                id,
                Some(rule.lifecycle_generation),
                rule.last_condition_evidence.clone(),
                result.clone(),
            )
            .await;
        runtime = state.orchestration_runtime().await;
        let rule = runtime
            .rules
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .or_else(|| match &outcome_commit {
                OrchestrationOutcomeCommit::Applied(Some(rule))
                | OrchestrationOutcomeCommit::Stale(Some(rule)) => Some(rule.clone()),
                OrchestrationOutcomeCommit::Applied(None)
                | OrchestrationOutcomeCommit::Stale(None) => None,
            })
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while recording cooldown outcome"
                ))
            })?;
        let lifecycle_commit = matches!(outcome_commit, OrchestrationOutcomeCommit::Stale(_))
            .then_some("stale_rule_generation");

        return Ok(orchestration_payload(
            orchestration_rule_subject(id),
            serde_json::json!({
                "rule": rule,
                "execution": result,
                "lifecycle_commit": lifecycle_commit,
            }),
            &runtime,
        ));
    }
    if rule.unavailable_reason.is_some() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is currently unavailable"),
            serde_json::json!({
                "reason": "orchestration_rule_unavailable",
                "unavailable_reason": rule.unavailable_reason,
            }),
        ));
    }
    let execution_evidence =
        capture_manual_orchestration_condition_evidence(router, state, &runtime, &rule, deadline)
            .await?;
    let manual_command_identity_key = execution_evidence.as_ref().map(orchestration_evidence_key);
    let result = execute_orchestration_rule(
        router,
        state,
        &runtime,
        &rule,
        manual_command_identity_key.as_deref(),
        Some(deadline),
        router_fence_disposition,
    )
    .await;
    let outcome_commit = state
        .record_orchestration_outcome(
            id,
            Some(rule.lifecycle_generation),
            execution_evidence,
            result.clone(),
        )
        .await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .or_else(|| match &outcome_commit {
            OrchestrationOutcomeCommit::Applied(Some(rule))
            | OrchestrationOutcomeCommit::Stale(Some(rule)) => Some(rule.clone()),
            OrchestrationOutcomeCommit::Applied(None) | OrchestrationOutcomeCommit::Stale(None) => {
                None
            }
        })
        .ok_or_else(|| {
            RubError::Internal(format!(
                "Orchestration rule id {id} disappeared while recording execution outcome"
            ))
        })?;
    let lifecycle_commit = matches!(outcome_commit, OrchestrationOutcomeCommit::Stale(_))
        .then_some("stale_rule_generation");

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
            "execution": result,
            "lifecycle_commit": lifecycle_commit,
        }),
        &runtime,
    ))
}

async fn capture_manual_orchestration_condition_evidence(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    deadline: TransactionDeadline,
) -> Result<Option<TriggerEvidenceInfo>, RubError> {
    if matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest) {
        return Ok(None);
    }
    let tab_target_id = rule.source.tab_target_id.as_deref().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Orchestration source address is missing source.tab_target_id",
            serde_json::json!({
                "reason": "orchestration_source_tab_target_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }),
        )
    })?;
    let probe = if rule.source.session_id == state.session_id {
        run_orchestration_future_with_outer_deadline(
            Some(deadline),
            || {
                RubError::domain_with_context(
                    ErrorCode::IpcTimeout,
                    "Manual orchestration execute exhausted the caller-owned timeout budget before authoritative source probe completed",
                    serde_json::json!({
                        "reason": "orchestration_source_probe_timeout_budget_exhausted",
                        "source_session_id": rule.source.session_id,
                        "source_session_name": rule.source.session_name,
                        "tab_target_id": tab_target_id,
                        "frame_id": rule.source.frame_id,
                    }),
                )
            },
            evaluate_orchestration_probe_for_tab(
                &router.browser_port(),
                state,
                tab_target_id,
                rule.source.frame_id.as_deref(),
                &rule.condition,
                u64::MAX,
                0,
            ),
        )
        .await?
    } else {
        let source_session =
            resolve_orchestration_source_session(runtime, rule).map_err(RubError::Domain)?;
        dispatch_remote_orchestration_probe(
            source_session,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            u64::MAX,
            0,
            Some(deadline),
        )
        .await
        .map_err(RubError::Domain)?
    };
    if let Some(reason) = probe.degraded_reason {
        return Err(RubError::Domain(
            orchestration_non_authoritative_evidence_error(
                "Manual orchestration execute cannot derive authoritative condition evidence because probe evidence was dropped",
                Some(reason),
                serde_json::json!({}),
            ),
        ));
    }
    Ok(if probe.matched { probe.evidence } else { None })
}

fn resolve_orchestration_source_session_by_identity<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    source_session_id: &str,
    source_session_name: &str,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    let session = runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == source_session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Source session '{}' is not available for orchestration condition evaluation",
                    source_session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_missing",
                "source_session_id": source_session_id,
                "source_session_name": source_session_name,
            }))
        })?;
    if crate::orchestration_runtime::orchestration_session_addressability_reason(session).is_some()
    {
        return Err(orchestration_session_not_addressable_error(
            session,
            ErrorCode::SessionBusy,
            format!(
                "Source session '{}' is still present but not addressable for orchestration condition evaluation",
                source_session_name
            ),
            "orchestration_source_session_not_addressable",
            "source_session_id",
            "source_session_name",
        ));
    }
    Ok(session)
}

fn resolve_orchestration_source_session<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    resolve_orchestration_source_session_by_identity(
        runtime,
        &rule.source.session_id,
        &rule.source.session_name,
    )
}

pub(super) fn ensure_orchestration_addressing_available(
    runtime: &OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.addressing_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration session addressing is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

pub(super) fn ensure_orchestration_execution_available(
    runtime: &OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.execution_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration execution is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

async fn acquire_manual_orchestration_execution_fence_if_remote<'a>(
    router: &'a DaemonRouter,
    state: &Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    deadline: TransactionDeadline,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<Option<crate::router::RouterTransactionGuard<'a>>, RubError> {
    if rule.target.session_id == state.session_id {
        return Ok(None);
    }

    let queue_wait_budget = std::time::Duration::from_millis(deadline.remaining_ms());
    let transaction = router
        .begin_automation_transaction_if_needed(
            state,
            "manual_orchestration_remote_execution",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            router_fence_disposition,
        )
        .await
        .map_err(RubError::Domain)?;
    Ok(transaction)
}

pub(super) async fn update_orchestration_status(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    next_status: OrchestrationRuleStatus,
) -> Result<serde_json::Value, RubError> {
    update_orchestration_status_with_router_fence_disposition(
        router,
        id,
        deadline,
        state,
        next_status,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await
}

async fn update_orchestration_status_with_router_fence_disposition(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    next_status: OrchestrationRuleStatus,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<serde_json::Value, RubError> {
    let queue_wait_budget = std::time::Duration::from_millis(deadline.remaining_ms());
    let _active_execution_fence = router
        .begin_automation_transaction_if_needed(
            state,
            "orchestration_rule_status_update",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            router_fence_disposition,
        )
        .await
        .map_err(RubError::Domain)?;
    let current = state
        .orchestration_rules()
        .await
        .into_iter()
        .find(|rule| rule.id == id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Orchestration rule id {id} is not present in the current registry"),
            )
        })?;

    let rule = match (current.status, next_status) {
        (OrchestrationRuleStatus::Armed, OrchestrationRuleStatus::Paused)
        | (OrchestrationRuleStatus::Paused, OrchestrationRuleStatus::Armed) => {
            let network_baseline = if matches!(next_status, OrchestrationRuleStatus::Armed)
                && matches!(current.condition.kind, TriggerConditionKind::NetworkRequest)
            {
                refresh_orchestration_runtime(state).await;
                let runtime = state.orchestration_runtime().await;
                Some(
                    capture_orchestration_network_request_baseline(
                        &runtime,
                        state,
                        &current.source,
                        &current.condition,
                        deadline,
                    )
                    .await?,
                )
            } else {
                None
            };
            state
                .set_orchestration_rule_status_with_network_baseline(
                    id,
                    next_status,
                    network_baseline,
                )
                .await
                .ok_or_else(|| {
                    RubError::Internal(format!(
                        "Orchestration rule id {id} disappeared while applying status update"
                    ))
                })?
        }
        (status, requested) if status == requested => current,
        _ => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration rule id {id} cannot transition from '{}' to '{}'",
                    orchestration_status_name(current.status),
                    orchestration_status_name(next_status),
                ),
            ));
        }
    };

    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
        }),
        &runtime,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_manual_orchestration_execution_fence_if_remote,
        capture_manual_orchestration_condition_evidence,
        capture_orchestration_network_request_baseline, cmd_orchestration_execute,
        cmd_orchestration_execute_with_router_fence_disposition,
        resolve_orchestration_source_session,
        update_orchestration_status_with_router_fence_disposition,
    };
    use crate::orchestration_probe::OrchestrationProbeResult;
    use crate::router::{DaemonRouter, RouterFenceDisposition, TransactionDeadline};
    use crate::session::SessionState;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
        OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
        OrchestrationRuntimeStatus, OrchestrationSessionAvailability, TriggerConditionKind,
        TriggerConditionSpec,
    };
    use rub_ipc::codec::NdJsonCodec;
    use rub_ipc::protocol::{IpcRequest, IpcResponse};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    fn queue_test_connection(socket_path: &std::path::Path) -> UnixStream {
        let (client, server) = UnixStream::pair().expect("create in-memory unix stream pair");
        crate::orchestration_executor::queue_remote_orchestration_connection_for_test(
            socket_path,
            client,
        );
        server
    }

    fn rule() -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id: 1,
            status: OrchestrationRuleStatus::Armed,
            lifecycle_generation: 1,
            source: OrchestrationAddressInfo {
                session_id: "sess-source".to_string(),
                session_name: "source".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("tab-source".to_string()),
                frame_id: None,
            },
            target: OrchestrationAddressInfo {
                session_id: "sess-target".to_string(),
                session_name: "target".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("tab-target".to_string()),
                frame_id: None,
            },
            mode: OrchestrationMode::Once,
            execution_policy: OrchestrationExecutionPolicyInfo::default(),
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::TextPresent,
                locator: None,
                text: Some("ready".to_string()),
                url_pattern: None,
                readiness_state: None,
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            actions: Vec::new(),
            correlation_key: "corr-source".to_string(),
            idempotency_key: "idem-source".to_string(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        }
    }

    fn local_rule() -> OrchestrationRuleInfo {
        let mut rule = rule();
        rule.source.session_id = "sess-local".to_string();
        rule.source.session_name = "default".to_string();
        rule
    }

    fn test_router() -> DaemonRouter {
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
        DaemonRouter::new(adapter)
    }

    fn addressable_session(
        session_id: &str,
        session_name: &str,
        current: bool,
    ) -> rub_core::model::OrchestrationSessionInfo {
        crate::orchestration_runtime::projected_orchestration_session(
            session_id.to_string(),
            session_name.to_string(),
            42,
            format!("/tmp/{session_id}.sock"),
            current,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            OrchestrationSessionAvailability::Addressable,
            Some(format!("/tmp/{session_id}-profile")),
        )
    }

    #[tokio::test]
    async fn capture_remote_network_request_baseline_preserves_nonzero_drop_count() {
        let socket_path = PathBuf::from(format!(
            "/tmp/rub-orch-baseline-{}.sock",
            uuid::Uuid::now_v7()
        ));
        let request_stream = queue_test_connection(&socket_path);

        let server = tokio::spawn(async move {
            let stream = request_stream;
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_orchestration_probe");
            assert_eq!(
                request
                    .args
                    .get("after_sequence")
                    .and_then(|value| value.as_u64()),
                Some(u64::MAX)
            );
            assert_eq!(
                request
                    .args
                    .get("last_observed_drop_count")
                    .and_then(|value| value.as_u64()),
                Some(u64::MAX)
            );
            let response = IpcResponse::success(
                "req-remote-baseline",
                serde_json::to_value(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: 41,
                    observed_drop_count: 5,
                    degraded_reason: None,
                })
                .expect("probe result should serialize"),
            )
            .with_daemon_session_id("sess-source")
            .expect("daemon session id should be valid")
            .with_command_id(
                request
                    .command_id
                    .as_deref()
                    .expect("probe request should keep command id"),
            )
            .expect("command id should be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-remote-baseline"),
            None,
        ));
        let runtime = OrchestrationRuntimeInfo {
            status: OrchestrationRuntimeStatus::Active,
            known_sessions: vec![
                crate::orchestration_runtime::projected_orchestration_session(
                    "sess-source".to_string(),
                    "source".to_string(),
                    42,
                    socket_path.display().to_string(),
                    false,
                    rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    OrchestrationSessionAvailability::Addressable,
                    Some("/tmp/rub-source-profile".to_string()),
                ),
            ],
            session_count: 1,
            addressing_supported: true,
            execution_supported: true,
            current_session_id: Some("sess-local".to_string()),
            current_session_name: Some("default".to_string()),
            degraded_reason: None,
            ..Default::default()
        };
        let mut remote_rule = rule();
        remote_rule.condition.kind = TriggerConditionKind::NetworkRequest;

        let baseline = capture_orchestration_network_request_baseline(
            &runtime,
            &state,
            &remote_rule.source,
            &remote_rule.condition,
            TransactionDeadline::new(1_000),
        )
        .await
        .expect("remote baseline capture should succeed");

        assert_eq!(baseline.cursor, 41);
        assert_eq!(baseline.observed_ingress_drop_count, 5);
        assert!(baseline.primed);

        server.await.expect("server join");
    }

    #[test]
    fn resolve_orchestration_source_session_rejects_visible_non_addressable_session_with_path_context()
     {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "sess-source".to_string(),
            "source".to_string(),
            42,
            "/tmp/rub-source.sock".to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            OrchestrationSessionAvailability::ProtocolIncompatible,
            Some("/tmp/rub-source-profile".to_string()),
        );
        let runtime = OrchestrationRuntimeInfo {
            status: OrchestrationRuntimeStatus::Degraded,
            known_sessions: vec![session],
            session_count: 1,
            addressing_supported: true,
            execution_supported: true,
            current_session_id: Some("sess-local".to_string()),
            current_session_name: Some("default".to_string()),
            degraded_reason: Some("registry_contains_non_addressable_sessions".to_string()),
            ..Default::default()
        };

        let error = resolve_orchestration_source_session(&runtime, &rule())
            .expect_err("non-addressable source session");

        assert_eq!(error.code, ErrorCode::SessionBusy);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_source_session_not_addressable")
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|value| value.get("user_data_dir"))
                .and_then(|value| value.as_str()),
            Some("/tmp/rub-source-profile")
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|value| value.get("user_data_dir_state"))
                .and_then(|value| value.get("path_kind"))
                .and_then(|value| value.as_str()),
            Some("managed_user_data_directory")
        );
    }

    #[tokio::test]
    async fn manual_orchestration_execute_fails_closed_when_local_probe_deadline_is_exhausted() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-local-orchestration-probe-timeout"),
            None,
        ));
        let runtime = OrchestrationRuntimeInfo::default();
        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));

        let error = capture_manual_orchestration_condition_evidence(
            &router,
            &state,
            &runtime,
            &local_rule(),
            deadline,
        )
        .await
        .expect_err("expired outer deadline should fail closed before local source probe");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_source_probe_timeout_budget_exhausted")
        );
    }

    #[tokio::test]
    async fn manual_orchestration_execute_checks_cooldown_before_local_source_probe() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-local-orchestration-cooldown"),
            None,
        ));
        state
            .set_orchestration_runtime(
                1,
                vec![addressable_session("sess-target", "target", false)],
                true,
                true,
                None,
            )
            .await;

        let mut cooling_rule = local_rule();
        cooling_rule.execution_policy.cooldown_until_ms = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_millis() as u64
                + 60_000,
        );
        cooling_rule.last_condition_evidence = Some(rub_core::model::TriggerEvidenceInfo {
            summary: "cached cooldown evidence".to_string(),
            fingerprint: Some("cooldown-fingerprint".to_string()),
        });
        let rule = state
            .register_orchestration_rule(cooling_rule)
            .await
            .expect("cooldown rule should register");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));

        let payload = cmd_orchestration_execute(&router, rule.id, deadline, &state)
            .await
            .expect("cooldown should fail closed before spending source-probe authority");

        assert_eq!(
            payload["result"]["execution"]["reason"],
            serde_json::json!("orchestration_cooldown_active")
        );
        assert_eq!(
            payload["result"]["rule"]["last_condition_evidence"]["summary"],
            serde_json::json!("cached cooldown evidence")
        );
    }

    #[tokio::test]
    async fn manual_orchestration_execute_checks_cooldown_before_unavailable_reason() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-local-orchestration-cooldown-unavailable"),
            None,
        ));
        state
            .set_orchestration_runtime(1, Vec::new(), true, true, None)
            .await;

        let mut cooling_rule = local_rule();
        cooling_rule.execution_policy.cooldown_until_ms = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_millis() as u64
                + 60_000,
        );
        let rule = state
            .register_orchestration_rule(cooling_rule)
            .await
            .expect("cooldown rule should register");

        let payload =
            cmd_orchestration_execute(&router, rule.id, TransactionDeadline::new(500), &state)
                .await
                .expect("cooldown should win before unavailable-rule lane");

        assert_eq!(
            payload["result"]["execution"]["reason"],
            serde_json::json!("orchestration_cooldown_active")
        );
    }

    #[tokio::test]
    async fn manual_orchestration_execute_checks_cooldown_before_remote_source_resolution() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-remote-orchestration-cooldown"),
            None,
        ));
        state
            .set_orchestration_runtime(
                1,
                vec![addressable_session("sess-target", "target", false)],
                true,
                true,
                None,
            )
            .await;

        let mut cooling_rule = rule();
        cooling_rule.execution_policy.cooldown_until_ms = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_millis() as u64
                + 60_000,
        );
        let rule = state
            .register_orchestration_rule(cooling_rule)
            .await
            .expect("cooldown rule should register");

        let payload = cmd_orchestration_execute_with_router_fence_disposition(
            &router,
            rule.id,
            TransactionDeadline::new(500),
            &state,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect("cooldown should win before remote source-session resolution");

        assert_eq!(
            payload["result"]["execution"]["reason"],
            serde_json::json!("orchestration_cooldown_active")
        );
    }

    #[tokio::test]
    async fn remote_manual_orchestration_execute_acquires_active_execution_fence() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-remote-orchestration-fence"),
            None,
        ));
        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_remote_execution_fence",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("held transaction should acquire");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        let error = acquire_manual_orchestration_execution_fence_if_remote(
            &router,
            &state,
            &rule(),
            deadline,
            RouterFenceDisposition::Acquire,
        )
        .await;
        let error = match error {
            Ok(_) => panic!("remote manual execute must wait behind the active execution fence"),
            Err(error) => error,
        };

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        drop(held);
    }

    #[tokio::test]
    async fn orchestration_pause_waits_for_active_execution_fence() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-pause-fence"),
            None,
        ));
        let registered = state
            .register_orchestration_rule(rule())
            .await
            .expect("rule should register");
        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_rule_status_update_fence",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("held transaction should acquire");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        let error = update_orchestration_status_with_router_fence_disposition(
            &router,
            registered.id,
            deadline,
            &state,
            OrchestrationRuleStatus::Paused,
            RouterFenceDisposition::Acquire,
        )
        .await
        .expect_err("pause must fail closed while active execution fence is held");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            state
                .orchestration_rule(registered.id)
                .await
                .unwrap()
                .status,
            OrchestrationRuleStatus::Armed
        );
        drop(held);
    }

    #[tokio::test]
    async fn orchestration_pause_reuses_outer_router_transaction_without_queue_reentry() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-pause-reuse-fence"),
            None,
        ));
        let registered = state
            .register_orchestration_rule(rule())
            .await
            .expect("rule should register");
        let _outer_transaction = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_rule_status_update_outer_transaction",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("outer transaction should acquire");

        let payload = update_orchestration_status_with_router_fence_disposition(
            &router,
            registered.id,
            TransactionDeadline::new(500),
            &state,
            OrchestrationRuleStatus::Paused,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect(
            "pause should reuse the current router transaction instead of queue reentry timing out",
        );

        assert_eq!(
            payload["result"]["rule"]["status"],
            serde_json::json!("paused")
        );
    }

    #[tokio::test]
    async fn manual_remote_execute_reuses_outer_router_transaction_without_queue_reentry() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-manual-execute-reuse-fence"),
            None,
        ));
        state
            .set_orchestration_runtime(
                1,
                vec![
                    addressable_session("sess-source", "source", false),
                    addressable_session("sess-target", "target", false),
                ],
                true,
                true,
                None,
            )
            .await;
        let mut remote_rule = rule();
        remote_rule.condition = TriggerConditionSpec {
            kind: TriggerConditionKind::NetworkRequest,
            locator: None,
            text: None,
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        };
        let registered = state
            .register_orchestration_rule(remote_rule)
            .await
            .expect("rule should register");
        let _outer_transaction = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_manual_execute_outer_transaction",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("outer transaction should acquire");

        let error = cmd_orchestration_execute_with_router_fence_disposition(
            &router,
            registered.id,
            TransactionDeadline::new(500),
            &state,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect_err("manual execute should reuse the current router transaction instead of timing out on queue reentry");
        let envelope = error.into_envelope();
        assert!(
            envelope.code != ErrorCode::IpcTimeout,
            "manual execute should fail for target/probe authority, not queue reentry: {envelope:?}"
        );
    }
}
