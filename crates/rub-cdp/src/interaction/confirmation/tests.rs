use super::support::{OBSERVATION_BACKOFF_CEILING, OBSERVATION_INTERVAL, observation_poll_delay};
use super::{
    ActuationFence, DialogFenceBaseline, await_actuation_result_or_dialog, dialog_confirmation,
};
use crate::dialogs::new_shared_dialog_runtime;
use rub_core::model::{
    DialogKind, DialogResolutionInfo, DialogRuntimeStatus, InteractionConfirmationKind,
    InteractionConfirmationStatus, PendingDialogInfo,
};
use std::time::Duration;

fn pending_dialog_info() -> PendingDialogInfo {
    PendingDialogInfo {
        kind: DialogKind::Alert,
        message: "Hello".to_string(),
        url: "https://example.test".to_string(),
        tab_target_id: Some("tab-1".to_string()),
        frame_id: Some("frame-a".to_string()),
        default_prompt: None,
        has_browser_handler: false,
        opened_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn dialog_baseline(
    previous_pending_opened_at: Option<&str>,
    previous_last_opened_at: Option<&str>,
) -> DialogFenceBaseline {
    DialogFenceBaseline {
        previous_pending_opened_at: previous_pending_opened_at.map(str::to_string),
        previous_last_opened_at: previous_last_opened_at.map(str::to_string),
    }
}

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
        state.pending_dialog = Some(pending_dialog_info());
    }

    let confirmation = dialog_confirmation(&runtime, "tab-1", &dialog_baseline(None, None))
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
        dialog_confirmation(&runtime, "tab-2", &dialog_baseline(None, None),)
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
        state.pending_dialog = Some(pending_dialog_info());
    }

    assert!(
        dialog_confirmation(
            &runtime,
            "tab-1",
            &dialog_baseline(Some("2026-01-01T00:00:00Z"), None),
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
        &dialog_baseline(Some("2026-01-01T00:00:00Z"), None),
    )
    .await
    .expect("new same-target dialog must still prove the current actuation");

    assert_eq!(
        confirmation.status,
        InteractionConfirmationStatus::Confirmed
    );
}

#[tokio::test]
async fn dialog_confirmation_accepts_same_target_dialog_resolved_by_intercept() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.status = DialogRuntimeStatus::Inactive;
        state.pending_dialog = None;
        state.last_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Handled by intercept".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "2026-01-01T00:00:02Z".to_string(),
        });
        state.last_result = Some(DialogResolutionInfo {
            accepted: false,
            user_input: None,
            closed_at: "2026-01-01T00:00:03Z".to_string(),
        });
    }

    let confirmation = dialog_confirmation(
        &runtime,
        "tab-1",
        &dialog_baseline(None, Some("2026-01-01T00:00:01Z")),
    )
    .await
    .expect("intercept-resolved same-target dialog must prove the current actuation");

    assert_eq!(
        confirmation.status,
        InteractionConfirmationStatus::Confirmed
    );
    assert_eq!(
        confirmation.kind,
        Some(InteractionConfirmationKind::DialogOpened)
    );
    assert_eq!(
        confirmation.details.unwrap()["accepted"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn dialog_confirmation_ignores_preexisting_resolved_dialog() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.last_dialog = Some(pending_dialog_info());
        state.last_result = Some(DialogResolutionInfo {
            accepted: true,
            user_input: None,
            closed_at: "2026-01-01T00:00:01Z".to_string(),
        });
    }

    assert!(
        dialog_confirmation(
            &runtime,
            "tab-1",
            &dialog_baseline(None, Some("2026-01-01T00:00:00Z")),
        )
        .await
        .is_none(),
        "already-resolved dialog from before actuation must not confirm the current action"
    );
}

#[tokio::test]
async fn dialog_confirmation_ignores_new_last_dialog_with_stale_resolution() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.last_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "New opening without matching close".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "2026-01-01T00:00:03Z".to_string(),
        });
        state.last_result = Some(DialogResolutionInfo {
            accepted: true,
            user_input: None,
            closed_at: "2026-01-01T00:00:01Z".to_string(),
        });
    }

    assert!(
        dialog_confirmation(
            &runtime,
            "tab-1",
            &dialog_baseline(None, Some("2026-01-01T00:00:02Z")),
        )
        .await
        .is_none(),
        "a stale resolution must not prove a newer last_dialog"
    );
}

#[tokio::test]
async fn dialog_confirmation_ignores_resolved_dialog_with_unparseable_times() {
    let runtime = new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.last_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Malformed time".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-a".to_string()),
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "not-rfc3339".to_string(),
        });
        state.last_result = Some(DialogResolutionInfo {
            accepted: true,
            user_input: None,
            closed_at: "2026-01-01T00:00:01Z".to_string(),
        });
    }

    assert!(
        dialog_confirmation(&runtime, "tab-1", &dialog_baseline(None, None),)
            .await
            .is_none(),
        "malformed dialog times must fail closed instead of confirming an action"
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

#[tokio::test]
async fn result_actuation_fence_returns_dialog_before_actuation_timeout() {
    let runtime = new_shared_dialog_runtime();
    let runtime_for_dialog = runtime.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut state = runtime_for_dialog.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Synchronous alert".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: None,
            default_prompt: None,
            has_browser_handler: false,
            opened_at: "2026-01-01T00:00:03Z".to_string(),
        });
    });

    let started = tokio::time::Instant::now();
    let outcome = await_actuation_result_or_dialog(
        async {
            tokio::time::sleep(super::DIALOG_ACTUATION_TIMEOUT * 4).await;
            Ok::<_, rub_core::error::RubError>("late".to_string())
        },
        runtime,
        "semantic_click",
        "tab-1",
    )
    .await
    .expect("fresh in-flight dialog should become the immediate fallback authority");

    assert_eq!(outcome.fence, ActuationFence::DialogOpened);
    assert_eq!(outcome.result, None);
    assert!(
        started.elapsed() < super::DIALOG_ACTUATION_TIMEOUT,
        "dialog fallback should not wait for the actuation timeout"
    );
}
