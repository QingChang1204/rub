use super::BatchCloseResult;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::PathReferenceState;
use std::path::Path;

fn batch_close_path_state(path_authority: &str, path_kind: &str) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_runtime_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: "cli_close_all_projection".to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(crate) fn daemon_ctl_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_runtime_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(crate) struct DaemonCtlPathContext<'a> {
    pub(crate) path_key: &'a str,
    pub(crate) path: &'a Path,
    pub(crate) path_authority: &'a str,
    pub(crate) upstream_truth: &'a str,
    pub(crate) path_kind: &'a str,
    pub(crate) reason: &'a str,
}

pub(crate) fn daemon_ctl_path_error(
    code: ErrorCode,
    message: String,
    context: DaemonCtlPathContext<'_>,
) -> RubError {
    RubError::domain_with_context(
        code,
        message,
        serde_json::json!({
            context.path_key: context.path.display().to_string(),
            format!("{}_state", context.path_key): daemon_ctl_path_state(
                context.path_authority,
                context.upstream_truth,
                context.path_kind,
            ),
            "reason": context.reason,
        }),
    )
}

pub(crate) fn daemon_ctl_socket_error(
    code: ErrorCode,
    message: String,
    socket_path: &Path,
    path_authority: &str,
    upstream_truth: &str,
    reason: &str,
) -> RubError {
    daemon_ctl_path_error(
        code,
        message,
        DaemonCtlPathContext {
            path_key: "socket_path",
            path: socket_path,
            path_authority,
            upstream_truth,
            path_kind: "session_socket",
            reason,
        },
    )
}

pub(crate) fn project_batch_close_result(
    rub_home: &Path,
    result: &BatchCloseResult,
) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "session_batch_close",
            "rub_home": rub_home.display().to_string(),
            "rub_home_state": batch_close_path_state(
                "cli.close_all.subject.rub_home",
                "close_all_home_directory",
            ),
        },
        "result": {
            "closed": result.closed,
            "cleaned_stale": result.cleaned_stale,
            "failed": result.failed,
        }
    })
}
