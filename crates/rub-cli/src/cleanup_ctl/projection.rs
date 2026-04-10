use super::CleanupResult;
use std::path::Path;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::PathReferenceState;

fn cleanup_path_state(path_authority: &str, path_kind: &str) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_runtime_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: "cli_cleanup_projection".to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn cleanup_runtime_path_state(
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

pub(super) struct CleanupPathContext<'a> {
    pub(super) path_key: &'a str,
    pub(super) path: &'a Path,
    pub(super) path_authority: &'a str,
    pub(super) upstream_truth: &'a str,
    pub(super) path_kind: &'a str,
    pub(super) reason: &'a str,
}

pub(super) fn cleanup_path_error(
    code: ErrorCode,
    message: String,
    context: CleanupPathContext<'_>,
) -> RubError {
    RubError::domain_with_context(
        code,
        message,
        serde_json::json!({
            context.path_key: context.path.display().to_string(),
            format!("{}_state", context.path_key): cleanup_runtime_path_state(
                context.path_authority,
                context.upstream_truth,
                context.path_kind,
            ),
            "reason": context.reason,
        }),
    )
}

fn cleanup_path_refs(
    paths: &[String],
    path_authority: &str,
    path_kind: &str,
) -> Vec<serde_json::Value> {
    paths
        .iter()
        .map(|path| {
            serde_json::json!({
                "path": path,
                "path_state": cleanup_path_state(path_authority, path_kind),
            })
        })
        .collect()
}

pub fn project_cleanup_result(rub_home: &Path, result: &CleanupResult) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "runtime_cleanup",
            "rub_home": rub_home.display().to_string(),
            "rub_home_state": cleanup_path_state(
                "cli.cleanup.subject.rub_home",
                "cleanup_home_directory",
            ),
        },
        "result": {
            "cleaned_stale_sessions": result.cleaned_stale_sessions,
            "kept_active_sessions": result.kept_active_sessions,
            "skipped_unreachable_sessions": result.skipped_unreachable_sessions,
            "cleaned_temp_daemons": result.cleaned_temp_daemons,
            "skipped_busy_temp_daemons": result.skipped_busy_temp_daemons,
            "removed_temp_homes": result.removed_temp_homes,
            "removed_temp_home_refs": cleanup_path_refs(
                &result.removed_temp_homes,
                "cli.cleanup.result.removed_temp_homes",
                "temp_owned_rub_home",
            ),
            "killed_orphan_browser_pids": result.killed_orphan_browser_pids,
            "removed_orphan_browser_profiles": result.removed_orphan_browser_profiles,
            "removed_orphan_browser_profile_refs": cleanup_path_refs(
                &result.removed_orphan_browser_profiles,
                "cli.cleanup.result.removed_orphan_browser_profiles",
                "orphan_browser_profile_root",
            ),
        }
    })
}
