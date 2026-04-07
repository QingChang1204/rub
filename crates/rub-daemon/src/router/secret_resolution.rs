use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::request_args::parse_json_spec;
use crate::rub_paths::RubPaths;
use rub_core::error::{ErrorCode, RubError};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SecretSource {
    Environment,
    SecretsFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretResolutionEntry {
    pub reference: String,
    pub value: String,
    pub source: SecretSource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SecretResolutionMetadata {
    entries: Vec<SecretResolutionEntry>,
}

impl SecretResolutionMetadata {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn record(&mut self, reference: String, value: String, source: SecretSource) {
        if self
            .entries
            .iter()
            .any(|entry| entry.reference == reference && entry.value == value)
        {
            return;
        }
        self.entries.push(SecretResolutionEntry {
            reference,
            value,
            source,
        });
        self.entries
            .sort_by(|left, right| right.value.len().cmp(&left.value.len()));
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedJsonSpec<T> {
    pub value: T,
    pub metadata: SecretResolutionMetadata,
}

#[derive(Debug, Default)]
struct SecretSources {
    process_env: BTreeMap<String, String>,
    file_env: BTreeMap<String, String>,
}

impl SecretSources {
    fn load(rub_home: &Path) -> Result<Self, RubError> {
        let process_env = std::env::vars().collect::<BTreeMap<_, _>>();
        let file_env = load_secrets_file(rub_home)?;
        Ok(Self {
            process_env,
            file_env,
        })
    }

    fn resolve(&self, key: &str) -> Option<(&str, SecretSource)> {
        if let Some(value) = self.process_env.get(key) {
            return Some((value.as_str(), SecretSource::Environment));
        }
        self.file_env
            .get(key)
            .map(|value| (value.as_str(), SecretSource::SecretsFile))
    }

    fn reference_for_value(&self, value: &str) -> Option<(String, SecretSource)> {
        if value.is_empty() {
            return None;
        }
        if let Some((key, _)) = self
            .process_env
            .iter()
            .find(|(_, candidate)| candidate.as_str() == value)
        {
            return Some((format!("${key}"), SecretSource::Environment));
        }
        self.file_env
            .iter()
            .find(|(_, candidate)| candidate.as_str() == value)
            .map(|(key, _)| (format!("${key}"), SecretSource::SecretsFile))
    }
}

pub(crate) fn parse_json_spec_with_secret_resolution<T>(
    raw: &str,
    command: &str,
    rub_home: &Path,
) -> Result<ResolvedJsonSpec<T>, RubError>
where
    T: DeserializeOwned,
{
    let spec = parse_json_value(raw, command)?;
    resolve_json_value_with_secret_resolution(spec, command, rub_home)
}

/// Resolve secret placeholders in an already-parsed JSON `Value` and
/// deserialize into the target type.  Use this when the caller needs to
/// pre-process the parsed JSON (e.g. shorthand normalization) before
/// secret resolution and typed deserialization — avoids a redundant
/// string → parse → string round-trip.
pub(crate) fn resolve_json_value_with_secret_resolution<T>(
    mut spec: Value,
    command: &str,
    rub_home: &Path,
) -> Result<ResolvedJsonSpec<T>, RubError>
where
    T: DeserializeOwned,
{
    let sources = SecretSources::load(rub_home)?;
    let mut metadata = SecretResolutionMetadata::default();
    resolve_placeholders(&mut spec, command, &sources, &mut metadata)?;
    let value = serde_json::from_value(spec).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid JSON spec for '{command}': {error}"),
        )
    })?;
    Ok(ResolvedJsonSpec { value, metadata })
}

pub(crate) fn redact_json_value(value: &mut Value, metadata: &SecretResolutionMetadata) {
    if metadata.is_empty() {
        return;
    }
    match value {
        Value::String(text) => redact_string(text, metadata),
        Value::Array(values) => {
            for value in values {
                redact_json_value(value, metadata);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                redact_json_value(value, metadata);
            }
        }
        _ => {}
    }
}

pub(crate) fn redact_json_value_from_secret_sources(
    value: &mut Value,
    rub_home: &Path,
) -> Result<SecretResolutionMetadata, RubError> {
    let sources = SecretSources::load(rub_home)?;
    let mut metadata = SecretResolutionMetadata::default();
    collect_secret_matches_and_redact_exact_leaves(value, &sources, &mut metadata);
    Ok(metadata)
}

pub(crate) fn redact_rub_error(error: RubError, metadata: &SecretResolutionMetadata) -> RubError {
    if metadata.is_empty() {
        return error;
    }
    match error {
        RubError::Domain(mut envelope) => {
            redact_string(&mut envelope.message, metadata);
            redact_string(&mut envelope.suggestion, metadata);
            if let Some(context) = envelope.context.as_mut() {
                redact_json_value(context, metadata);
            }
            RubError::Domain(envelope)
        }
        RubError::Internal(mut message) => {
            redact_string(&mut message, metadata);
            RubError::Internal(message)
        }
        other => other,
    }
}

fn parse_json_value(raw: &str, command: &str) -> Result<Value, RubError> {
    parse_json_spec(raw, command)
}

fn resolve_placeholders(
    value: &mut Value,
    command: &str,
    sources: &SecretSources,
    metadata: &mut SecretResolutionMetadata,
) -> Result<(), RubError> {
    match value {
        Value::String(text) => {
            let Some(secret_name) = parse_secret_placeholder(text) else {
                return Ok(());
            };
            let Some((resolved, source)) = sources.resolve(secret_name) else {
                return Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("Unresolved secret reference '${secret_name}' in '{command}' spec"),
                    serde_json::json!({
                        "command": command,
                        "reference": format!("${secret_name}"),
                        "sources": [
                            "environment",
                            "RUB_HOME/secrets.env"
                        ],
                    }),
                ));
            };
            metadata.record(format!("${secret_name}"), resolved.to_string(), source);
            *text = resolved.to_string();
            Ok(())
        }
        Value::Array(values) => {
            for value in values {
                resolve_placeholders(value, command, sources, metadata)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                resolve_placeholders(value, command, sources, metadata)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn parse_secret_placeholder(value: &str) -> Option<&str> {
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

fn load_secrets_file(rub_home: &Path) -> Result<BTreeMap<String, String>, RubError> {
    let path = RubPaths::new(rub_home).secrets_env_path();
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    ensure_secure_secrets_permissions(&path)?;
    let content = fs::read_to_string(&path).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cannot read secrets file: {error}"),
            serde_json::json!({ "path": path.display().to_string() }),
        )
    })?;
    parse_secrets_env(&content, &path)
}

fn parse_secrets_env(content: &str, path: &Path) -> Result<BTreeMap<String, String>, RubError> {
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
        if parse_secret_placeholder(&format!("${key}")).is_none() {
            return Err(invalid_secrets_env_entry(
                path,
                line_no,
                "invalid secret name; use letters, digits, and underscores",
            ));
        }
        let mut value = raw_value.trim().to_string();
        if let Some(unquoted) = strip_matching_quotes(&value) {
            value = unquoted.to_string();
        }
        values.insert(key.to_string(), value);
    }
    Ok(values)
}

fn strip_matching_quotes(value: &str) -> Option<&str> {
    if value.len() < 2 {
        return None;
    }
    let bytes = value.as_bytes();
    let first = bytes.first().copied()?;
    let last = bytes.last().copied()?;
    if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
        return Some(&value[1..value.len() - 1]);
    }
    None
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

#[cfg(unix)]
fn ensure_secure_secrets_permissions(path: &Path) -> Result<(), RubError> {
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
fn ensure_secure_secrets_permissions(_path: &Path) -> Result<(), RubError> {
    Ok(())
}

fn redact_string(text: &mut String, metadata: &SecretResolutionMetadata) {
    if text.is_empty() {
        return;
    }
    let mut redacted = text.clone();
    for entry in &metadata.entries {
        if entry.value.is_empty() {
            continue;
        }
        if redacted.contains(&entry.value) {
            redacted = redacted.replace(&entry.value, &entry.reference);
        }
    }
    *text = redacted;
}

fn collect_secret_matches_and_redact_exact_leaves(
    value: &mut Value,
    sources: &SecretSources,
    metadata: &mut SecretResolutionMetadata,
) {
    match value {
        Value::String(text) => {
            if let Some((reference, source)) = sources.reference_for_value(text) {
                metadata.record(reference.clone(), text.clone(), source);
                *text = reference;
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_secret_matches_and_redact_exact_leaves(value, sources, metadata);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                collect_secret_matches_and_redact_exact_leaves(value, sources, metadata);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SecretResolutionMetadata, SecretSource, SecretSources,
        parse_json_spec_with_secret_resolution, redact_json_value,
        redact_json_value_from_secret_sources, redact_rub_error,
    };
    use rub_core::error::{ErrorCode, RubError};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct DemoSpec {
        username: String,
        nested: NestedSpec,
    }

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct NestedSpec {
        password: String,
    }

    #[test]
    fn parse_json_spec_with_secret_resolution_resolves_placeholders_recursively() {
        let home = unique_temp_home();
        std::fs::write(
            home.join("secrets.env"),
            "RUB_USER=alice\nRUB_PASSWORD=hunter2\n",
        )
        .expect("write secrets.env");
        set_secure_permissions(&home.join("secrets.env"));

        let resolved = parse_json_spec_with_secret_resolution::<DemoSpec>(
            r#"{"username":"$RUB_USER","nested":{"password":"$RUB_PASSWORD"}}"#,
            "pipe",
            &home,
        )
        .expect("secret resolution should succeed");

        assert_eq!(
            resolved.value,
            DemoSpec {
                username: "alice".to_string(),
                nested: NestedSpec {
                    password: "hunter2".to_string(),
                },
            }
        );
        assert_eq!(resolved.metadata.entries.len(), 2);
        assert_eq!(resolved.metadata.entries[0].reference, "$RUB_PASSWORD");
        assert_eq!(resolved.metadata.entries[1].reference, "$RUB_USER");
    }

    #[test]
    fn secret_sources_prioritize_process_env_over_secrets_file() {
        let sources = SecretSources {
            process_env: BTreeMap::from([("RUB_TOKEN".to_string(), "env-token".to_string())]),
            file_env: BTreeMap::from([("RUB_TOKEN".to_string(), "file-token".to_string())]),
        };
        let (value, source) = sources.resolve("RUB_TOKEN").expect("secret should resolve");
        assert_eq!(value, "env-token");
        assert_eq!(source, SecretSource::Environment);
    }

    #[test]
    fn redact_json_value_replaces_secret_values_with_references() {
        let mut value = serde_json::json!({
            "result": { "text": "hunter2" },
            "summary": "submitted hunter2 to the page"
        });
        let metadata = SecretResolutionMetadata {
            entries: vec![super::SecretResolutionEntry {
                reference: "$RUB_PASSWORD".to_string(),
                value: "hunter2".to_string(),
                source: SecretSource::SecretsFile,
            }],
        };

        redact_json_value(&mut value, &metadata);

        assert_eq!(value["result"]["text"], "$RUB_PASSWORD");
        assert_eq!(value["summary"], "submitted $RUB_PASSWORD to the page");
    }

    #[test]
    fn redact_json_value_from_secret_sources_uses_exact_leaf_matches() {
        let home = unique_temp_home();
        std::fs::write(home.join("secrets.env"), "RUB_TOKEN=secret-token\n").expect("write file");
        set_secure_permissions(&home.join("secrets.env"));

        let mut value = serde_json::json!({
            "token": "secret-token",
            "note": "prefix-secret-token-suffix"
        });

        let metadata =
            redact_json_value_from_secret_sources(&mut value, &home).expect("redaction succeeds");

        assert_eq!(value["token"], "$RUB_TOKEN");
        assert_eq!(value["note"], "prefix-secret-token-suffix");
        assert_eq!(metadata.entries.len(), 1);
    }

    #[test]
    fn redact_rub_error_rewrites_domain_message_and_context() {
        let metadata = SecretResolutionMetadata {
            entries: vec![super::SecretResolutionEntry {
                reference: "$RUB_PASSWORD".to_string(),
                value: "hunter2".to_string(),
                source: SecretSource::SecretsFile,
            }],
        };
        let error = RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "typed_text=hunter2",
            serde_json::json!({ "secret": "hunter2" }),
        );

        let redacted = redact_rub_error(error, &metadata).into_envelope();
        assert_eq!(redacted.message, "typed_text=$RUB_PASSWORD");
        assert_eq!(
            redacted.context.expect("context")["secret"],
            serde_json::json!("$RUB_PASSWORD")
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_json_spec_with_secret_resolution_rejects_insecure_secrets_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let home = unique_temp_home();
        let path = home.join("secrets.env");
        std::fs::write(&path, "RUB_SECRET=hunter2\n").expect("write secrets.env");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set permissions");

        let error = parse_json_spec_with_secret_resolution::<serde_json::Value>(
            r#"{"secret":"$RUB_SECRET"}"#,
            "pipe",
            &home,
        )
        .expect_err("insecure permissions should fail");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("Insecure permissions"));
    }

    #[test]
    fn parse_json_spec_with_secret_resolution_reports_missing_reference() {
        let home = unique_temp_home();
        let error = parse_json_spec_with_secret_resolution::<serde_json::Value>(
            r#"{"secret":"$RUB_MISSING"}"#,
            "fill",
            &home,
        )
        .expect_err("missing secret should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("$RUB_MISSING"));
    }

    fn unique_temp_home() -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rub-secret-resolution-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp home");
        path
    }

    #[cfg(unix)]
    fn set_secure_permissions(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("set secure permissions");
    }

    #[cfg(not(unix))]
    fn set_secure_permissions(_path: &std::path::Path) {}
}
