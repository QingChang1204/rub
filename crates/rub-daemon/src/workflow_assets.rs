use std::path::{Path, PathBuf};

use rub_core::error::{ErrorCode, RubError};

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
    let normalized = normalize_workflow_name(name)?;
    let path = resolve_named_workflow_path(rub_home, &normalized)?;
    let path_string = path.display().to_string();
    let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RubError::domain(
            ErrorCode::FileNotFound,
            format!("Named workflow not found: {normalized} ({path_string})"),
        ),
        _ => RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to read workflow asset {path_string}: {error}"),
        ),
    })?;
    Ok((normalized, contents, path))
}

#[cfg(test)]
mod tests {
    use super::{load_named_workflow_spec, normalize_workflow_name, resolve_named_workflow_path};
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
}
