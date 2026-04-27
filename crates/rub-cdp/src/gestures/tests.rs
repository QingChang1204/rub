use crate::interaction::{ActuationFence, await_actuation_or_dialog};
use crate::interaction::{DIALOG_ACTUATION_GRACE_PERIOD, DIALOG_ACTUATION_TIMEOUT};
use rub_core::model::{DialogKind, PendingDialogInfo};
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
        "tab-active",
    )
    .await;

    assert!(
        matches!(
            result
                .expect("timed out actuation should stay truthful")
                .fence,
            ActuationFence::Indeterminate
        ),
        "post-timeout local actuation must fail closed unless a fresh dialog becomes authoritative"
    );
    sleep(DIALOG_ACTUATION_GRACE_PERIOD + Duration::from_millis(300)).await;
    assert!(
        !committed.load(Ordering::SeqCst),
        "timed-out local actuation must not keep issuing background browser commands"
    );
}

#[tokio::test]
async fn timed_out_actuation_ignores_foreign_pending_dialog() {
    let runtime = crate::dialogs::new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Background dialog".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-foreign".to_string()),
            frame_id: None,
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }

    let fence = await_actuation_or_dialog(
        async move {
            sleep(
                DIALOG_ACTUATION_TIMEOUT
                    + DIALOG_ACTUATION_GRACE_PERIOD
                    + Duration::from_millis(50),
            )
            .await;
            Ok(())
        },
        runtime,
        "click",
        "tab-active",
    )
    .await
    .expect("foreign dialog must not crash actuation fence");

    assert!(
        !matches!(fence.fence, ActuationFence::DialogOpened),
        "foreign pending dialog must not confirm an interaction on another page"
    );
}

#[tokio::test]
async fn timed_out_actuation_ignores_same_target_dialog_from_before_actuation() {
    let runtime = crate::dialogs::new_shared_dialog_runtime();
    {
        let mut state = runtime.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Existing dialog".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-active".to_string()),
            frame_id: None,
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "2026-01-01T00:00:00Z".to_string(),
        });
    }

    let fence = await_actuation_or_dialog(
        async move {
            sleep(
                DIALOG_ACTUATION_TIMEOUT
                    + DIALOG_ACTUATION_GRACE_PERIOD
                    + Duration::from_millis(50),
            )
            .await;
            Ok(())
        },
        runtime,
        "click",
        "tab-active",
    )
    .await
    .expect("stale same-target dialog must not crash actuation fence");

    assert!(
        !matches!(fence.fence, ActuationFence::DialogOpened),
        "pre-existing same-target dialog must not confirm the current actuation"
    );
}

#[tokio::test]
async fn timed_out_actuation_accepts_fresh_dialog_opened_after_timeout() {
    let runtime = crate::dialogs::new_shared_dialog_runtime();
    let runtime_for_dialog = runtime.clone();
    tokio::spawn(async move {
        sleep(DIALOG_ACTUATION_TIMEOUT + Duration::from_millis(50)).await;
        let mut state = runtime_for_dialog.write().await;
        state.pending_dialog = Some(PendingDialogInfo {
            kind: DialogKind::Alert,
            message: "Fresh dialog".to_string(),
            url: "https://example.test".to_string(),
            tab_target_id: Some("tab-active".to_string()),
            frame_id: None,
            default_prompt: None,
            has_browser_handler: true,
            opened_at: "2026-01-01T00:00:01Z".to_string(),
        });
    });

    let fence = await_actuation_or_dialog(
        async move {
            sleep(
                DIALOG_ACTUATION_TIMEOUT
                    + DIALOG_ACTUATION_GRACE_PERIOD
                    + Duration::from_millis(50),
            )
            .await;
            Ok(())
        },
        runtime,
        "click",
        "tab-active",
    )
    .await
    .expect("fresh post-timeout dialog should remain a truthful fence");

    assert!(
        matches!(fence.fence, ActuationFence::DialogOpened),
        "fresh same-target dialog opened after timeout must retain dialog fence authority"
    );
}
