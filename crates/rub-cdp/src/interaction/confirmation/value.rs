use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId;
use rub_core::model::{InteractionConfirmation, InteractionConfirmationKind};
use serde_json::json;
use std::sync::Arc;
use tokio::time::Instant;

use super::support::{
    OBSERVATION_WINDOW, confirmed, contradicted, degraded, sleep_observation_step, unconfirmed,
};
use crate::interaction::observation::{
    PageObservation, editable_effect_contradicted, editable_effect_matches_expected,
    observe_element, observe_related_page, observed_editable_content, page_changed,
};

pub(crate) async fn confirm_input(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    expected: &str,
    before_page: PageObservation,
) -> InteractionConfirmation {
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        let observed = observe_element(page, object_id).await.ok();
        if let Some(observed) = observed.as_ref()
            && editable_effect_matches_expected(observed, expected)
        {
            return confirmed(
                InteractionConfirmationKind::ValueApplied,
                json!({
                    "editable_projection": observed.editable_projection,
                    "observed_content": observed_editable_content(observed),
                    "active": observed.active,
                }),
            );
        }

        if Instant::now() >= deadline {
            let after_page = observe_related_page(page, object_id).await;
            if observed.is_none() {
                return degraded(
                    Some(InteractionConfirmationKind::ValueApplied),
                    json!({
                        "expected": expected,
                        "observed": observed,
                        "context_changed": page_changed(&before_page, &after_page),
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }
            if let Some(observed) = observed.as_ref()
                && editable_effect_contradicted(observed, expected)
            {
                return contradicted(
                    InteractionConfirmationKind::ValueApplied,
                    json!({
                        "expected": expected,
                        "editable_projection": observed.editable_projection,
                        "observed_content": observed_editable_content(observed),
                        "observed": observed,
                    }),
                );
            }
            return unconfirmed(json!({
                "expected": expected,
                "observed": observed,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_select(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    expected_value: &str,
    expected_text: &str,
    before_page: PageObservation,
) -> InteractionConfirmation {
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        let observed = observe_element(page, object_id).await.ok();
        if let Some(observed) = observed.as_ref()
            && observed.value.as_deref() == Some(expected_value)
            && observed.selected_text.as_deref() == Some(expected_text)
        {
            return confirmed(
                InteractionConfirmationKind::SelectionApplied,
                json!({
                    "selected_value": observed.value,
                    "selected_text": observed.selected_text,
                }),
            );
        }

        if Instant::now() >= deadline {
            let after_page = observe_related_page(page, object_id).await;
            if observed.is_none() {
                return degraded(
                    Some(InteractionConfirmationKind::SelectionApplied),
                    json!({
                        "expected_value": expected_value,
                        "expected_text": expected_text,
                        "observed": observed,
                        "context_changed": page_changed(&before_page, &after_page),
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }
            if let Some(observed) = observed.as_ref()
                && (observed.value.as_deref() != Some(expected_value)
                    || observed.selected_text.as_deref() != Some(expected_text))
            {
                return contradicted(
                    InteractionConfirmationKind::SelectionApplied,
                    json!({
                        "expected_value": expected_value,
                        "expected_text": expected_text,
                        "observed": observed,
                    }),
                );
            }
            return unconfirmed(json!({
                "expected_value": expected_value,
                "expected_text": expected_text,
                "observed": observed,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}

pub(crate) async fn confirm_upload(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    expected_file_name: &str,
    before_page: PageObservation,
) -> InteractionConfirmation {
    let deadline = Instant::now() + OBSERVATION_WINDOW;
    let mut poll_count = 0u32;

    loop {
        let observed = observe_element(page, object_id).await.ok();
        if let Some(observed) = observed.as_ref()
            && observed
                .file_names
                .as_ref()
                .is_some_and(|names| names.iter().any(|name| name == expected_file_name))
        {
            return confirmed(
                InteractionConfirmationKind::FilesAttached,
                json!({
                    "file_names": observed.file_names,
                    "value": observed.value,
                }),
            );
        }

        if Instant::now() >= deadline {
            let after_page = observe_related_page(page, object_id).await;
            if observed.is_none() {
                return degraded(
                    Some(InteractionConfirmationKind::FilesAttached),
                    json!({
                        "expected_file_name": expected_file_name,
                        "observed": observed,
                        "context_changed": page_changed(&before_page, &after_page),
                        "before_page": before_page,
                        "after_page": after_page,
                    }),
                );
            }
            if let Some(observed) = observed.as_ref()
                && observed
                    .file_names
                    .as_ref()
                    .is_some_and(|names| !names.is_empty())
            {
                return contradicted(
                    InteractionConfirmationKind::FilesAttached,
                    json!({
                        "expected_file_name": expected_file_name,
                        "observed": observed,
                    }),
                );
            }
            return unconfirmed(json!({
                "expected_file_name": expected_file_name,
                "observed": observed,
            }));
        }

        sleep_observation_step(&mut poll_count).await;
    }
}
