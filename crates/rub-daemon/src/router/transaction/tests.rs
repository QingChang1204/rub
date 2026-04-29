use super::{
    PendingResponseCommit, finalize_response, preflight_rejection_response,
    prepare_command_dispatch, prepare_replay_fence, prepare_request_preflight,
    prepare_request_preflight_with_inherited_deadline, queue_timeout_response,
    replay_request_fingerprint, replay_timeout_response,
};
use crate::router::TransactionDeadline;
use crate::session::ReplayCommandClaim;
use crate::session::SessionState;
use crate::workflow_capture::WorkflowCaptureDeliveryState;
use rub_core::error::ErrorCode;
use rub_ipc::codec::{MAX_FRAME_BYTES, encoded_frame_len};
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
async fn delivery_failure_after_execution_caches_committed_response_truth_across_durable_surfaces()
{
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
    .expect_err("retry should receive cached committed response");
    assert_eq!(replay.command_id.as_deref(), Some("cmd-delivery"));
    assert_eq!(replay.status, rub_ipc::protocol::ResponseStatus::Success);
    assert_eq!(replay.data, Some(serde_json::json!({ "ok": true })));
    assert!(replay.error.is_none());
    assert_eq!(
        state.pending_post_commit_projection_count(),
        0,
        "delivery-failure fallback should not leave history/workflow projection queued after the committed-truth fence returns"
    );
    assert_eq!(
        state.pending_post_commit_followup_count(),
        0,
        "delivery-failure fallback should not detach downstream post-commit followup authority"
    );

    let history = state.command_history(5).await;
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].command, "open");
    assert!(history.entries[0].success);
    assert_eq!(history.entries[0].summary.as_deref(), Some("success"));
    assert!(history.entries[0].error_code.is_none());
    let capture = state.workflow_capture(5).await;
    assert_eq!(capture.entries.len(), 1);
    assert_eq!(capture.entries[0].command, "open");
    assert_eq!(
        capture.entries[0].delivery_state,
        WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit
    );
    let journal = state
        .read_post_commit_journal_entries_for_tests()
        .expect("read journal");
    assert_eq!(journal.len(), 1);
    assert_eq!(
        journal[0]["response"]["status"],
        serde_json::json!("success")
    );
    assert_eq!(
        journal[0]["response"]["data"]["ok"],
        serde_json::json!(true)
    );
    assert!(journal[0]["response"]["error"].is_null());
    assert_eq!(
        journal[0]["delivery_state"],
        serde_json::json!("delivery_failed_after_commit")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn transport_frame_overflow_does_not_replace_committed_response_truth() {
    let home = unique_home("transport-overflow-truth");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new("state", serde_json::json!({}), 1_000)
        .with_command_id("cmd-transport-overflow")
        .expect("static command_id must be valid");
    let mut replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-overflow",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");
    replay_owner.mark_execution_started();
    let response = IpcResponse::success(
        "req-overflow",
        serde_json::json!({
            "payload": "x".repeat(MAX_FRAME_BYTES),
        }),
    );
    let pending =
        PendingResponseCommit::new(request.clone(), response, false, true, Some(replay_owner));

    let transport_response = pending.response_for_transport(&state.session_id);
    assert_eq!(
        transport_response.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(
        transport_response
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("response_exceeds_ipc_frame_limit")
    );
    let context = transport_response
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("overflow error must carry recovery context");
    assert_eq!(context["daemon_request_committed"], true);
    assert_eq!(context["safe_to_rerun_with_new_command_id"], false);
    assert_eq!(
        context["recovery_authority"],
        "replay_same_command_id_or_reduce_response_projection"
    );

    pending
        .commit_after_delivery_failure(&state, "transport projection overflow".to_string())
        .await;

    let replay = prepare_replay_fence(
        &request,
        &state,
        "req-replay",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect_err("retry should receive cached committed response truth");
    assert_eq!(replay.command_id.as_deref(), Some("cmd-transport-overflow"));
    assert_eq!(replay.status, rub_ipc::protocol::ResponseStatus::Success);
    assert!(replay.error.is_none());
    assert_eq!(
        replay
            .data
            .as_ref()
            .and_then(|data| data.get("payload"))
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(MAX_FRAME_BYTES)
    );

    let cached_pending =
        match prepare_command_dispatch(&request, &state, prepare_request_preflight(&request)).await
        {
            Ok(_) => panic!("cached replay should produce a final response before execution"),
            Err(pending) => pending,
        };
    let cached_transport_response = cached_pending.response_for_transport(&state.session_id);
    let cached_context = cached_transport_response
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("cached overflow error must carry committed recovery context");
    assert_eq!(cached_context["daemon_request_committed"], true);
    assert_eq!(cached_context["safe_to_rerun_with_new_command_id"], false);

    let history = state.command_history(5).await;
    assert_eq!(history.entries.len(), 1);
    assert!(history.entries[0].success);
    let journal = state
        .read_post_commit_journal_entries_for_tests()
        .expect("read journal");
    assert_eq!(journal.len(), 1);
    assert_eq!(
        journal[0]["response"]["status"],
        serde_json::json!("success")
    );
    assert!(journal[0]["response"]["error"].is_null());

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn first_committed_internal_transport_overflow_is_not_rerunnable() {
    let home = unique_home("internal-first-overflow-truth");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({}),
        1_000,
    )
    .with_command_id("cmd-internal-first-overflow")
    .expect("static command_id must be valid");
    let response = IpcResponse::success(
        "req-internal-first-overflow",
        serde_json::json!({
            "payload": "x".repeat(MAX_FRAME_BYTES),
        }),
    );

    let pending = PendingResponseCommit::new(request, response, true, true, None);
    let transport_response = pending.response_for_transport(&state.session_id);
    let context = transport_response
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("first internal overflow error must carry committed recovery context");

    assert_eq!(context["daemon_request_committed"], true);
    assert_eq!(context["safe_to_rerun_with_new_command_id"], false);

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn cached_committed_internal_transport_overflow_is_not_rerunnable() {
    let home = unique_home("internal-cached-overflow-truth");
    std::fs::create_dir_all(&home).expect("create rub_home");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({}),
        1_000,
    )
    .with_command_id("cmd-internal-transport-overflow")
    .expect("static command_id must be valid");
    let mut replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-internal-overflow",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");
    replay_owner.mark_execution_started();
    let response = IpcResponse::success(
        "req-internal-overflow",
        serde_json::json!({
            "payload": "x".repeat(MAX_FRAME_BYTES),
        }),
    );

    PendingResponseCommit::new(request.clone(), response, true, true, Some(replay_owner))
        .commit_after_delivery_failure(&state, "transport projection overflow".to_string())
        .await;

    let cached_pending =
        match prepare_command_dispatch(&request, &state, prepare_request_preflight(&request)).await
        {
            Ok(_) => panic!("cached internal replay should produce a final response"),
            Err(pending) => pending,
        };
    let cached_transport_response = cached_pending.response_for_transport(&state.session_id);
    let cached_context = cached_transport_response
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("cached internal overflow error must carry committed recovery context");
    assert_eq!(cached_context["daemon_request_committed"], true);
    assert_eq!(cached_context["safe_to_rerun_with_new_command_id"], false);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn response_for_transport_applies_frame_limit_after_daemon_session_id_injection() {
    let daemon_session_id =
        "daemon-session-id-that-is-long-enough-to-cross-the-final-wire-frame-limit";
    let request = IpcRequest::new("state", serde_json::json!({}), 1_000)
        .with_command_id("cmd-wire-limit")
        .expect("static command_id must be valid")
        .with_daemon_session_id(daemon_session_id)
        .expect("static daemon_session_id must be valid");
    let mut low = 0;
    let mut high = MAX_FRAME_BYTES;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let fits = {
            let response = IpcResponse::success(
                "req-wire-limit",
                serde_json::json!({
                    "payload": "x".repeat(mid),
                }),
            )
            .with_command_id("cmd-wire-limit")
            .expect("static command_id must be valid");
            encoded_frame_len(&response).is_ok_and(|encoded| encoded <= MAX_FRAME_BYTES)
        };
        if fits {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let payload_len = low;
    let response = IpcResponse::success(
        "req-wire-limit",
        serde_json::json!({
            "payload": "x".repeat(payload_len),
        }),
    );
    let pending = PendingResponseCommit::new(request.clone(), response, false, true, None);

    let transport_response = pending.response_for_transport(daemon_session_id);

    assert_eq!(
        transport_response.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(
        transport_response
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("response_exceeds_ipc_frame_limit")
    );
    let context = transport_response
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("overflow error must carry recovery context");
    assert_eq!(context["daemon_request_committed"], true);
    assert_eq!(context["safe_to_rerun_with_new_command_id"], false);
    assert_eq!(
        transport_response.command_id.as_deref(),
        Some("cmd-wire-limit")
    );
    assert_eq!(
        transport_response.daemon_session_id.as_deref(),
        Some(daemon_session_id)
    );
    transport_response
        .validate_correlated_contract(&request)
        .expect("overflow projection must preserve bound daemon authority correlation");
    assert!(
        encoded_frame_len(&transport_response)
            .expect("overflow projection itself must remain encodable")
            <= MAX_FRAME_BYTES
    );
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
fn queue_timeout_response_clamps_reported_queue_ms_to_transaction_budget() {
    let deadline = TransactionDeadline::new(100);
    std::thread::sleep(std::time::Duration::from_millis(110));

    let response = queue_timeout_response("state", "req-clamped", deadline);
    let envelope = response.error.expect("queue timeout should be an error");
    let context = envelope
        .context
        .expect("queue timeout should include context");

    assert_eq!(context["transaction_timeout_ms"], serde_json::json!(100));
    assert_eq!(context["queue_ms"], serde_json::json!(100));
    assert_eq!(response.timing.queue_ms, 100);
    assert_eq!(response.timing.exec_ms, 0);
    assert_eq!(response.timing.total_ms, 100);
    assert_eq!(
        envelope.message,
        "Command timed out waiting in queue after 100ms"
    );
}

#[test]
fn replay_timeout_response_reports_inherited_transaction_budget() {
    let request = IpcRequest::new("state", serde_json::json!({}), 5_000)
        .with_command_id("cmd-replay-timeout")
        .expect("static command_id must validate");
    let deadline = TransactionDeadline::new(100);
    std::thread::sleep(std::time::Duration::from_millis(110));

    let response = replay_timeout_response(
        &request,
        "req-replay-timeout",
        "cmd-replay-timeout",
        deadline,
    );
    let envelope = response.error.expect("replay timeout should be an error");
    let context = envelope
        .context
        .expect("replay timeout should include context");

    assert_eq!(context["transaction_timeout_ms"], serde_json::json!(100));
    assert_eq!(context["reason"], "replay_fence_wait_timeout");
    assert_ne!(context["transaction_timeout_ms"], serde_json::json!(5_000));
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

#[test]
fn prepare_request_preflight_clamps_to_inherited_deadline_budget() {
    let inherited_deadline = TransactionDeadline::new(50);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let request = IpcRequest::new("inspect", serde_json::json!({ "sub": "page" }), 1_000);

    let preflight =
        prepare_request_preflight_with_inherited_deadline(&request, Some(inherited_deadline));

    assert!(preflight.deadline.timeout_ms <= inherited_deadline.remaining_ms().saturating_add(1));
    assert!(
        preflight.deadline.timeout_ms < request.timeout_ms,
        "inherited outer deadline should clamp inner preflight budget"
    );
}

#[test]
fn external_preflight_allows_protocol_mismatch_only_for_control_plane_internal_commands() {
    let home = unique_home("transport-internal-protocol");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let mut request = IpcRequest::new("_handshake", serde_json::json!({}), 1_000);
    request.ipc_protocol_version = "0.9".to_string();
    let preflight = prepare_request_preflight(&request);

    assert!(
        preflight_rejection_response(&request, &preflight, &state, false).is_none(),
        "transport-exposed internal commands should reach router compatibility handling"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn external_preflight_rejects_protocol_mismatch_for_semantic_internal_commands() {
    let home = unique_home("semantic-internal-protocol");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let mut request = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({}),
        1_000,
    );
    request.ipc_protocol_version = "0.9".to_string();
    let preflight = prepare_request_preflight(&request);

    let response = preflight_rejection_response(&request, &preflight, &state, false)
        .expect("semantic internal protocol mismatch must fail closed");
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcVersionMismatch)
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn external_preflight_rejects_daemon_authority_mismatch_for_internal_commands() {
    let home = unique_home("internal-daemon-authority");
    let state = Arc::new(SessionState::new("default", home.clone(), None));
    let request = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({}),
        1_000,
    )
    .with_daemon_session_id("sess-other")
    .expect("static daemon_session_id must be valid");
    let preflight = prepare_request_preflight(&request);

    let response = preflight_rejection_response(&request, &preflight, &state, false)
        .expect("internal command daemon authority mismatch must fail closed");
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("daemon_authority_mismatch")
    );

    let _ = std::fs::remove_dir_all(home);
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
