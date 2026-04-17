use super::{
    PendingResponseCommit, finalize_response, preflight_rejection_response, prepare_replay_fence,
    prepare_request_preflight, replay_request_fingerprint,
};
use crate::router::TransactionDeadline;
use crate::session::ReplayCommandClaim;
use crate::session::SessionState;
use crate::workflow_capture::WorkflowCaptureDeliveryState;
use rub_core::error::ErrorCode;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use std::path::PathBuf;
use std::sync::Arc;

fn unique_home(label: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "rub-post-commit-journal-{label}-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&home);
    home
}

#[tokio::test]
async fn finalize_response_appends_redacted_post_commit_journal() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let home = unique_home("success");
        std::fs::create_dir_all(&home).expect("create rub_home");
        let secrets = home.join("secrets.env");
        std::fs::write(&secrets, "RUB_TOKEN=token-123\n").expect("write secrets");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600))
            .expect("set permissions");

        let state = Arc::new(SessionState::new("default", home.clone(), None));
        let request = IpcRequest::new(
            "type",
            serde_json::json!({ "selector": "#password", "text": "token-123", "clear": true }),
            1_000,
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "echo": "token-123",
                "ok": true
            }),
        );

        let committed = finalize_response(&request, response, false, None, &state).await;
        let history = state.command_history(5).await;
        let journal = state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal");

        assert_eq!(committed.command_id.as_deref(), Some("cmd-1"));
        assert_eq!(history.entries.len(), 1);
        assert_eq!(journal.len(), 1);
        assert_eq!(
            journal[0]["journal_state"]["commit_relation"],
            serde_json::json!("downstream_of_daemon_commit_fence")
        );
        assert_eq!(
            journal[0]["journal_state"]["durability"],
            serde_json::json!("durable")
        );
        assert_eq!(journal[0]["command"], serde_json::json!("type"));
        assert_eq!(journal[0]["command_id"], serde_json::json!("cmd-1"));
        assert_eq!(
            journal[0]["request"]["args"]["text"],
            serde_json::json!("$RUB_TOKEN")
        );
        assert_eq!(
            journal[0]["response"]["data"]["echo"],
            serde_json::json!("$RUB_TOKEN")
        );
        assert_eq!(
            journal[0]["response"]["request_id"],
            serde_json::json!("req-1")
        );

        let _ = std::fs::remove_dir_all(home);
    }
}

#[tokio::test]
async fn finalize_response_journal_failure_does_not_rewrite_commit_truth() {
    let home = unique_home("failure");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        1_000,
    )
    .with_command_id("cmd-2")
    .expect("static command_id must be valid");
    let response = IpcResponse::success("req-2", serde_json::json!({ "ok": true }));

    state.force_post_commit_journal_failure_once();
    let committed = finalize_response(&request, response, false, None, &state).await;
    let history = state.command_history(5).await;

    assert_eq!(committed.command_id.as_deref(), Some("cmd-2"));
    assert!(committed.data.is_some());
    assert_eq!(state.post_commit_journal_failure_count(), 1);
    assert_eq!(
        state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal")
            .len(),
        0
    );
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].command, "open");

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn delivery_failure_after_execution_caches_authoritative_failure_response() {
    let home = unique_home("delivery-after-exec");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        1_000,
    )
    .with_command_id("cmd-delivery")
    .expect("static command_id must be valid");
    let mut replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");
    replay_owner.mark_execution_started();

    let response = IpcResponse::success("req-1", serde_json::json!({ "ok": true }));
    let pending =
        PendingResponseCommit::new(request.clone(), response, false, true, Some(replay_owner));
    let delivery_error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "socket closed");
    pending
        .commit_after_delivery_failure(&state, delivery_error.to_string())
        .await;

    let replay = prepare_replay_fence(
        &request,
        &state,
        "req-2",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect_err("retry should receive cached authoritative delivery failure");
    assert_eq!(replay.command_id.as_deref(), Some("cmd-delivery"));
    assert_eq!(
        replay.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(
        replay
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_delivery_failed_after_execution_commit")
    );

    let history = state.command_history(5).await;
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].command, "open");
    let capture = state.workflow_capture(5).await;
    assert_eq!(capture.entries.len(), 1);
    assert_eq!(capture.entries[0].command, "open");
    assert_eq!(
        capture.entries[0].delivery_state,
        WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit
    );
    assert_eq!(
        state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn delivery_failure_before_execution_releases_replay_without_committing_history() {
    let home = unique_home("delivery-before-exec");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
        .with_command_id("cmd-pre-exec")
        .expect("static command_id must be valid");
    let replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");

    let response = IpcResponse::error(
        "req-1",
        rub_core::error::ErrorEnvelope::new(
            ErrorCode::IpcTimeout,
            "Command timed out waiting in queue after 250ms",
        ),
    );
    let pending =
        PendingResponseCommit::new(request.clone(), response, false, false, Some(replay_owner));
    let delivery_error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "socket closed");
    pending
        .commit_after_delivery_failure(&state, delivery_error.to_string())
        .await;

    let replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-2",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("delivery failure before execution should release replay owner")
    .expect("replay owner should be reclaimable");
    assert_eq!(replay_owner.command_id, "cmd-pre-exec");

    match state.claim_replay_command("cmd-pre-exec", replay_request_fingerprint(&request)) {
        ReplayCommandClaim::Wait(_) => {}
        ReplayCommandClaim::SpentWithoutCachedResponse => {
            panic!("pre-execution delivery failure must not spend the command_id")
        }
        other => panic!("reclaimed replay fence should now be owned in-flight, got {other:?}"),
    }
    let history = state.command_history(5).await;
    assert!(
        history.entries.is_empty(),
        "undelivered pre-execution response must not appear in post-commit history"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn external_preflight_rejects_in_process_only_internal_commands() {
    let state = Arc::new(SessionState::new(
        "default",
        unique_home("in-process-only"),
        None,
    ));
    let request = IpcRequest::new("_trigger_pipe", serde_json::json!({ "spec": "[]" }), 1_000);
    let preflight = prepare_request_preflight(&request);

    let external = preflight_rejection_response(&request, &preflight, &state, false)
        .expect("external dispatch should reject in-process-only command");
    assert_eq!(
        external
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("in_process_only_internal_command")
    );

    assert!(
        preflight_rejection_response(&request, &preflight, &state, true).is_none(),
        "in-process dispatch should retain authority to call reserved trigger wrappers"
    );
}

#[tokio::test]
async fn pre_execution_local_final_response_does_not_enter_post_commit_surfaces() {
    let home = unique_home("pre-exec-local-final");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let mut request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        1_000,
    );
    request.ipc_protocol_version = "0.0".to_string();
    let preflight = prepare_request_preflight(&request);
    let response = preflight_rejection_response(&request, &preflight, &state, false)
        .expect("protocol mismatch should fail closed before execution");

    let committed = PendingResponseCommit::new(request.clone(), response, false, false, None)
        .commit_locally(&state)
        .await;

    assert_eq!(committed.request_id, preflight.request_id);
    assert_eq!(
        committed.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcVersionMismatch)
    );
    assert!(state.command_history(5).await.entries.is_empty());
    assert!(state.workflow_capture(5).await.entries.is_empty());
    assert_eq!(
        state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal")
            .len(),
        0
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn pre_execution_external_final_response_does_not_enter_post_commit_surfaces() {
    let home = unique_home("pre-exec-external-final");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let mut request = IpcRequest::new(
        "open",
        serde_json::json!({ "url": "https://example.com" }),
        1_000,
    );
    request.ipc_protocol_version = "0.0".to_string();
    let preflight = prepare_request_preflight(&request);
    let response = preflight_rejection_response(&request, &preflight, &state, false)
        .expect("protocol mismatch should fail closed before execution");

    PendingResponseCommit::new(request, response, false, false, None)
        .commit_after_delivery(&state)
        .await;

    assert!(state.command_history(5).await.entries.is_empty());
    assert!(state.workflow_capture(5).await.entries.is_empty());
    assert_eq!(
        state
            .read_post_commit_journal_entries_for_tests()
            .expect("read journal")
            .len(),
        0
    );

    let _ = std::fs::remove_dir_all(home);
}
