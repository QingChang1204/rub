use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{TriggerConditionKind, TriggerInfo, TriggerRegistrationSpec, TriggerStatus};

use crate::runtime_refresh::refresh_live_trigger_runtime;
use crate::session::SessionState;

use super::super::DaemonRouter;
use super::super::frame_scope::ensure_tab_frame_available;
use super::super::request_args::parse_json_spec_value;
use super::command::{TriggerAddArgs, TriggerTraceArgs};
use super::projection::{
    resolve_trigger_tab_binding, trigger_payload, trigger_registration_reusable,
    trigger_registry_subject, trigger_status_name, trigger_subject,
};
use super::validation::validate_trigger_registration_spec;

pub(super) async fn cmd_trigger_add(
    router: &DaemonRouter,
    args: TriggerAddArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let mut spec =
        parse_json_spec_value::<TriggerRegistrationSpec>(args.spec.into_value(), "trigger add")?;
    validate_trigger_registration_spec(&mut spec)?;

    let tabs = refresh_live_trigger_runtime(&router.browser, state).await?;
    let browser = router.browser_port();
    let source_tab = resolve_trigger_tab_binding(
        &tabs,
        spec.source_tab,
        spec.source_frame_id.as_deref(),
        "source",
    )?;
    if let Some(frame_id) = source_tab.frame_id.as_deref() {
        ensure_tab_frame_available(&browser, &source_tab.target_id, frame_id, "source").await?;
    }
    let target_tab = resolve_trigger_tab_binding(
        &tabs,
        spec.target_tab,
        spec.target_frame_id.as_deref(),
        "target",
    )?;
    if let Some(frame_id) = target_tab.frame_id.as_deref() {
        ensure_tab_frame_available(&browser, &target_tab.target_id, frame_id, "target").await?;
    }

    let existing_trigger =
        state.triggers().await.into_iter().find(|trigger| {
            trigger_registration_reusable(trigger, &source_tab, &target_tab, &spec)
        });
    let trigger = if let Some(existing) = existing_trigger {
        if matches!(
            existing.condition.kind,
            TriggerConditionKind::NetworkRequest
        ) {
            state
                .ensure_trigger_network_request_baseline(
                    existing.id,
                    state.current_network_request_baseline().await,
                )
                .await
                .unwrap_or(existing)
        } else {
            existing
        }
    } else {
        let network_baseline = if !args.paused
            && matches!(spec.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            Some(state.current_network_request_baseline().await)
        } else {
            None
        };
        state
            .register_trigger_with_network_baseline(
                TriggerInfo {
                    id: 0,
                    status: if args.paused {
                        TriggerStatus::Paused
                    } else {
                        TriggerStatus::Armed
                    },
                    lifecycle_generation: 1,
                    mode: spec.mode,
                    source_tab,
                    target_tab,
                    condition: spec.condition,
                    action: spec.action,
                    last_condition_evidence: None,
                    consumed_evidence_fingerprint: None,
                    last_action_result: None,
                    unavailable_reason: None,
                },
                network_baseline,
            )
            .await
    };
    state.reconcile_trigger_runtime(&tabs).await;
    let runtime = state.trigger_runtime().await;
    let trigger = runtime
        .triggers
        .iter()
        .find(|entry| entry.id == trigger.id)
        .cloned()
        .unwrap_or(trigger);

    Ok(trigger_payload(
        trigger_subject(trigger.id),
        serde_json::json!({
            "trigger": trigger,
            "spec_source": args.spec_source.unwrap_or_else(|| serde_json::json!({ "kind": "inline" })),
        }),
        &runtime,
    ))
}

pub(super) async fn cmd_trigger_list(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    Ok(trigger_payload(
        trigger_registry_subject(),
        serde_json::json!({
            "items": runtime.triggers.clone(),
        }),
        &runtime,
    ))
}

pub(super) async fn cmd_trigger_trace(
    router: &DaemonRouter,
    args: TriggerTraceArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let last = usize::try_from(args.last).unwrap_or(usize::MAX);
    let runtime = state.trigger_runtime().await;
    let trace = state.trigger_trace(last).await;
    Ok(trigger_payload(
        serde_json::json!({
            "kind": "trigger_trace",
            "last": last,
        }),
        serde_json::to_value(trace).map_err(RubError::from)?,
        &runtime,
    ))
}

pub(super) async fn cmd_trigger_remove(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let removed = state.remove_trigger(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Trigger id {id} is not present in the current registry"),
        )
    })?;
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    Ok(trigger_payload(
        trigger_subject(id),
        serde_json::json!({
            "removed": removed,
        }),
        &runtime,
    ))
}

pub(super) async fn update_trigger_status(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
    next_status: TriggerStatus,
) -> Result<serde_json::Value, RubError> {
    let current = state
        .triggers()
        .await
        .into_iter()
        .find(|trigger| trigger.id == id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Trigger id {id} is not present in the current registry"),
            )
        })?;

    let trigger = match (current.status, next_status) {
        (TriggerStatus::Armed, TriggerStatus::Paused)
        | (TriggerStatus::Paused, TriggerStatus::Armed) => state
            .set_trigger_status_with_network_baseline(
                id,
                next_status,
                if matches!(next_status, TriggerStatus::Armed)
                    && matches!(current.condition.kind, TriggerConditionKind::NetworkRequest)
                {
                    Some(state.current_network_request_baseline().await)
                } else {
                    None
                },
            )
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Trigger id {id} disappeared while applying status update"
                ))
            })?,
        (status, requested) if status == requested => current,
        _ => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Trigger id {id} cannot transition from '{}' to '{}'",
                    trigger_status_name(current.status),
                    trigger_status_name(next_status),
                ),
            ));
        }
    };

    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    let trigger = runtime
        .triggers
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(trigger);
    Ok(trigger_payload(
        trigger_subject(id),
        serde_json::json!({
            "trigger": trigger,
        }),
        &runtime,
    ))
}
