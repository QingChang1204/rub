use crate::commands::{
    InterceptSubcommand, InterferenceModeArg, InterferenceSubcommand, OrchestrationSubcommand,
    TriggerSubcommand,
};
use rub_core::error::{ErrorCode, RubError};
use rub_daemon::orchestration_assets::load_named_orchestration_spec_with_authority;
use rub_ipc::protocol::IpcRequest;
use std::path::Path;

use super::super::{mutating_request, resolve_cli_path};
use crate::timeout_budget::helpers::input_path_reference_state;

pub(crate) fn build_trigger_request(
    timeout: u64,
    subcommand: &TriggerSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        TriggerSubcommand::Add { file, paused } => {
            let path = resolve_cli_path(file);
            let path_string = path.display().to_string();
            let path_state = input_path_reference_state(
                "cli.trigger.spec_source.path",
                "cli_trigger_file_option",
                "trigger_registration_file",
            );
            let spec = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => RubError::domain_with_context(
                    ErrorCode::FileNotFound,
                    format!("Trigger spec file not found: {path_string}"),
                    serde_json::json!({
                        "path": path_string,
                        "path_state": path_state,
                        "reason": "trigger_spec_file_not_found",
                    }),
                ),
                _ => RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("Failed to read trigger spec file {path_string}: {error}"),
                    serde_json::json!({
                        "path": path_string,
                        "path_state": path_state,
                        "reason": "trigger_spec_file_read_failed",
                    }),
                ),
            })?;
            Ok(mutating_request(
                "trigger",
                serde_json::json!({
                    "sub": "add",
                    "spec": spec,
                    "paused": paused,
                    "spec_source": {
                        "kind": "file",
                        "path": path_string,
                        "path_state": input_path_reference_state(
                            "cli.trigger.spec_source.path",
                            "cli_trigger_file_option",
                            "trigger_registration_file",
                        ),
                    }
                }),
                timeout,
            ))
        }
        TriggerSubcommand::List => Ok(IpcRequest::new(
            "trigger",
            serde_json::json!({ "sub": "list" }),
            timeout,
        )),
        TriggerSubcommand::Trace { last } => Ok(IpcRequest::new(
            "trigger",
            serde_json::json!({
                "sub": "trace",
                "last": last,
            }),
            timeout,
        )),
        TriggerSubcommand::Remove { id } => Ok(mutating_request(
            "trigger",
            serde_json::json!({
                "sub": "remove",
                "id": id,
            }),
            timeout,
        )),
        TriggerSubcommand::Pause { id } => Ok(mutating_request(
            "trigger",
            serde_json::json!({
                "sub": "pause",
                "id": id,
            }),
            timeout,
        )),
        TriggerSubcommand::Resume { id } => Ok(mutating_request(
            "trigger",
            serde_json::json!({
                "sub": "resume",
                "id": id,
            }),
            timeout,
        )),
    }
}

pub(crate) fn build_orchestration_request(
    timeout: u64,
    rub_home: &Path,
    subcommand: &OrchestrationSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        OrchestrationSubcommand::Add {
            file,
            asset,
            paused,
        } => {
            let (spec, spec_source) = match (file.as_ref(), asset.as_ref()) {
                (Some(file), None) => {
                    let path = resolve_cli_path(file);
                    let path_string = path.display().to_string();
                    let path_state = input_path_reference_state(
                        "cli.orchestration.spec_source.path",
                        "cli_orchestration_file_option",
                        "orchestration_registration_file",
                    );
                    let spec =
                        std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                            std::io::ErrorKind::NotFound => RubError::domain_with_context(
                                ErrorCode::FileNotFound,
                                format!("Orchestration spec file not found: {path_string}"),
                                serde_json::json!({
                                    "path": path_string,
                                    "path_state": path_state,
                                    "reason": "orchestration_spec_file_not_found",
                                }),
                            ),
                            _ => RubError::domain_with_context(
                                ErrorCode::InvalidInput,
                                format!(
                                    "Failed to read orchestration spec file {path_string}: {error}"
                                ),
                                serde_json::json!({
                                    "path": path_string,
                                    "path_state": path_state,
                                    "reason": "orchestration_spec_file_read_failed",
                                }),
                            ),
                        })?;
                    (
                        spec,
                        serde_json::json!({
                            "kind": "file",
                            "path": path_string,
                            "path_state": input_path_reference_state(
                                "cli.orchestration.spec_source.path",
                                "cli_orchestration_file_option",
                                "orchestration_registration_file",
                            ),
                        }),
                    )
                }
                (None, Some(asset)) => {
                    let (name, spec, path) = load_named_orchestration_spec_with_authority(
                        rub_home,
                        asset,
                        "cli.orchestration.spec_source.path",
                        "cli_orchestration_asset_option",
                    )?;
                    (
                        spec,
                        serde_json::json!({
                            "kind": "asset",
                            "name": name,
                            "path": path.display().to_string(),
                            "path_state": input_path_reference_state(
                                "cli.orchestration.spec_source.path",
                                "cli_orchestration_asset_option",
                                "orchestration_asset_reference",
                            ),
                        }),
                    )
                }
                _ => {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        "Provide exactly one orchestration source: --file or --asset",
                    ));
                }
            };
            Ok(mutating_request(
                "orchestration",
                serde_json::json!({
                    "sub": "add",
                    "spec": spec,
                    "paused": paused,
                    "spec_source": spec_source,
                }),
                timeout,
            ))
        }
        OrchestrationSubcommand::List => Ok(IpcRequest::new(
            "orchestration",
            serde_json::json!({ "sub": "list" }),
            timeout,
        )),
        OrchestrationSubcommand::Trace { last } => Ok(IpcRequest::new(
            "orchestration",
            serde_json::json!({
                "sub": "trace",
                "last": last,
            }),
            timeout,
        )),
        OrchestrationSubcommand::ListAssets => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "orchestration list-assets is handled locally and should not build an IPC request",
        )),
        OrchestrationSubcommand::Remove { id } => Ok(mutating_request(
            "orchestration",
            serde_json::json!({
                "sub": "remove",
                "id": id,
            }),
            timeout,
        )),
        OrchestrationSubcommand::Pause { id } => Ok(mutating_request(
            "orchestration",
            serde_json::json!({
                "sub": "pause",
                "id": id,
            }),
            timeout,
        )),
        OrchestrationSubcommand::Resume { id } => Ok(mutating_request(
            "orchestration",
            serde_json::json!({
                "sub": "resume",
                "id": id,
            }),
            timeout,
        )),
        OrchestrationSubcommand::Execute { id, id_option } => {
            let id = match (id, id_option) {
                (Some(id), None) | (None, Some(id)) => *id,
                (Some(_), Some(_)) => {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        "Use either `orchestration execute <id>` or `orchestration execute --id <id>`, not both",
                    ));
                }
                (None, None) => {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        "orchestration execute requires a rule id",
                    ));
                }
            };
            Ok(mutating_request(
                "orchestration",
                serde_json::json!({
                    "sub": "execute",
                    "id": id,
                }),
                timeout,
            ))
        }
        OrchestrationSubcommand::Export { id, .. } => Ok(IpcRequest::new(
            "orchestration",
            serde_json::json!({
                "sub": "export",
                "id": id,
            }),
            timeout,
        )),
    }
}

pub(crate) fn build_intercept_request(
    timeout: u64,
    subcommand: &InterceptSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        InterceptSubcommand::Rewrite {
            source_pattern,
            target_base,
        } => Ok(mutating_request(
            "intercept",
            serde_json::json!({
                "sub": "rewrite",
                "source_pattern": source_pattern,
                "target_base": target_base,
            }),
            timeout,
        )),
        InterceptSubcommand::Block { url_pattern } => Ok(mutating_request(
            "intercept",
            serde_json::json!({
                "sub": "block",
                "url_pattern": url_pattern,
            }),
            timeout,
        )),
        InterceptSubcommand::Allow { url_pattern } => Ok(mutating_request(
            "intercept",
            serde_json::json!({
                "sub": "allow",
                "url_pattern": url_pattern,
            }),
            timeout,
        )),
        InterceptSubcommand::Header {
            url_pattern,
            name,
            value,
            headers,
        } => {
            let normalized_headers = normalize_header_overrides(name, value, headers)?;
            Ok(mutating_request(
                "intercept",
                serde_json::json!({
                    "sub": "header",
                    "url_pattern": url_pattern,
                    "headers": normalized_headers,
                }),
                timeout,
            ))
        }
        InterceptSubcommand::List => Ok(IpcRequest::new(
            "intercept",
            serde_json::json!({ "sub": "list" }),
            timeout,
        )),
        InterceptSubcommand::Remove { id } => Ok(mutating_request(
            "intercept",
            serde_json::json!({
                "sub": "remove",
                "id": id,
            }),
            timeout,
        )),
        InterceptSubcommand::Clear => Ok(mutating_request(
            "intercept",
            serde_json::json!({ "sub": "clear" }),
            timeout,
        )),
    }
}

fn normalize_header_overrides(
    name: &Option<String>,
    value: &Option<String>,
    headers: &[String],
) -> Result<Vec<String>, RubError> {
    match (name.as_deref(), value.as_deref(), headers.is_empty()) {
        (Some(name), Some(value), true) => Ok(vec![format!("{name}={value}")]),
        (None, None, false) => Ok(headers.to_vec()),
        (Some(_), None, _) | (None, Some(_), _) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "intercept header requires both NAME and VALUE when using the positional form",
        )),
        (Some(_), Some(_), false) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Use either `intercept header <pattern> <NAME> <VALUE>` or repeat `--header NAME=VALUE`, not both",
        )),
        (None, None, true) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "intercept header requires either `<NAME> <VALUE>` or one or more `--header NAME=VALUE` entries",
        )),
    }
}

pub(crate) fn build_interference_request(
    timeout: u64,
    subcommand: &InterferenceSubcommand,
) -> Result<IpcRequest, RubError> {
    match subcommand {
        InterferenceSubcommand::Mode { mode } => Ok(mutating_request(
            "interference",
            serde_json::json!({
                "sub": "mode",
                "mode": match mode {
                    InterferenceModeArg::Normal => "normal",
                    InterferenceModeArg::PublicWebStable => "public_web_stable",
                    InterferenceModeArg::Strict => "strict",
                }
            }),
            timeout,
        )),
        InterferenceSubcommand::Recover => Ok(mutating_request(
            "interference",
            serde_json::json!({ "sub": "recover" }),
            timeout,
        )),
    }
}
