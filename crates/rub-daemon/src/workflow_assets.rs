use std::path::{Path, PathBuf};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::PathReferenceState;
use serde_json::{Value, json};

use crate::rub_paths::RubPaths;

pub fn resolve_named_workflow_path(rub_home: &Path, name: &str) -> Result<PathBuf, RubError> {
    let normalized = normalize_workflow_name(name)?;
    Ok(RubPaths::new(rub_home)
        .workflows_dir()
        .join(format!("{normalized}.json")))
}

pub fn normalize_workflow_name(name: &str) -> Result<String, RubError> {
    let trimmed = name.trim().trim_end_matches(".json");
    if trimmed.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Workflow name cannot be empty",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid workflow name '{name}'"),
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid workflow name '{name}'; use letters, digits, underscores, and dashes"),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn load_named_workflow_spec(
    rub_home: &Path,
    name: &str,
) -> Result<(String, String, PathBuf), RubError> {
    load_named_workflow_spec_with_authority(
        rub_home,
        name,
        "rub_daemon.workflow_assets.path",
        "named_workflow_name",
    )
}

pub fn load_named_workflow_spec_with_authority(
    rub_home: &Path,
    name: &str,
    path_authority: &str,
    upstream_truth: &str,
) -> Result<(String, String, PathBuf), RubError> {
    let normalized = normalize_workflow_name(name)?;
    let path = resolve_named_workflow_path(rub_home, &normalized)?;
    let path_string = path.display().to_string();
    let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RubError::domain_with_context(
            ErrorCode::FileNotFound,
            format!("Named workflow not found: {normalized} ({path_string})"),
            json!({
                "path": path_string,
                "path_state": workflow_asset_path_state(path_authority, upstream_truth),
                "reason": "named_workflow_asset_not_found",
            }),
        ),
        _ => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Failed to read workflow asset {path_string}: {error}"),
            json!({
                "path": path_string,
                "path_state": workflow_asset_path_state(path_authority, upstream_truth),
                "reason": "named_workflow_asset_read_failed",
            }),
        ),
    })?;
    Ok((normalized, contents, path))
}

pub fn workflow_asset_path_state(path_authority: &str, upstream_truth: &str) -> PathReferenceState {
    PathReferenceState {
        truth_level: "input_path_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: "workflow_asset_reference".to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(crate) fn annotate_workflow_asset_path_state(
    payload: &mut Value,
    state_field: &str,
    path_authority: &str,
    upstream_truth: &str,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };

    object.insert(
        state_field.to_string(),
        json!(workflow_asset_path_state(path_authority, upstream_truth)),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        annotate_workflow_asset_path_state, load_named_workflow_spec,
        load_named_workflow_spec_with_authority, normalize_workflow_name,
        resolve_named_workflow_path, workflow_asset_path_state,
    };
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn normalize_workflow_name_rejects_invalid_shapes() {
        assert_eq!(normalize_workflow_name("login_flow").unwrap(), "login_flow");
        assert!(normalize_workflow_name("../bad").is_err());
        assert!(normalize_workflow_name("bad/name").is_err());
        assert!(normalize_workflow_name("bad name").is_err());
    }

    #[test]
    fn resolve_named_workflow_path_projects_canonical_asset_location() {
        let path = resolve_named_workflow_path(Path::new("/tmp/rub-home"), "login_flow").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/rub-home/workflows/login_flow.json")
        );
    }

    #[test]
    fn load_named_workflow_spec_reads_saved_asset() {
        let home =
            std::env::temp_dir().join(format!("rub-daemon-workflow-assets-{}", std::process::id()));
        let path = resolve_named_workflow_path(&home, "reply_flow").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"steps":[{"command":"doctor","args":{}}]}"#).unwrap();

        let (name, contents, loaded_path) = load_named_workflow_spec(&home, "reply_flow").unwrap();
        assert_eq!(name, "reply_flow");
        assert_eq!(contents, r#"{"steps":[{"command":"doctor","args":{}}]}"#);
        assert_eq!(loaded_path, path);

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn load_named_workflow_spec_missing_file_preserves_path_state() {
        let home = std::env::temp_dir().join(format!(
            "rub-daemon-workflow-assets-missing-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = fs::remove_dir_all(&home);

        let envelope = load_named_workflow_spec_with_authority(
            &home,
            "reply_flow",
            "trigger.workflow.spec_source.path",
            "trigger_workflow_payload.workflow_name",
        )
        .expect_err("missing named workflow should fail")
        .into_envelope();
        let context = envelope.context.expect("workflow asset error context");
        assert_eq!(context["reason"], "named_workflow_asset_not_found");
        assert_eq!(
            context["path_state"]["path_authority"],
            "trigger.workflow.spec_source.path"
        );
        assert_eq!(
            context["path_state"]["upstream_truth"],
            "trigger_workflow_payload.workflow_name"
        );
    }

    #[test]
    fn workflow_asset_path_state_marks_named_workflow_reference_boundary() {
        let state = workflow_asset_path_state(
            "automation.action.workflow_path",
            "trigger_action_payload.workflow_name",
        );
        assert_eq!(state.truth_level, "input_path_reference");
        assert_eq!(state.path_authority, "automation.action.workflow_path");
        assert_eq!(state.upstream_truth, "trigger_action_payload.workflow_name");
        assert_eq!(state.path_kind, "workflow_asset_reference");
        assert_eq!(state.control_role, "display_only");
    }

    #[test]
    fn annotate_workflow_asset_path_state_projects_structured_reference() {
        let mut payload = json!({
            "path": "/tmp/rub-home/workflows/reply_flow.json",
        });
        annotate_workflow_asset_path_state(
            &mut payload,
            "path_state",
            "orchestration.workflow.spec_source.path",
            "orchestration_workflow_payload.workflow_name",
        );
        assert_eq!(payload["path_state"]["truth_level"], "input_path_reference");
        assert_eq!(
            payload["path_state"]["path_authority"],
            "orchestration.workflow.spec_source.path"
        );
        assert_eq!(
            payload["path_state"]["upstream_truth"],
            "orchestration_workflow_payload.workflow_name"
        );
        assert_eq!(
            payload["path_state"]["path_kind"],
            "workflow_asset_reference"
        );
    }
}
