use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::MouseButton;
use rub_core::InteractionOutcome;
use rub_core::error::RubError;
use rub_core::model::{
    Element, ElementTag, InteractionActuation, InteractionConfirmation,
    InteractionConfirmationKind, InteractionConfirmationStatus, InteractionSemanticClass,
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use tracing::info;

use crate::dialogs::SharedDialogRuntime;
use crate::humanize::HumanizeConfig;

const DIALOG_ACTUATION_TIMEOUT: Duration = Duration::from_millis(500);
const DIALOG_ACTUATION_GRACE_PERIOD: Duration = Duration::from_millis(500);
const DIALOG_ACTUATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) async fn click(
    page: &Arc<Page>,
    element: &Element,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    let activation_target =
        crate::targeting::resolve_activation_target(page, &resolved, element.tag).await?;
    crate::interaction::ensure_activation_target_enabled(page, &resolved.remote_object_id).await?;
    if activation_target.remote_object_id != resolved.remote_object_id {
        crate::interaction::ensure_activation_target_enabled(
            page,
            &activation_target.remote_object_id,
        )
        .await?;
    }
    let baseline =
        crate::interaction::capture_interaction_baseline(page, &resolved.remote_object_id).await;

    if prefers_semantic_click(element.tag, humanize.enabled) {
        // Semantic `.click()` still needs the same visibility/occlusion fence as a
        // pointer click. Otherwise hidden or covered elements can bypass the
        // interactability authority simply because they have a DOM `.click()`.
        let _ = crate::targeting::resolve_pointer_point(page, &activation_target).await?;
        let page_for_click = page.clone();
        let target_object_id = activation_target.remote_object_id.clone();
        let fence = await_actuation_or_dialog(
            async move {
                crate::js::call_function(
                    &page_for_click,
                    &target_object_id,
                    "function() { this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); this.click(); }",
                    false,
                )
                .await
            },
            dialog_runtime.clone(),
            "semantic_click",
        )
        .await?;
        if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(confirmation),
            });
        }
        if matches!(fence, ActuationFence::DialogOpened) {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(unconfirmed_dialog_opening()),
            });
        }
        if matches!(fence, ActuationFence::Indeterminate) {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(indeterminate_actuation_confirmation("semantic_click")),
            });
        }
        let confirmation = crate::interaction::confirm_click(
            page,
            &resolved.remote_object_id,
            element.tag,
            baseline,
            dialog_runtime,
        )
        .await;
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Semantic),
            confirmation: Some(confirmation),
        });
    }

    let pointer_point = crate::targeting::resolve_pointer_point(page, &activation_target).await?;
    crate::pointer::move_to(page, pointer_point.x, pointer_point.y, humanize).await?;
    let page_for_click = page.clone();
    let fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(
                &page_for_click,
                pointer_point.x,
                pointer_point.y,
                MouseButton::Left,
                1,
            )
            .await
        },
        dialog_runtime.clone(),
        "pointer_click",
    )
    .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation("pointer_click")),
        });
    }
    let confirmation = crate::interaction::confirm_click(
        page,
        &resolved.remote_object_id,
        element.tag,
        baseline,
        dialog_runtime,
    )
    .await;

    Ok(InteractionOutcome {
        semantic_class: click_semantic_class(element.tag),
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn click_xy(
    page: &Arc<Page>,
    x: f64,
    y: f64,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let baseline = crate::interaction::capture_page_baseline(page).await;
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence =
        await_actuation_or_dialog(
            async move {
                crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Left, 1).await
            },
            dialog_runtime.clone(),
            "pointer_click_xy",
        )
        .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation("pointer_click_xy")),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(page, baseline, dialog_runtime).await;
    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: false,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn dblclick_xy(
    page: &Arc<Page>,
    x: f64,
    y: f64,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let baseline = crate::interaction::capture_page_baseline(page).await;
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence =
        await_actuation_or_dialog(
            async move {
                crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Left, 2).await
            },
            dialog_runtime.clone(),
            "pointer_dblclick_xy",
        )
        .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation("pointer_dblclick_xy")),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(page, baseline, dialog_runtime).await;
    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: false,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn rightclick_xy(
    page: &Arc<Page>,
    x: f64,
    y: f64,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let baseline = crate::interaction::capture_page_baseline(page).await;
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Right, 1).await
        },
        dialog_runtime.clone(),
        "pointer_rightclick_xy",
    )
    .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation(
                "pointer_rightclick_xy",
            )),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(page, baseline, dialog_runtime).await;
    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: false,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn hover(
    page: &Arc<Page>,
    element: &Element,
    humanize: &HumanizeConfig,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    let target = crate::targeting::resolve_activation_target(page, &resolved, element.tag).await?;
    crate::interaction::ensure_activation_target_enabled(page, &resolved.remote_object_id).await?;
    if target.remote_object_id != resolved.remote_object_id {
        crate::interaction::ensure_activation_target_enabled(page, &target.remote_object_id)
            .await?;
    }
    let point = crate::targeting::resolve_pointer_point(page, &target).await?;
    let baseline =
        crate::interaction::capture_interaction_baseline(page, &resolved.remote_object_id).await;

    crate::pointer::move_to(page, point.x, point.y, humanize).await?;
    let confirmation =
        crate::interaction::confirm_hover(page, &resolved.remote_object_id, baseline).await;

    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::Hover,
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn dblclick(
    page: &Arc<Page>,
    element: &Element,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    let target = crate::targeting::resolve_activation_target(page, &resolved, element.tag).await?;
    crate::interaction::ensure_activation_target_enabled(page, &resolved.remote_object_id).await?;
    if target.remote_object_id != resolved.remote_object_id {
        crate::interaction::ensure_activation_target_enabled(page, &target.remote_object_id)
            .await?;
    }
    let point = crate::targeting::resolve_pointer_point(page, &target).await?;
    let baseline =
        crate::interaction::capture_interaction_baseline(page, &resolved.remote_object_id).await;

    crate::pointer::move_to(page, point.x, point.y, humanize).await?;
    let page_for_first_click = page.clone();
    let first_click_fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(
                &page_for_first_click,
                point.x,
                point.y,
                MouseButton::Left,
                1,
            )
            .await
        },
        dialog_runtime.clone(),
        "pointer_dblclick_first",
    )
    .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(first_click_fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(first_click_fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation(
                "pointer_dblclick_first",
            )),
        });
    }
    if humanize.enabled {
        tokio::time::sleep(Duration::from_millis(crate::humanize::random_delay(
            30, 120,
        )))
        .await;
    }
    let page_for_second_click = page.clone();
    let second_click_fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(
                &page_for_second_click,
                point.x,
                point.y,
                MouseButton::Left,
                2,
            )
            .await
        },
        dialog_runtime.clone(),
        "pointer_dblclick_second",
    )
    .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(second_click_fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(second_click_fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation(
                "pointer_dblclick_second",
            )),
        });
    }

    let confirmation = crate::interaction::confirm_click(
        page,
        &resolved.remote_object_id,
        element.tag,
        baseline,
        dialog_runtime,
    )
    .await;

    Ok(InteractionOutcome {
        semantic_class: click_semantic_class(element.tag),
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn rightclick(
    page: &Arc<Page>,
    element: &Element,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    let target = crate::targeting::resolve_activation_target(page, &resolved, element.tag).await?;
    let point = crate::targeting::resolve_pointer_point(page, &target).await?;
    let baseline =
        crate::interaction::capture_interaction_baseline(page, &resolved.remote_object_id).await;

    crate::pointer::move_to(page, point.x, point.y, humanize).await?;
    let page_for_click = page.clone();
    let fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(&page_for_click, point.x, point.y, MouseButton::Right, 1)
                .await
        },
        dialog_runtime.clone(),
        "pointer_rightclick",
    )
    .await?;
    if let Some(confirmation) = dialog_confirmation(dialog_runtime).await {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(indeterminate_actuation_confirmation("pointer_rightclick")),
        });
    }

    let confirmation = crate::interaction::confirm_click(
        page,
        &resolved.remote_object_id,
        element.tag,
        baseline,
        dialog_runtime,
    )
    .await;

    Ok(InteractionOutcome {
        semantic_class: click_semantic_class(element.tag),
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(confirmation),
    })
}

fn prefers_semantic_click(tag: ElementTag, humanize_enabled: bool) -> bool {
    if humanize_enabled {
        return false;
    }

    matches!(
        tag,
        ElementTag::Button | ElementTag::Link | ElementTag::Checkbox | ElementTag::Radio
    )
}

fn click_semantic_class(tag: ElementTag) -> InteractionSemanticClass {
    match tag {
        ElementTag::Checkbox | ElementTag::Radio => InteractionSemanticClass::ToggleState,
        _ => InteractionSemanticClass::Activate,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActuationFence {
    Completed,
    DialogOpened,
    Indeterminate,
}

async fn await_actuation_or_dialog<F>(
    actuation: F,
    dialog_runtime: SharedDialogRuntime,
    label: &'static str,
) -> Result<ActuationFence, RubError>
where
    F: std::future::Future<Output = Result<(), RubError>> + Send + 'static,
{
    let mut handle = tokio::spawn(actuation);
    match tokio::time::timeout(DIALOG_ACTUATION_TIMEOUT, &mut handle).await {
        Ok(joined) => {
            joined
                .map_err(|error| RubError::Internal(format!("{label} task failed: {error}")))??;
            Ok(ActuationFence::Completed)
        }
        Err(_) => {
            info!(
                actuation = label,
                "Interaction actuation timed out; waiting for a truthful post-timeout fence instead of assuming rollback"
            );
            let deadline = Instant::now() + DIALOG_ACTUATION_GRACE_PERIOD;
            loop {
                if crate::dialogs::pending_dialog(&dialog_runtime)
                    .await
                    .is_some()
                {
                    info!(
                        actuation = label,
                        "Dialog fallback became active after actuation timeout"
                    );
                    return Ok(ActuationFence::DialogOpened);
                }
                if handle.is_finished() {
                    let joined = handle.await.map_err(|error| {
                        RubError::Internal(format!("{label} task failed after timeout: {error}"))
                    })?;
                    joined?;
                    return Ok(ActuationFence::Completed);
                }
                if Instant::now() >= deadline {
                    return Ok(ActuationFence::Indeterminate);
                }
                tokio::time::sleep(DIALOG_ACTUATION_POLL_INTERVAL).await;
            }
        }
    }
}

async fn dialog_confirmation(
    dialog_runtime: &SharedDialogRuntime,
) -> Option<InteractionConfirmation> {
    let dialog = crate::dialogs::pending_dialog(dialog_runtime).await?;
    Some(InteractionConfirmation {
        status: InteractionConfirmationStatus::Confirmed,
        kind: Some(InteractionConfirmationKind::DialogOpened),
        details: Some(json!({
            "kind": dialog.kind,
            "message": dialog.message,
            "url": dialog.url,
            "frame_id": dialog.frame_id,
            "default_prompt": dialog.default_prompt,
        })),
    })
}

fn unconfirmed_dialog_opening() -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Unconfirmed,
        kind: Some(InteractionConfirmationKind::DialogOpened),
        details: None,
    }
}

fn indeterminate_actuation_confirmation(label: &'static str) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Degraded,
        kind: None,
        details: Some(json!({
            "reason": "actuation_commit_fence_indeterminate",
            "actuation": label,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActuationFence, DIALOG_ACTUATION_GRACE_PERIOD, DIALOG_ACTUATION_TIMEOUT,
        await_actuation_or_dialog,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::{Duration, sleep};

    #[tokio::test]
    async fn timed_out_actuation_reports_indeterminate_when_browser_side_commit_may_still_land() {
        let committed = Arc::new(AtomicBool::new(false));
        let committed_task = committed.clone();

        let result = await_actuation_or_dialog(
            async move {
                sleep(DIALOG_ACTUATION_TIMEOUT + Duration::from_millis(250)).await;
                committed_task.store(true, Ordering::SeqCst);
                Ok(())
            },
            crate::dialogs::new_shared_dialog_runtime(),
            "click",
        )
        .await;

        assert!(
            matches!(
                result.expect("timed out actuation should stay truthful"),
                ActuationFence::Completed | ActuationFence::Indeterminate
            ),
            "local timeout must not fabricate a rollback fence"
        );
        sleep(DIALOG_ACTUATION_GRACE_PERIOD + Duration::from_millis(300)).await;
        assert!(
            committed.load(Ordering::SeqCst),
            "browser-side actuation may still late-commit after the local timeout fence"
        );
    }
}
