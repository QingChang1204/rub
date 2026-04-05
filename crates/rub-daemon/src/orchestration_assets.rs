use std::path::{Path, PathBuf};

use rub_core::error::{ErrorCode, RubError};

use crate::rub_paths::RubPaths;

pub fn resolve_named_orchestration_path(rub_home: &Path, name: &str) -> Result<PathBuf, RubError> {
    let normalized = normalize_orchestration_name(name)?;
    Ok(RubPaths::new(rub_home)
        .orchestrations_dir()
        .join(format!("{normalized}.json")))
}

pub fn normalize_orchestration_name(name: &str) -> Result<String, RubError> {
    let trimmed = name.trim().trim_end_matches(".json");
    if trimmed.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Orchestration asset name cannot be empty",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid orchestration asset name '{name}'"),
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Invalid orchestration asset name '{name}'; use letters, digits, underscores, and dashes"
            ),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn load_named_orchestration_spec(
    rub_home: &Path,
    name: &str,
) -> Result<(String, String, PathBuf), RubError> {
    let normalized = normalize_orchestration_name(name)?;
    let path = resolve_named_orchestration_path(rub_home, &normalized)?;
    let path_string = path.display().to_string();
    let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RubError::domain(
            ErrorCode::FileNotFound,
            format!("Named orchestration asset not found: {normalized} ({path_string})"),
        ),
        _ => RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to read orchestration asset {path_string}: {error}"),
        ),
    })?;
    Ok((normalized, contents, path))
}

#[cfg(test)]
mod tests {
    use super::{
        load_named_orchestration_spec, normalize_orchestration_name,
        resolve_named_orchestration_path,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn normalize_orchestration_name_rejects_invalid_shapes() {
        assert_eq!(
            normalize_orchestration_name("follow_up_rule").unwrap(),
            "follow_up_rule"
        );
        assert!(normalize_orchestration_name("../bad").is_err());
        assert!(normalize_orchestration_name("bad/name").is_err());
        assert!(normalize_orchestration_name("bad name").is_err());
    }

    #[test]
    fn resolve_named_orchestration_path_projects_canonical_asset_location() {
        let path =
            resolve_named_orchestration_path(Path::new("/tmp/rub-home"), "follow_up_rule").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/rub-home/orchestrations/follow_up_rule.json")
        );
    }

    #[test]
    fn load_named_orchestration_spec_reads_saved_asset() {
        let home = std::env::temp_dir().join(format!(
            "rub-daemon-orchestration-assets-{}",
            std::process::id()
        ));
        let path = resolve_named_orchestration_path(&home, "reply_rule").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"source":{"session_id":"a"},"target":{"session_id":"b"},"condition":{"kind":"url_match","url":"x"},"actions":[{"kind":"browser_command","command":"reload"}]}"#).unwrap();

        let (name, contents, loaded_path) =
            load_named_orchestration_spec(&home, "reply_rule").unwrap();
        assert_eq!(name, "reply_rule");
        assert!(contents.contains("\"browser_command\""));
        assert_eq!(loaded_path, path);

        let _ = fs::remove_dir_all(home);
    }
}
