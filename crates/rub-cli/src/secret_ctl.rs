use crate::binding_ctl::binding_path_state;
use crate::commands::{EffectiveCli, SecretSubcommand};
use crate::output::{self, InteractionTraceMode};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::fs::FileCommitOutcome;
use rub_core::secrets_env::{
    SecretEffectiveSource, SecretsEnvPersistOutcome, is_valid_secret_name,
};
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::Path;

const SECRET_REGISTRY_SCHEMA_VERSION: u32 = 1;

mod input;
mod store;

use self::input::{SecretInputMode, resolve_secret_input_value, secret_input_mode};
use self::store::{update_secret_store, with_secret_store};

pub(crate) fn handle_secret_command(
    cli: &EffectiveCli,
    subcommand: &SecretSubcommand,
) -> Result<(), RubError> {
    let data = match subcommand {
        SecretSubcommand::List => project_secret_list(&cli.rub_home)?,
        SecretSubcommand::Inspect { name } => inspect_secret_value(&cli.rub_home, name)?,
        SecretSubcommand::Set {
            name,
            value,
            from_env,
            stdin,
        } => set_secret_value(
            &cli.rub_home,
            name,
            value.as_deref(),
            from_env.as_deref(),
            *stdin,
        )?,
        SecretSubcommand::Remove { name } => remove_secret_value(&cli.rub_home, name)?,
    };

    let output = output::format_cli_success(
        "secret",
        &cli.session,
        &cli.rub_home,
        data,
        cli.json_pretty,
        InteractionTraceMode::Compact,
    );
    println!("{output}");
    Ok(())
}

fn project_secret_list(rub_home: &Path) -> Result<Value, RubError> {
    with_secret_store(rub_home, |secrets| {
        let items = secrets
            .keys()
            .map(|name| {
                let environment_override_present = secret_environment_override_present(name);
                json!({
                    "name": name,
                    "source": "rub_home_secrets_env",
                    "effective_source": effective_secret_source(true, environment_override_present),
                    "environment_override_present": environment_override_present,
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "subject": secret_registry_subject(rub_home),
            "result": {
                "schema_version": SECRET_REGISTRY_SCHEMA_VERSION,
                "count": items.len(),
                "items": items,
            }
        }))
    })
}

fn inspect_secret_value(rub_home: &Path, name: &str) -> Result<Value, RubError> {
    let name = normalize_secret_name(name)?;
    with_secret_store(rub_home, |secrets| {
        let local_store_present = secrets.contains_key(&name);
        let environment_override_present = secret_environment_override_present(&name);
        let effective_source =
            effective_secret_source(local_store_present, environment_override_present);

        if matches!(effective_source, SecretEffectiveSource::Unresolved) {
            return Err(secret_name_not_found_error(rub_home, &name));
        }

        Ok(json!({
            "subject": secret_subject(rub_home, &name),
            "result": {
                "secret": {
                    "name": name,
                    "reference": format!("${name}"),
                    "local_store_present": local_store_present,
                    "environment_override_present": environment_override_present,
                    "effective_source": effective_source,
                    "store_source": if local_store_present {
                        Some("rub_home_secrets_env")
                    } else {
                        None::<&str>
                    },
                }
            }
        }))
    })
}

fn set_secret_value(
    rub_home: &Path,
    name: &str,
    inline_value: Option<&str>,
    from_env: Option<&str>,
    stdin: bool,
) -> Result<Value, RubError> {
    let name = normalize_secret_name(name)?;
    let input_mode = secret_input_mode(inline_value, from_env, stdin)?;
    let resolved_value = resolve_secret_input_value(input_mode, inline_value, from_env)?;

    let (mut payload, persist_outcome) = update_secret_store(rub_home, |secrets| {
        let action = if secrets.contains_key(&name) {
            "updated"
        } else {
            "created"
        };
        secrets.insert(name.clone(), resolved_value);

        Ok(json!({
            "subject": secret_subject(rub_home, &name),
            "result": {
                "action": action,
                "secret": {
                    "name": name,
                    "source": "rub_home_secrets_env",
                },
                "input_mode": match input_mode {
                    SecretInputMode::InlineValue => "inline_value",
                    SecretInputMode::Environment => "environment",
                    SecretInputMode::Stdin => "stdin",
                },
            }
        }))
    })?;
    annotate_secret_registry_persistence(&mut payload, persist_outcome);
    Ok(payload)
}

fn remove_secret_value(rub_home: &Path, name: &str) -> Result<Value, RubError> {
    let name = normalize_secret_name(name)?;
    let (mut payload, persist_outcome) = update_secret_store(rub_home, |secrets| {
        if secrets.remove(&name).is_none() {
            return Err(secret_name_not_found_error(rub_home, &name));
        }

        Ok(json!({
            "subject": secret_subject(rub_home, &name),
            "result": {
                "removed_name": name,
            }
        }))
    })?;
    annotate_secret_registry_persistence(&mut payload, persist_outcome);
    Ok(payload)
}

fn normalize_secret_name(name: &str) -> Result<String, RubError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Secret name cannot be empty",
        ));
    }
    if !is_valid_secret_name(trimmed) {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid secret name: {trimmed}"),
            json!({
                "name": trimmed,
                "reason": "invalid_secret_name",
                "allowed_pattern": "[A-Za-z_][A-Za-z0-9_]*",
            }),
        ));
    }
    Ok(trimmed.to_string())
}

fn secret_registry_subject(rub_home: &Path) -> Value {
    let paths = RubPaths::new(rub_home);
    json!({
        "kind": "secret_registry",
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": secret_path_state(
            "cli.secret.subject.rub_home",
            "cli_secret_registry",
            "rub_home_directory",
        ),
        "secrets_path": paths.secrets_env_path().display().to_string(),
        "secrets_path_state": secret_path_state(
            "cli.secret.subject.secrets_path",
            "cli_secret_registry",
            "secrets_env_file",
        ),
        "lock_path": paths.secrets_env_lock_path().display().to_string(),
        "lock_path_state": secret_path_state(
            "cli.secret.subject.lock_path",
            "cli_secret_registry",
            "secrets_env_lock",
        ),
    })
}

fn secret_subject(rub_home: &Path, name: &str) -> Value {
    json!({
        "kind": "secret_name",
        "name": name,
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": secret_path_state(
            "cli.secret.subject.rub_home",
            "cli_secret_registry",
            "rub_home_directory",
        ),
    })
}

fn secret_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> rub_core::model::PathReferenceState {
    binding_path_state(path_authority, upstream_truth, path_kind)
}

fn secret_name_not_found_error(rub_home: &Path, name: &str) -> RubError {
    let paths = RubPaths::new(rub_home);
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Secret not found: {name}"),
        json!({
            "name": name,
            "secrets_path": paths.secrets_env_path().display().to_string(),
            "secrets_path_state": secret_path_state(
                "cli.secret.subject.secrets_path",
                "cli_secret_registry",
                "secrets_env_file",
            ),
            "reason": "secret_name_not_found",
        }),
    )
}

fn effective_secret_source(
    local_store_present: bool,
    environment_override_present: bool,
) -> SecretEffectiveSource {
    if environment_override_present {
        SecretEffectiveSource::Environment
    } else if local_store_present {
        SecretEffectiveSource::RubHomeSecretsEnv
    } else {
        SecretEffectiveSource::Unresolved
    }
}

fn secret_environment_override_present(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn annotate_secret_registry_persistence(payload: &mut Value, outcome: SecretsEnvPersistOutcome) {
    let Some(result) = payload.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };

    let durability = match outcome.durability() {
        Some(FileCommitOutcome::Durable) => "durable",
        Some(FileCommitOutcome::Published) => "published",
        None => "not_applicable",
    };
    result.insert(
        "projection_state".to_string(),
        json!({
            "truth_level": "local_persistence_projection",
            "projection_kind": "cli_secret_registry_mutation",
            "projection_authority": "cli.secret.registry_persistence",
            "upstream_commit_truth": "secrets_env_mutation_attempt",
            "control_role": "display_only",
            "durability": durability,
            "persist_action": outcome.action(),
        }),
    );
    if matches!(outcome.durability(), Some(FileCommitOutcome::Published)) {
        result.insert("durability_confirmed".to_string(), Value::Bool(false));
    }
}

pub(super) fn secret_registry_io_error(
    rub_home: &Path,
    path: &Path,
    path_authority: &str,
    path_kind: &str,
    reason: &str,
    error: std::io::Error,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IoError,
        format!("Secret registry IO failure: {error}"),
        json!({
            "rub_home": rub_home.display().to_string(),
            "path": path.display().to_string(),
            "path_state": secret_path_state(
                path_authority,
                "cli_secret_registry",
                path_kind,
            ),
            "reason": reason,
        }),
    )
}

pub(super) fn secret_registry_error(
    rub_home: &Path,
    path: &Path,
    path_authority: &str,
    path_kind: &str,
    reason: &str,
    error: RubError,
) -> RubError {
    let mut envelope = error.into_envelope();
    let mut context = match envelope.context.take() {
        Some(serde_json::Value::Object(existing)) => existing,
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("previous_context".to_string(), other);
            object
        }
        None => serde_json::Map::new(),
    };
    context.insert(
        "rub_home".to_string(),
        json!(rub_home.display().to_string()),
    );
    context.insert("path".to_string(), json!(path.display().to_string()));
    context.insert(
        "path_state".to_string(),
        serde_json::to_value(secret_path_state(
            path_authority,
            "cli_secret_registry",
            path_kind,
        ))
        .unwrap_or_else(|_| json!({})),
    );
    context.insert("reason".to_string(), json!(reason));
    RubError::Domain(ErrorEnvelope {
        context: Some(serde_json::Value::Object(context)),
        ..envelope
    })
}

#[cfg(test)]
mod tests;
