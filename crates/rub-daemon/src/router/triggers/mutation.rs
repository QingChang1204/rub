use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{TriggerConditionKind, TriggerInfo, TriggerRegistrationSpec, TriggerStatus};

use crate::router::RouterFenceDisposition;
use crate::router::timeout_projection::record_registry_control_commit_timeout_projection;
use crate::runtime_refresh::refresh_live_trigger_runtime;
use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;
use crate::session::SessionState;

use super::super::DaemonRouter;
use super::super::TransactionDeadline;
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
    record_registry_control_commit_timeout_projection(
        "trigger",
        "add",
        "trigger",
        serde_json::json!(trigger),
    );
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
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_trigger_remove_with_router_fence_disposition(
        router,
        id,
        deadline,
        state,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await
}

async fn cmd_trigger_remove_with_router_fence_disposition(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<serde_json::Value, RubError> {
    let queue_wait_budget = std::time::Duration::from_millis(deadline.remaining_ms());
    let _active_execution_fence = router
        .begin_automation_transaction_if_needed(
            state,
            "trigger_rule_remove",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            router_fence_disposition,
        )
        .await
        .map_err(RubError::Domain)?;
    let removed = state.remove_trigger(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Trigger id {id} is not present in the current registry"),
        )
    })?;
    record_registry_control_commit_timeout_projection(
        "trigger",
        "remove",
        "removed",
        serde_json::json!(removed),
    );
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
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    next_status: TriggerStatus,
) -> Result<serde_json::Value, RubError> {
    update_trigger_status_with_router_fence_disposition(
        router,
        id,
        deadline,
        state,
        next_status,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await
}

async fn update_trigger_status_with_router_fence_disposition(
    router: &DaemonRouter,
    id: u32,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    next_status: TriggerStatus,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<serde_json::Value, RubError> {
    let queue_wait_budget = std::time::Duration::from_millis(deadline.remaining_ms());
    let _active_execution_fence = router
        .begin_automation_transaction_if_needed(
            state,
            "trigger_rule_status_update",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            router_fence_disposition,
        )
        .await
        .map_err(RubError::Domain)?;
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerStatus, TriggerTabBindingInfo,
    };

    use super::{
        cmd_trigger_remove_with_router_fence_disposition,
        update_trigger_status_with_router_fence_disposition,
    };
    use crate::router::{DaemonRouter, RouterFenceDisposition, TransactionDeadline};
    use crate::session::SessionState;

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

    fn temp_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rub-trigger-registry-fence-{name}"))
    }

    fn trigger(status: TriggerStatus) -> TriggerInfo {
        TriggerInfo {
            id: 0,
            status,
            lifecycle_generation: 1,
            mode: TriggerMode::Once,
            source_tab: tab_binding(0, "SOURCE_TAB"),
            target_tab: tab_binding(1, "TARGET_TAB"),
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::LocatorPresent,
                locator: Some(CanonicalLocator::Selector {
                    css: "#ready".to_string(),
                    selection: None,
                }),
                text: None,
                url_pattern: None,
                readiness_state: None,
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            action: TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(serde_json::json!({ "selector": "#submit" })),
            },
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        }
    }

    fn tab_binding(index: u32, target_id: &str) -> TriggerTabBindingInfo {
        TriggerTabBindingInfo {
            index,
            target_id: target_id.to_string(),
            frame_id: None,
            url: format!("https://example.com/{index}"),
            title: format!("Tab {index}"),
        }
    }

    #[tokio::test]
    async fn trigger_remove_waits_for_active_execution_fence() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("remove-active-fence"),
            None,
        ));
        let registered = state
            .register_trigger_with_network_baseline(trigger(TriggerStatus::Armed), None)
            .await;
        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_trigger_remove_fence",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("held transaction should acquire");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        let error = cmd_trigger_remove_with_router_fence_disposition(
            &router,
            registered.id,
            deadline,
            &state,
            RouterFenceDisposition::Acquire,
        )
        .await
        .expect_err("remove must fail closed while active execution fence is held");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, rub_core::error::ErrorCode::IpcTimeout);
        assert!(state.trigger_rule(registered.id).await.is_some());
        drop(held);
    }

    #[tokio::test]
    async fn trigger_remove_reuses_outer_router_transaction_without_queue_reentry() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("remove-reuse-fence"),
            None,
        ));
        let registered = state
            .register_trigger_with_network_baseline(trigger(TriggerStatus::Armed), None)
            .await;
        let _outer_transaction = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_trigger_remove_outer_transaction",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("outer transaction should acquire");

        let payload = cmd_trigger_remove_with_router_fence_disposition(
            &router,
            registered.id,
            TransactionDeadline::new(500),
            &state,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect("remove should reuse the current router transaction instead of queue reentry timing out");

        assert_eq!(
            payload["result"]["removed"]["id"],
            serde_json::json!(registered.id)
        );
        assert!(state.trigger_rule(registered.id).await.is_none());
    }

    #[tokio::test]
    async fn trigger_pause_waits_for_active_execution_fence() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("pause-active-fence"),
            None,
        ));
        let registered = state
            .register_trigger_with_network_baseline(trigger(TriggerStatus::Armed), None)
            .await;
        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_trigger_status_update_fence",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("held transaction should acquire");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        let error = update_trigger_status_with_router_fence_disposition(
            &router,
            registered.id,
            deadline,
            &state,
            TriggerStatus::Paused,
            RouterFenceDisposition::Acquire,
        )
        .await
        .expect_err("pause must fail closed while active execution fence is held");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, rub_core::error::ErrorCode::IpcTimeout);
        assert_eq!(
            state.trigger_rule(registered.id).await.unwrap().status,
            TriggerStatus::Armed
        );
        drop(held);
    }

    #[tokio::test]
    async fn trigger_pause_reuses_outer_router_transaction_without_queue_reentry() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("pause-reuse-fence"),
            None,
        ));
        let registered = state
            .register_trigger_with_network_baseline(trigger(TriggerStatus::Armed), None)
            .await;
        let _outer_transaction = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_trigger_status_update_outer_transaction",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("outer transaction should acquire");

        let payload = update_trigger_status_with_router_fence_disposition(
            &router,
            registered.id,
            TransactionDeadline::new(500),
            &state,
            TriggerStatus::Paused,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect(
            "pause should reuse the current router transaction instead of queue reentry timing out",
        );

        assert_eq!(
            payload["result"]["trigger"]["status"],
            serde_json::json!("paused")
        );
        assert_eq!(
            state.trigger_rule(registered.id).await.unwrap().status,
            TriggerStatus::Paused
        );
    }
}
