use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::MouseButton;
use rub_core::InteractionOutcome;
use rub_core::error::RubError;
use rub_core::model::{Element, ElementTag, InteractionActuation, InteractionSemanticClass};
use std::sync::Arc;
use tokio::time::Duration;

use crate::dialogs::SharedDialogRuntime;
use crate::humanize::HumanizeConfig;
use crate::interaction::{ActuationFence, await_actuation_or_dialog};

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
    let expected_target_id = page.target_id().as_ref().to_string();

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
            &expected_target_id,
        )
        .await?;
        if let Some(confirmation) = crate::interaction::dialog_confirmation(
            dialog_runtime,
            &expected_target_id,
            &fence.dialog_baseline,
        )
        .await
        {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(confirmation),
            });
        }
        if matches!(fence.fence, ActuationFence::DialogOpened) {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
            });
        }
        if matches!(fence.fence, ActuationFence::Indeterminate) {
            return Ok(InteractionOutcome {
                semantic_class: click_semantic_class(element.tag),
                element_verified: resolved.verified,
                actuation: Some(InteractionActuation::Semantic),
                confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                    "semantic_click",
                )),
            });
        }
        let confirmation = crate::interaction::confirm_click(
            page,
            &resolved.remote_object_id,
            element.tag,
            baseline,
            dialog_runtime,
            &fence.dialog_baseline,
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
        &expected_target_id,
    )
    .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "pointer_click",
            )),
        });
    }
    let confirmation = crate::interaction::confirm_click(
        page,
        &resolved.remote_object_id,
        element.tag,
        baseline,
        dialog_runtime,
        &fence.dialog_baseline,
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
    let expected_target_id = page.target_id().as_ref().to_string();
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence =
        await_actuation_or_dialog(
            async move {
                crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Left, 1).await
            },
            dialog_runtime.clone(),
            "pointer_click_xy",
            &expected_target_id,
        )
        .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "pointer_click_xy",
            )),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(
        page,
        baseline,
        dialog_runtime,
        &fence.dialog_baseline,
    )
    .await;
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
    let expected_target_id = page.target_id().as_ref().to_string();
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence =
        await_actuation_or_dialog(
            async move {
                crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Left, 2).await
            },
            dialog_runtime.clone(),
            "pointer_dblclick_xy",
            &expected_target_id,
        )
        .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "pointer_dblclick_xy",
            )),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(
        page,
        baseline,
        dialog_runtime,
        &fence.dialog_baseline,
    )
    .await;
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
    let expected_target_id = page.target_id().as_ref().to_string();
    crate::pointer::move_to(page, x, y, humanize).await?;
    let page_for_click = page.clone();
    let fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(&page_for_click, x, y, MouseButton::Right, 1).await
        },
        dialog_runtime.clone(),
        "pointer_rightclick_xy",
        &expected_target_id,
    )
    .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: false,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "pointer_rightclick_xy",
            )),
        });
    }
    let confirmation = crate::interaction::confirm_click_xy(
        page,
        baseline,
        dialog_runtime,
        &fence.dialog_baseline,
    )
    .await;
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
    let expected_target_id = page.target_id().as_ref().to_string();

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
        &expected_target_id,
    )
    .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &first_click_fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(first_click_fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(first_click_fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
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
        &expected_target_id,
    )
    .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &second_click_fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(second_click_fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(second_click_fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
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
        &second_click_fence.dialog_baseline,
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
    crate::interaction::ensure_activation_target_enabled(page, &resolved.remote_object_id).await?;
    if target.remote_object_id != resolved.remote_object_id {
        crate::interaction::ensure_activation_target_enabled(page, &target.remote_object_id)
            .await?;
    }
    let point = crate::targeting::resolve_pointer_point(page, &target).await?;
    let baseline =
        crate::interaction::capture_interaction_baseline(page, &resolved.remote_object_id).await;
    let expected_target_id = page.target_id().as_ref().to_string();

    crate::pointer::move_to(page, point.x, point.y, humanize).await?;
    let page_for_click = page.clone();
    let fence = await_actuation_or_dialog(
        async move {
            crate::pointer::dispatch_click(&page_for_click, point.x, point.y, MouseButton::Right, 1)
                .await
        },
        dialog_runtime.clone(),
        "pointer_rightclick",
        &expected_target_id,
    )
    .await?;
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(confirmation),
        });
    }
    if matches!(fence.fence, ActuationFence::DialogOpened) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(fence.fence, ActuationFence::Indeterminate) {
        return Ok(InteractionOutcome {
            semantic_class: click_semantic_class(element.tag),
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "pointer_rightclick",
            )),
        });
    }

    let confirmation = crate::interaction::confirm_click(
        page,
        &resolved.remote_object_id,
        element.tag,
        baseline,
        dialog_runtime,
        &fence.dialog_baseline,
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

#[cfg(test)]
mod tests;
