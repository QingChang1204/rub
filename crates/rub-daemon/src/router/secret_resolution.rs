use std::collections::BTreeMap;
use std::path::Path;

use crate::rub_paths::RubPaths;
use rub_core::error::{ErrorCode, RubError};
use rub_core::secrets_env::{
    SecretEffectiveSource, SecretReferenceProvenance, SecretReferenceProvenanceProjection,
    load_secrets_env_file, parse_secret_placeholder,
};
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
        let file_env = load_secrets_env_file(&RubPaths::new(rub_home).secrets_env_path())?;
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

pub(crate) fn attach_secret_resolution_projection(
    payload: &mut Value,
    metadata: &SecretResolutionMetadata,
) {
    let Some(projection) = project_secret_resolution_projection(metadata) else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert(
        "input_secret_references".to_string(),
        serde_json::to_value(projection).expect("secret provenance should serialize"),
    );
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

fn project_secret_resolution_projection(
    metadata: &SecretResolutionMetadata,
) -> Option<SecretReferenceProvenanceProjection> {
    if metadata.is_empty() {
        return None;
    }
    let mut items = metadata
        .entries
        .iter()
        .map(|entry| SecretReferenceProvenance {
            reference: entry.reference.clone(),
            effective_source: project_secret_effective_source(&entry.source),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.reference.cmp(&right.reference));
    Some(SecretReferenceProvenanceProjection {
        count: items.len(),
        items,
    })
}

fn project_secret_effective_source(source: &SecretSource) -> SecretEffectiveSource {
    match source {
        SecretSource::Environment => SecretEffectiveSource::Environment,
        SecretSource::SecretsFile => SecretEffectiveSource::RubHomeSecretsEnv,
    }
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
        SecretResolutionMetadata, SecretSource, SecretSources, attach_secret_resolution_projection,
        redact_json_value, redact_json_value_from_secret_sources, redact_rub_error,
        resolve_json_value_with_secret_resolution,
    };
    use crate::router::request_args::parse_json_spec;
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

        let spec = parse_json_spec(
            r#"{"username":"$RUB_USER","nested":{"password":"$RUB_PASSWORD"}}"#,
            "pipe",
        )
        .expect("raw json spec should parse");
        let resolved = resolve_json_value_with_secret_resolution::<DemoSpec>(spec, "pipe", &home)
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

    #[test]
    fn attach_secret_resolution_projection_reports_reference_sources_without_values() {
        let metadata = SecretResolutionMetadata {
            entries: vec![
                super::SecretResolutionEntry {
                    reference: "$RUB_PASSWORD".to_string(),
                    value: "hunter2".to_string(),
                    source: SecretSource::SecretsFile,
                },
                super::SecretResolutionEntry {
                    reference: "$RUB_USER".to_string(),
                    value: "alice".to_string(),
                    source: SecretSource::Environment,
                },
            ],
        };
        let mut payload = serde_json::json!({
            "steps": []
        });

        attach_secret_resolution_projection(&mut payload, &metadata);

        assert_eq!(payload["input_secret_references"]["count"], 2);
        assert_eq!(
            payload["input_secret_references"]["items"][0]["reference"],
            "$RUB_PASSWORD"
        );
        assert_eq!(
            payload["input_secret_references"]["items"][0]["effective_source"],
            "rub_home_secrets_env"
        );
        assert_eq!(
            payload["input_secret_references"]["items"][1]["reference"],
            "$RUB_USER"
        );
        assert_eq!(
            payload["input_secret_references"]["items"][1]["effective_source"],
            "environment"
        );
        assert!(!payload.to_string().contains("hunter2"));
        assert!(!payload.to_string().contains("alice"));
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

        let spec =
            parse_json_spec(r#"{"secret":"$RUB_SECRET"}"#, "pipe").expect("raw json spec parses");
        let error =
            resolve_json_value_with_secret_resolution::<serde_json::Value>(spec, "pipe", &home)
                .expect_err("insecure permissions should fail");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("Insecure permissions"));
    }

    #[test]
    fn parse_json_spec_with_secret_resolution_reports_missing_reference() {
        let home = unique_temp_home();
        let spec =
            parse_json_spec(r#"{"secret":"$RUB_MISSING"}"#, "fill").expect("raw json spec parses");
        let error =
            resolve_json_value_with_secret_resolution::<serde_json::Value>(spec, "fill", &home)
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
