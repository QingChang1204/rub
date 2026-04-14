use crate::error::{ErrorCode, RubError};
use crate::fs::atomic_write_bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::{fs, io};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretEffectiveSource {
    Environment,
    RubHomeSecretsEnv,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReferenceProvenance {
    pub reference: String,
    pub effective_source: SecretEffectiveSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReferenceProvenanceProjection {
    pub count: usize,
    pub items: Vec<SecretReferenceProvenance>,
}

pub fn parse_secret_placeholder(value: &str) -> Option<&str> {
    let name = value.strip_prefix('$')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        Some(name)
    } else {
        None
    }
}

pub fn is_valid_secret_name(name: &str) -> bool {
    parse_secret_placeholder(&format!("${name}")).is_some()
}

pub fn load_secrets_env_file(path: &Path) -> Result<BTreeMap<String, String>, RubError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    ensure_secure_secrets_permissions(path)?;
    let content = fs::read_to_string(path).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cannot read secrets file: {error}"),
            serde_json::json!({ "path": path.display().to_string() }),
        )
    })?;
    parse_secrets_env(&content, path)
}

pub fn parse_secrets_env(content: &str, path: &Path) -> Result<BTreeMap<String, String>, RubError> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in content.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(invalid_secrets_env_entry(
                path,
                line_no,
                "expected KEY=VALUE",
            ));
        };
        let key = raw_key.trim();
        if !is_valid_secret_name(key) {
            return Err(invalid_secrets_env_entry(
                path,
                line_no,
                "invalid secret name; use letters, digits, and underscores",
            ));
        }
        let value = decode_secret_value(raw_value.trim(), path, line_no)?;
        values.insert(key.to_string(), value);
    }
    Ok(values)
}

pub fn render_secrets_env(values: &BTreeMap<String, String>) -> String {
    let mut rendered = String::new();
    for (key, value) in values {
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&encode_secret_value(value));
        rendered.push('\n');
    }
    rendered
}

pub fn write_secrets_env_file(
    path: &Path,
    values: &BTreeMap<String, String>,
) -> Result<(), RubError> {
    let content = render_secrets_env(values);
    atomic_write_bytes(path, content.as_bytes(), 0o600).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::IoError,
            format!("Failed to write secrets file: {error}"),
            serde_json::json!({ "path": path.display().to_string() }),
        )
    })?;
    Ok(())
}

pub fn remove_secrets_env_file(path: &Path) -> Result<(), RubError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RubError::domain_with_context(
            ErrorCode::IoError,
            format!("Failed to remove secrets file: {error}"),
            serde_json::json!({ "path": path.display().to_string() }),
        )),
    }
}

#[cfg(unix)]
pub fn ensure_secure_secrets_permissions(path: &Path) -> Result<(), RubError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cannot stat secrets file: {error}"),
            serde_json::json!({ "path": path.display().to_string() }),
        )
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Insecure permissions on secrets.env; expected chmod 600".to_string(),
            serde_json::json!({
                "path": path.display().to_string(),
                "mode": format!("{mode:o}"),
                "required_mode": "600",
            }),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn ensure_secure_secrets_permissions(_path: &Path) -> Result<(), RubError> {
    Ok(())
}

fn invalid_secrets_env_entry(path: &Path, line: usize, reason: &str) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Invalid secrets.env entry on line {line}: {reason}"),
        serde_json::json!({
            "path": path.display().to_string(),
            "line": line,
        }),
    )
}

fn decode_secret_value(value: &str, path: &Path, line: usize) -> Result<String, RubError> {
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).map_err(|_| {
            invalid_secrets_env_entry(path, line, "invalid double-quoted value encoding")
        });
    }
    if let Some(unquoted) = strip_matching_single_quotes(value) {
        return Ok(unquoted.to_string());
    }
    Ok(value.to_string())
}

fn strip_matching_single_quotes(value: &str) -> Option<&str> {
    if value.len() < 2 {
        return None;
    }
    let bytes = value.as_bytes();
    let first = bytes.first().copied()?;
    let last = bytes.last().copied()?;
    if first == b'\'' && last == b'\'' {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

fn encode_secret_value(value: &str) -> String {
    if should_emit_quoted_secret_value(value) {
        serde_json::to_string(value).expect("secret values should serialize")
    } else {
        value.to_string()
    }
}

fn should_emit_quoted_secret_value(value: &str) -> bool {
    value.is_empty()
        || value.trim() != value
        || value.contains('\n')
        || value.contains('\r')
        || value.contains('"')
        || value.contains('\'')
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_secure_secrets_permissions, is_valid_secret_name, load_secrets_env_file,
        parse_secret_placeholder, parse_secrets_env, remove_secrets_env_file, render_secrets_env,
        write_secrets_env_file,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parse_secret_placeholder_accepts_expected_names() {
        assert_eq!(parse_secret_placeholder("$RUB_TOKEN"), Some("RUB_TOKEN"));
        assert_eq!(parse_secret_placeholder("RUB_TOKEN"), None);
        assert_eq!(parse_secret_placeholder("$1BAD"), None);
    }

    #[test]
    fn parse_secrets_env_supports_raw_json_quoted_and_single_quoted_values() {
        let parsed = parse_secrets_env(
            "RAW=hunter2\nJSON=\" spaced value \"\nSINGLE='quoted value'\n",
            Path::new("/tmp/secrets.env"),
        )
        .expect("parse should succeed");

        assert_eq!(parsed["RAW"], "hunter2");
        assert_eq!(parsed["JSON"], " spaced value ");
        assert_eq!(parsed["SINGLE"], "quoted value");
    }

    #[test]
    fn render_and_load_secrets_env_round_trip_values() {
        let path = unique_temp_path("secrets.env");
        let mut values = BTreeMap::new();
        values.insert("RUB_A".to_string(), "hunter2".to_string());
        values.insert("RUB_B".to_string(), " spaced value ".to_string());
        values.insert("RUB_C".to_string(), "line with \"quotes\"".to_string());

        write_secrets_env_file(&path, &values).expect("write should succeed");
        let loaded = load_secrets_env_file(&path).expect("load should succeed");

        assert_eq!(loaded, values);
    }

    #[test]
    fn render_secrets_env_sorts_keys_stably() {
        let values = BTreeMap::from([
            ("B".to_string(), "two".to_string()),
            ("A".to_string(), "one".to_string()),
        ]);

        assert_eq!(render_secrets_env(&values), "A=one\nB=two\n");
    }

    #[test]
    fn remove_secrets_env_file_is_idempotent() {
        let path = unique_temp_path("secrets.env");
        remove_secrets_env_file(&path).expect("missing file removal should succeed");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_secure_secrets_permissions_rejects_group_readable_files() {
        use std::os::unix::fs::PermissionsExt;

        let path = unique_temp_path("secrets.env");
        std::fs::write(&path, "RUB_TOKEN=hunter2\n").expect("write file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set permissions");

        let error = ensure_secure_secrets_permissions(&path)
            .expect_err("group-readable file should be rejected")
            .into_envelope();
        assert_eq!(error.code, crate::error::ErrorCode::InvalidInput);
    }

    #[test]
    fn validate_secret_name_accepts_expected_shape() {
        assert!(is_valid_secret_name("RUB_TOKEN"));
        assert!(is_valid_secret_name("_TOKEN"));
        assert!(!is_valid_secret_name("bad-name"));
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rub-secrets-env-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create temp root");
        root.join(name)
    }
}
