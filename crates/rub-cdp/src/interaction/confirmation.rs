mod support;
mod value;

use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::{ExecutionContextId, RemoteObjectId};
use rub_core::error::RubError;
use rub_core::model::{
    ElementTag, InteractionConfirmation, InteractionConfirmationKind, InteractionConfirmationStatus,
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use tracing::info;

use self::support::{
    OBSERVATION_WINDOW, confirmed, contradicted, degraded, sleep_observation_step, unconfirmed,
};
pub(crate) use self::value::{confirm_input, confirm_select, confirm_upload};
use super::observation::{
    ActiveInteractionBaseline, InteractionBaseline, active_element_changed, active_element_matches,
    confirmation_observation_degraded, element_state_changed, observe_active_element,
    observe_active_element_in_context, observe_element, observe_page, observe_page_in_context,
    observe_related_page, page_changed, page_mutated, typed_effect_contradicted,
    typed_effect_observed,
};
use crate::dialogs::{SharedDialogRuntime, pending_dialog_for_target};

pub(crate) const DIALOG_ACTUATION_TIMEOUT: Duration = Duration::from_millis(500);
pub(crate) const DIALOG_ACTUATION_GRACE_PERIOD: Duration = Duration::from_millis(500);
const DIALOG_ACTUATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActuationFence {
    Completed,
    DialogOpened,
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DialogFenceBaseline {
    previous_opened_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActuationFenceOutcome {
    pub(crate) fence: ActuationFence,
    pub(crate) dialog_baseline: DialogFenceBaseline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActuationResultFenceOutcome<T> {
    pub(crate) result: Option<T>,
    pub(crate) fence: ActuationFence,
    pub(crate) dialog_baseline: DialogFenceBaseline,
}

pub(crate) async fn capture_dialog_fence_baseline(
    dialog_runtime: &SharedDialogRuntime,
    expected_target_id: &str,
) -> DialogFenceBaseline {
    DialogFenceBaseline {
        previous_opened_at: pending_dialog_for_target(dialog_runtime, expected_target_id)
            .await
            .map(|dialog| dialog.opened_at),
    }
}

fn dialog_is_new_since_baseline(
    dialog: &rub_core::model::PendingDialogInfo,
    baseline: &DialogFenceBaseline,
) -> bool {
    baseline.previous_opened_at.as_deref() != Some(dialog.opened_at.as_str())
}

async fn pending_dialog_for_target_since(
    dialog_runtime: &SharedDialogRuntime,
    expected_target_id: &str,
    baseline: &DialogFenceBaseline,
) -> Option<rub_core::model::PendingDialogInfo> {
    let dialog = pending_dialog_for_target(dialog_runtime, expected_target_id).await?;
    dialog_is_new_since_baseline(&dialog, baseline).then_some(dialog)
}

pub(crate) async fn await_actuation_or_dialog<F>(
    actuation: F,
    dialog_runtime: SharedDialogRuntime,
    label: &'static str,
    expected_target_id: &str,
) -> Result<ActuationFenceOutcome, RubError>
where
    F: std::future::Future<Output = Result<(), RubError>> + Send + 'static,
{
    let outcome =
        await_actuation_result_or_dialog(actuation, dialog_runtime, label, expected_target_id)
            .await?;
    Ok(ActuationFenceOutcome {
        fence: outcome.fence,
        dialog_baseline: outcome.dialog_baseline,
    })
}

pub(crate) async fn await_actuation_result_or_dialog<F, T>(
    actuation: F,
    dialog_runtime: SharedDialogRuntime,
    label: &'static str,
    expected_target_id: &str,
) -> Result<ActuationResultFenceOutcome<T>, RubError>
where
    F: std::future::Future<Output = Result<T, RubError>> + Send + 'static,
    T: Send + 'static,
{
    let dialog_baseline = capture_dialog_fence_baseline(&dialog_runtime, expected_target_id).await;
    let mut handle = tokio::spawn(actuation);
    match tokio::time::timeout(DIALOG_ACTUATION_TIMEOUT, &mut handle).await {
        Ok(joined) => {
            let result = joined
                .map_err(|error| RubError::Internal(format!("{label} task failed: {error}")))??;
            Ok(ActuationResultFenceOutcome {
                result: Some(result),
                fence: ActuationFence::Completed,
                dialog_baseline,
            })
        }
        Err(_) => {
            info!(
                actuation = label,
                "Interaction actuation timed out; aborting the local actuation task and waiting for a truthful post-timeout dialog fence"
            );
            handle.abort();
            let _ = handle.await;
            let deadline = Instant::now() + DIALOG_ACTUATION_GRACE_PERIOD;
            loop {
                if pending_dialog_for_target_since(
                    &dialog_runtime,
                    expected_target_id,
                    &dialog_baseline,
                )
                .await
                .is_some()
                {
                    info!(
                        actuation = label,
                        "Dialog fallback became active after actuation timeout"
                    );
                    return Ok(ActuationResultFenceOutcome {
                        result: None,
                        fence: ActuationFence::DialogOpened,
                        dialog_baseline,
                    });
                }
                if Instant::now() >= deadline {
                    return Ok(ActuationResultFenceOutcome {
                        result: None,
                        fence: ActuationFence::Indeterminate,
                        dialog_baseline,
                    });
                }
                tokio::time::sleep(DIALOG_ACTUATION_POLL_INTERVAL).await;
            }
        }
    }
}

pub(crate) async fn dialog_confirmation(
    dialog_runtime: &SharedDialogRuntime,
    expected_target_id: &str,
    dialog_baseline: &DialogFenceBaseline,
) -> Option<InteractionConfirmation> {
    let dialog =
        pending_dialog_for_target_since(dialog_runtime, expected_target_id, dialog_baseline)
            .await?;
    Some(InteractionConfirmation {
        status: InteractionConfirmationStatus::Confirmed,
        kind: Some(InteractionConfirmationKind::DialogOpened),
        details: Some(json!({
            "kind": dialog.kind,
            "message": dialog.message,
            "url": dialog.url,
            "frame_id": dialog.frame_id,
            "default_prompt": dialog.default_prompt,
            "opened_at": dialog.opened_at,
        })),
    })
}

pub(crate) fn unconfirmed_dialog_opening() -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Unconfirmed,
        kind: Some(InteractionConfirmationKind::DialogOpened),
        details: None,
    }
}

pub(crate) fn indeterminate_actuation_confirmation(label: &'static str) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Degraded,
        kind: None,
        details: Some(json!({
            "reason": "actuation_commit_fence_indeterminate",
            "actuation": label,
        })),
    }
}

pub(crate) async fn confirm_click(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    tag: ElementTag,
    baseline: InteractionBaseline,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    let target_id = page.target_id().as_ref().to_string();
    let before_element = baseline.before_element;
    let before_page = baseline.before_page;
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut pending_focus_change: Option<serde_json::Value> = None;
    let mut poll_count = 0u32;

    loop {
        if let Some(dialog) =
            pending_dialog_for_target_since(dialog_runtime, &target_id, dialog_baseline).await
        {
            return confirmed(
                InteractionConfirmationKind::DialogOpened,
                json!({
                    "kind": dialog.kind,
                    "message": dialog.message,
                    "url": dialog.url,
                    "frame_id": dialog.frame_id,
                    "default_prompt": dialog.default_prompt,
                    "opened_at": dialog.opened_at,
                }),
            );
        }

        if matches!(tag, ElementTag::Checkbox | ElementTag::Radio)
            && let (Some(before), Some(after)) = (
                before_element.as_ref(),
                observe_element(page, object_id).await.ok(),
            )
            && before.checked != after.checked
        {
            return confirmed(
                InteractionConfirmationKind::ToggleState,
                json!({
                    "before_checked": before.checked,
                    "after_checked": after.checked,
                }),
            );
        }

        if let (Some(before), Some(after)) = (
            before_element.as_ref(),
            observe_element(page, object_id).await.ok(),
        ) {
            if !before.active && after.active {
                pending_focus_change = Some(json!({
                    "before_active": before.active,
                    "after_active": after.active,
                }));
            }

            if element_state_changed(before, &after) {
                return confirmed(
                    InteractionConfirmationKind::ElementStateChange,
                    json!({
                        "before": before,
                        "after": after,
                    }),
                );
            }
        }

        let after_page = observe_related_page(page, object_id).await;
        if page_changed(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::ContextChange,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if page_mutated(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::PageMutation,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if Instant::now() >= deadline {
            let after_element = observe_element(page, object_id).await.ok();
            if matches!(tag, ElementTag::Checkbox | ElementTag::Radio)
                && let (Some(before), Some(after)) =
                    (before_element.as_ref(), after_element.as_ref())
                && before.checked == after.checked
            {
                return contradicted(
                    InteractionConfirmationKind::ToggleState,
                    json!({
                        "before_checked": before.checked,
                        "after_checked": after.checked,
                    }),
                );
            }
            if let Some(details) = pending_focus_change {
                return confirmed(InteractionConfirmationKind::FocusChange, details);
            }
            if confirmation_observation_degraded(
                &before_page,
                &after_page,
                before_element.is_some(),
                after_element.is_some(),
            ) {
                return degraded(
                    matches!(tag, ElementTag::Checkbox | ElementTag::Radio)
                        .then_some(InteractionConfirmationKind::ToggleState),
                    json!({
                        "before_element": before_element,
                        "after_element": after_element,
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }
            return unconfirmed(json!({
                "before_element": before_element,
                "after_element": after_element,
                "before_page": before_page,
                "after_page": after_page,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_click_xy(
    page: &Arc<Page>,
    before_page: super::observation::PageObservation,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    let target_id = page.target_id().as_ref().to_string();
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        if let Some(dialog) =
            pending_dialog_for_target_since(dialog_runtime, &target_id, dialog_baseline).await
        {
            return confirmed(
                InteractionConfirmationKind::DialogOpened,
                json!({
                    "kind": dialog.kind,
                    "message": dialog.message,
                    "url": dialog.url,
                    "frame_id": dialog.frame_id,
                    "default_prompt": dialog.default_prompt,
                    "opened_at": dialog.opened_at,
                }),
            );
        }

        let after_page = observe_page(page).await;
        if page_changed(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::ContextChange,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if page_mutated(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::PageMutation,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if Instant::now() >= deadline {
            if !after_page.available {
                return degraded(
                    Some(InteractionConfirmationKind::PageMutation),
                    json!({
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }

            return unconfirmed(json!({
                "before_page": before_page,
                "after_page": after_page,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_hover(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    baseline: InteractionBaseline,
) -> InteractionConfirmation {
    let before_element = baseline.before_element;
    let before_page = baseline.before_page;
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        if let (Some(before), Some(after)) = (
            before_element.as_ref(),
            observe_element(page, object_id).await.ok(),
        ) {
            if !before.hovered && after.hovered {
                return confirmed(
                    InteractionConfirmationKind::HoverState,
                    json!({
                        "before_hovered": before.hovered,
                        "after_hovered": after.hovered,
                    }),
                );
            }

            if element_state_changed(before, &after) {
                return confirmed(
                    InteractionConfirmationKind::ElementStateChange,
                    json!({
                        "before": before,
                        "after": after,
                    }),
                );
            }
        }

        let after_page = observe_related_page(page, object_id).await;
        if page_changed(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::ContextChange,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if page_mutated(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::PageMutation,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if Instant::now() >= deadline {
            let after_element = observe_element(page, object_id).await.ok();
            if let Some(after) = after_element.as_ref()
                && !after.hovered
            {
                return contradicted(
                    InteractionConfirmationKind::HoverState,
                    json!({
                        "before_element": before_element,
                        "after_element": after_element,
                    }),
                );
            }
            if confirmation_observation_degraded(
                &before_page,
                &after_page,
                before_element.is_some(),
                after_element.is_some(),
            ) {
                return degraded(
                    Some(InteractionConfirmationKind::HoverState),
                    json!({
                        "before_element": before_element,
                        "after_element": after_element,
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }
            return unconfirmed(json!({
                "before_element": before_element,
                "after_element": after_element,
                "before_page": before_page,
                "after_page": after_page,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_key_combo(
    page: &Arc<Page>,
    baseline: ActiveInteractionBaseline,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    confirm_key_combo_in_context(page, baseline, None, dialog_runtime, dialog_baseline).await
}

pub(crate) async fn confirm_key_combo_in_context(
    page: &Arc<Page>,
    baseline: ActiveInteractionBaseline,
    context_id: Option<ExecutionContextId>,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    let target_id = page.target_id().as_ref().to_string();
    let before_active = baseline.before_active;
    let before_page = baseline.before_page;
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        if let Some(confirmation) =
            dialog_confirmation(dialog_runtime, &target_id, dialog_baseline).await
        {
            return confirmation;
        }

        let after_active = match context_id {
            Some(context_id) => observe_active_element_in_context(page, Some(context_id))
                .await
                .ok(),
            None => observe_active_element(page).await.ok(),
        };
        if let (Some(before), Some(after)) = (before_active.as_ref(), after_active.as_ref()) {
            if active_element_changed(before, after) {
                return confirmed(
                    InteractionConfirmationKind::FocusChange,
                    json!({
                        "before_active": before.observation,
                        "after_active": after.observation,
                    }),
                );
            }

            if before.observation.checked != after.observation.checked {
                return confirmed(
                    InteractionConfirmationKind::ToggleState,
                    json!({
                        "before_checked": before.observation.checked,
                        "after_checked": after.observation.checked,
                    }),
                );
            }

            if element_state_changed(&before.observation, &after.observation) {
                return confirmed(
                    InteractionConfirmationKind::ElementStateChange,
                    json!({
                        "before": before.observation,
                        "after": after.observation,
                    }),
                );
            }
        }

        let after_page = match context_id {
            Some(context_id) => observe_page_in_context(page, Some(context_id)).await,
            None => observe_page(page).await,
        };
        if page_changed(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::ContextChange,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if page_mutated(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::PageMutation,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if Instant::now() >= deadline {
            if confirmation_observation_degraded(
                &before_page,
                &after_page,
                before_active.is_some(),
                after_active.is_some(),
            ) {
                return degraded(
                    Some(InteractionConfirmationKind::ElementStateChange),
                    json!({
                        "before_active": before_active.as_ref().map(|active| &active.observation),
                        "after_active": after_active.as_ref().map(|active| &active.observation),
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }

            return unconfirmed(json!({
                "before_active": before_active.as_ref().map(|active| &active.observation),
                "after_active": after_active.as_ref().map(|active| &active.observation),
                "before_page": before_page,
                "after_page": after_page,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_typed_text(
    page: &Arc<Page>,
    typed_text: &str,
    baseline: ActiveInteractionBaseline,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    confirm_typed_text_in_context(
        page,
        typed_text,
        baseline,
        None,
        dialog_runtime,
        dialog_baseline,
    )
    .await
}

pub(crate) async fn confirm_typed_text_in_context(
    page: &Arc<Page>,
    typed_text: &str,
    baseline: ActiveInteractionBaseline,
    context_id: Option<ExecutionContextId>,
    dialog_runtime: &SharedDialogRuntime,
    dialog_baseline: &DialogFenceBaseline,
) -> InteractionConfirmation {
    let target_id = page.target_id().as_ref().to_string();
    let before_active = baseline.before_active;
    let before_page = baseline.before_page;
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        if let Some(confirmation) =
            dialog_confirmation(dialog_runtime, &target_id, dialog_baseline).await
        {
            return confirmation;
        }

        let after_active = observe_active_element_in_context(page, context_id)
            .await
            .ok();
        if let (Some(before), Some(after)) = (before_active.as_ref(), after_active.as_ref())
            && active_element_matches(before, after)
            && typed_effect_observed(&before.observation, &after.observation, typed_text)
        {
            return confirmed(
                InteractionConfirmationKind::ValueApplied,
                json!({
                    "typed_text": typed_text,
                    "before": before.observation,
                    "after": after.observation,
                }),
            );
        }

        let after_page = observe_page_in_context(page, context_id).await;
        if page_changed(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::ContextChange,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if page_mutated(&before_page, &after_page) {
            return confirmed(
                InteractionConfirmationKind::PageMutation,
                json!({
                    "before": before_page,
                    "after": after_page,
                }),
            );
        }

        if Instant::now() >= deadline {
            if confirmation_observation_degraded(
                &before_page,
                &after_page,
                before_active.is_some(),
                after_active.is_some(),
            ) {
                return degraded(
                    Some(InteractionConfirmationKind::ValueApplied),
                    json!({
                        "typed_text": typed_text,
                        "before_active": before_active.as_ref().map(|active| &active.observation),
                        "after_active": after_active.as_ref().map(|active| &active.observation),
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }

            if let (Some(before), Some(after)) = (before_active.as_ref(), after_active.as_ref())
                && active_element_matches(before, after)
                && typed_effect_contradicted(&before.observation, &after.observation, typed_text)
            {
                return contradicted(
                    InteractionConfirmationKind::ValueApplied,
                    json!({
                        "typed_text": typed_text,
                        "before": before.observation,
                        "observed": after.observation,
                    }),
                );
            }

            return unconfirmed(json!({
                "typed_text": typed_text,
                "before_active": before_active.as_ref().map(|active| &active.observation),
                "after_active": after_active.as_ref().map(|active| &active.observation),
                "before_page": before_page,
                "after_page": after_page,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

#[cfg(test)]
mod tests;
