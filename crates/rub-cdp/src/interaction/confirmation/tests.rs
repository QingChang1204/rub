use super::support::{OBSERVATION_BACKOFF_CEILING, OBSERVATION_INTERVAL, observation_poll_delay};
use super::{
    ActuationFence, DialogFenceBaseline, await_actuation_result_or_dialog, dialog_confirmation,
};
use crate::dialogs::new_shared_dialog_runtime;
use rub_core::model::{
    DialogKind, InteractionConfirmationKind, InteractionConfirmationStatus, PendingDialogInfo,
};
use std::time::Duration;

#[test]
fn observation_poll_delay_uses_bounded_backoff() {
    assert_eq!(observation_poll_delay(0), OBSERVATION_INTERVAL);
    assert_eq!(observation_poll_delay(1), Duration::from_millis(50));
    assert_eq!(observation_poll_delay(2), Duration::from_millis(75));
    assert_eq!(observation_poll_delay(3), OBSERVATION_BACKOFF_CEILING);
    assert_eq!(observation_poll_delay(8), OBSERVATION_BACKOFF_CEILING);
}

#[tokio::test]
async fn dialog_confirmation_is_target_scoped() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Hello".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: false,
            opened_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }

    let confirmation = dialog_confirmation(
        &runtime,
        "tab-1",
        &DialogFenceBaseline {
            previous_opened_at: None,
        },
    )
    .await
    .expect("matching target should retain dialog authority");
    assert_eq!(
        confirmation.status,
        InteractionConfirmationStatus::Confirmed
    );
    assert_eq!(
        confirmation.kind,
        Some(InteractionConfirmationKind::DialogOpened)
    );

    assert!(
        dialog_confirmation(
            &runtime,
            "tab-2",
            &DialogFenceBaseline {
                previous_opened_at: None,
            },
        )
        .await
        .is_none(),
        "foreign target must not consume dialog authority"
    );
}

#[tokio::test]
async fn dialog_confirmation_ignores_same_target_dialog_from_before_actuation() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Hello".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: false,
            opened_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }

    assert!(
        dialog_confirmation(
            &runtime,
            "tab-1",
            &DialogFenceBaseline {
                previous_opened_at: Some("2026-01-01T00:00:00Z".to_string()),
            },
        )
        .await
        .is_none(),
        "pre-existing same-target dialog must not prove the current actuation"
    );
}

#[tokio::test]
async fn dialog_confirmation_accepts_same_target_dialog_with_new_opened_at() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Hello again".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: false,
            opened_at: "2026-01-01T00:00:01Z".to_string(),
        });
    }

    let confirmation = dialog_confirmation(
        &runtime,
        "tab-1",
        &DialogFenceBaseline {
            previous_opened_at: Some("2026-01-01T00:00:00Z".to_string()),
        },
    )
    .await
    .expect("new same-target dialog must still prove the current actuation");

    assert_eq!(
        confirmation.status,
        InteractionConfirmationStatus::Confirmed
    );
}

#[tokio::test]
async fn result_actuation_fence_preserves_completed_result() {
    let runtime = new_shared_dialog_runtime();
    let outcome = await_actuation_result_or_dialog(
        async { Ok::<_, rub_core::error::RubError>("selected".to_string()) },
        runtime,
        "select_option",
        "tab-1",
    )
    .await
    .expect("completed actuation should return result");

    assert_eq!(outcome.fence, ActuationFence::Completed);
    assert_eq!(outcome.result.as_deref(), Some("selected"));
}

#[tokio::test]
async fn result_actuation_fence_returns_dialog_without_unproven_result() {
    let runtime = new_shared_dialog_runtime();
    let runtime_for_dialog = runtime.clone();
    tokio::spawn(async move {
        tokio::time::sleep(super::DIALOG_ACTUATION_TIMEOUT + Duration::from_millis(25)).await;
        let mut state = runtime_for_dialog.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Blocked".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: None,
            default_prompt: None,
            has_browser_handler: false,
            opened_at: "2026-01-01T00:00:02Z".to_string(),
        });
    });

    let outcome = await_actuation_result_or_dialog(
        async {
            tokio::time::sleep(super::DIALOG_ACTUATION_TIMEOUT * 4).await;
            Ok::<_, rub_core::error::RubError>("late".to_string())
        },
        runtime,
        "select_option",
        "tab-1",
    )
    .await
    .expect("new dialog should become the fallback authority after timeout");

    assert_eq!(outcome.fence, ActuationFence::DialogOpened);
    assert_eq!(outcome.result, None);
}
