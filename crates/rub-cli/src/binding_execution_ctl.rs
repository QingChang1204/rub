use crate::binding_memory_ctl::resolve_remembered_alias_target;
use crate::commands::EffectiveCli;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingExecutionMode, BindingExecutionResolutionInfo, BindingExecutionSourceKind,
    BindingLiveStatus, BindingRecord, BindingRefreshPath, BindingResolution,
    RememberedBindingAliasRecord, RememberedBindingAliasTarget,
};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug)]
pub(crate) struct BindingExecutionContext {
    pub(crate) cli: EffectiveCli,
    pub(crate) projection: Option<BindingExecutionResolutionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindingLaunchTarget {
    Profile { dir_name: String },
    UserDataDir { path: String },
}

pub(crate) fn resolve_command_execution_binding(
    cli: &EffectiveCli,
) -> Result<BindingExecutionContext, RubError> {
    let Some(alias) = cli.use_alias.as_deref() else {
        return Ok(BindingExecutionContext {
            cli: cli.clone(),
            projection: None,
        });
    };
    if cli.cdp_url.is_some()
        || cli.connect
        || cli.profile.is_some()
        || cli.requested_launch_policy.user_data_dir.is_some()
    {
        return Err(RubError::domain_with_context(
            ErrorCode::ConflictingConnectOptions,
            "Use either --use or an explicit browser attachment selector, not both",
            json!({
                "alias": alias,
                "reason": "binding_execution_conflicting_connect_options",
            }),
        ));
    }

    let (remembered_alias, target) = resolve_remembered_alias_target(&cli.rub_home, alias)?;
    let RememberedBindingAliasTarget::Resolved {
        binding_alias,
        binding,
        live_status,
        resolution,
    } = target
    else {
        return Err(remembered_alias_execution_error(
            &cli.rub_home,
            &remembered_alias,
            None,
            None,
            None,
            "remembered_alias_target_binding_missing",
            format!(
                "Remembered alias '{}' points at a binding that no longer exists",
                remembered_alias.alias
            ),
        ));
    };

    let binding = *binding;
    match &resolution {
        BindingResolution::LiveMatch {
            session_name,
            session_id,
            ..
        } => {
            let mut resolved_cli = cli.clone();
            resolved_cli.session = session_name.clone();
            resolved_cli.session_id = Some(session_id.clone());
            resolved_cli.use_alias = None;
            resolved_cli.user_data_dir = None;
            resolved_cli.requested_launch_policy.user_data_dir = None;
            resolved_cli.effective_launch_policy.user_data_dir = None;

            Ok(BindingExecutionContext {
                projection: Some(BindingExecutionResolutionInfo {
                    source_kind: BindingExecutionSourceKind::RememberedAlias,
                    requested_alias: remembered_alias.alias,
                    remembered_alias_kind: remembered_alias.kind,
                    binding_alias,
                    mode: BindingExecutionMode::ReuseLiveSession,
                    effective_session_name: session_name.clone(),
                    effective_session_id: Some(session_id.clone()),
                    effective_profile_dir_name: None,
                    effective_user_data_dir: None,
                    live_status,
                    resolution,
                    available_refresh_paths: refresh_paths_for_binding(&binding),
                }),
                cli: resolved_cli,
            })
        }
        BindingResolution::NoLiveMatch
            if matches!(
                binding.persistence_policy,
                rub_core::model::BindingPersistencePolicy::RubHomeLocalDurable
            ) && reusable_launch_target(&binding).is_some() =>
        {
            let mut resolved_cli = cli.clone();
            let launch_target = reusable_launch_target(&binding).expect("checked above");
            resolved_cli.use_alias = None;
            match launch_target {
                BindingLaunchTarget::Profile { dir_name } => {
                    resolved_cli.profile = Some(dir_name.clone());
                    resolved_cli.user_data_dir = None;
                    resolved_cli.requested_launch_policy.user_data_dir = None;
                    resolved_cli.effective_launch_policy.user_data_dir = None;

                    Ok(BindingExecutionContext {
                        projection: Some(BindingExecutionResolutionInfo {
                            source_kind: BindingExecutionSourceKind::RememberedAlias,
                            requested_alias: remembered_alias.alias,
                            remembered_alias_kind: remembered_alias.kind,
                            binding_alias,
                            mode: BindingExecutionMode::LaunchBoundProfile,
                            effective_session_name: resolved_cli.session.clone(),
                            effective_session_id: None,
                            effective_profile_dir_name: Some(dir_name),
                            effective_user_data_dir: None,
                            live_status,
                            resolution,
                            available_refresh_paths: refresh_paths_for_binding(&binding),
                        }),
                        cli: resolved_cli,
                    })
                }
                BindingLaunchTarget::UserDataDir { path } => {
                    resolved_cli.user_data_dir = Some(path.clone());
                    resolved_cli.requested_launch_policy.user_data_dir = Some(path.clone());
                    resolved_cli.effective_launch_policy.user_data_dir = Some(path.clone());

                    Ok(BindingExecutionContext {
                        projection: Some(BindingExecutionResolutionInfo {
                            source_kind: BindingExecutionSourceKind::RememberedAlias,
                            requested_alias: remembered_alias.alias,
                            remembered_alias_kind: remembered_alias.kind,
                            binding_alias,
                            mode: BindingExecutionMode::LaunchBoundRuntime,
                            effective_session_name: resolved_cli.session.clone(),
                            effective_session_id: None,
                            effective_profile_dir_name: None,
                            effective_user_data_dir: Some(path),
                            live_status,
                            resolution,
                            available_refresh_paths: refresh_paths_for_binding(&binding),
                        }),
                        cli: resolved_cli,
                    })
                }
            }
        }
        _ => Err(remembered_alias_execution_error(
            &cli.rub_home,
            &remembered_alias,
            Some(&binding),
            Some(&live_status),
            Some(&resolution),
            execution_unavailable_reason(&live_status, &resolution, &binding),
            execution_unavailable_message(&remembered_alias, &live_status, &resolution),
        )),
    }
}

pub(crate) fn attach_binding_execution_projection(
    data: &mut Option<Value>,
    projection: &BindingExecutionResolutionInfo,
) {
    let Ok(projection_value) = serde_json::to_value(projection) else {
        return;
    };
    let Some(Value::Object(object)) = data.as_mut() else {
        return;
    };
    object.insert("binding_resolution".to_string(), projection_value);
}

fn reusable_launch_target(binding: &BindingRecord) -> Option<BindingLaunchTarget> {
    let is_profile_binding = binding
        .attachment_identity
        .as_deref()
        .is_some_and(|identity| identity.starts_with("profile:"));
    if is_profile_binding {
        return reusable_profile_dir_name(binding)
            .map(|dir_name| BindingLaunchTarget::Profile { dir_name });
    }
    binding
        .user_data_dir_reference
        .clone()
        .map(|path| BindingLaunchTarget::UserDataDir { path })
}

fn reusable_profile_dir_name(binding: &BindingRecord) -> Option<String> {
    let is_profile_binding = binding
        .attachment_identity
        .as_deref()
        .is_some_and(|identity| identity.starts_with("profile:"));
    if !is_profile_binding {
        return None;
    }
    let reference = binding.profile_directory_reference.as_deref()?;
    Path::new(reference)
        .file_name()
        .or_else(|| {
            Path::new(reference)
                .components()
                .next_back()
                .and_then(|component| match component {
                    std::path::Component::Normal(value) => Some(value),
                    _ => None,
                })
        })
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

fn refresh_paths_for_binding(binding: &BindingRecord) -> Vec<BindingRefreshPath> {
    match binding.auth_provenance.auth_input_mode {
        rub_core::model::BindingAuthInputMode::Human => vec![BindingRefreshPath::Human],
        rub_core::model::BindingAuthInputMode::Cli => vec![BindingRefreshPath::Cli],
        rub_core::model::BindingAuthInputMode::Mixed => vec![
            BindingRefreshPath::Human,
            BindingRefreshPath::Cli,
            BindingRefreshPath::Mixed,
        ],
        rub_core::model::BindingAuthInputMode::Unknown => vec![
            BindingRefreshPath::Human,
            BindingRefreshPath::Cli,
            BindingRefreshPath::Mixed,
        ],
    }
}

fn execution_unavailable_reason(
    live_status: &BindingLiveStatus,
    resolution: &BindingResolution,
    binding: &BindingRecord,
) -> &'static str {
    match resolution {
        BindingResolution::AmbiguousLiveMatch { .. } => {
            "remembered_alias_resolves_to_multiple_live_sessions"
        }
        BindingResolution::LiveStatusUnavailable => "remembered_alias_live_status_unavailable",
        BindingResolution::NoLiveMatch => {
            if reusable_launch_target(binding).is_none() {
                "remembered_alias_has_no_reusable_launch_target"
            } else {
                match live_status.status {
                    rub_core::model::BindingStatus::ExternalReattachmentRequired => {
                        "remembered_alias_requires_external_reattachment"
                    }
                    rub_core::model::BindingStatus::EphemeralBinding => {
                        "remembered_alias_target_is_ephemeral"
                    }
                    _ => "remembered_alias_requires_refresh_before_reuse",
                }
            }
        }
        BindingResolution::LiveMatch { .. } => "remembered_alias_execution_unavailable",
    }
}

fn execution_unavailable_message(
    remembered_alias: &RememberedBindingAliasRecord,
    live_status: &BindingLiveStatus,
    resolution: &BindingResolution,
) -> String {
    match resolution {
        BindingResolution::AmbiguousLiveMatch { .. } => format!(
            "Remembered alias '{}' currently matches multiple live sessions; inspect or rebind it before reuse",
            remembered_alias.alias
        ),
        BindingResolution::LiveStatusUnavailable => format!(
            "Remembered alias '{}' cannot be reused because live binding status is unavailable right now",
            remembered_alias.alias
        ),
        BindingResolution::NoLiveMatch => match live_status.status {
            rub_core::model::BindingStatus::ExternalReattachmentRequired => format!(
                "Remembered alias '{}' needs the original external browser attachment to be reattached before reuse",
                remembered_alias.alias
            ),
            rub_core::model::BindingStatus::EphemeralBinding => format!(
                "Remembered alias '{}' points at an ephemeral temp-home binding and cannot be silently reused",
                remembered_alias.alias
            ),
            _ => format!(
                "Remembered alias '{}' needs an explicit refresh or durable runtime relaunch before reuse",
                remembered_alias.alias
            ),
        },
        BindingResolution::LiveMatch { .. } => format!(
            "Remembered alias '{}' is not reusable from the current runtime state",
            remembered_alias.alias
        ),
    }
}

fn remembered_alias_execution_error(
    rub_home: &Path,
    remembered_alias: &RememberedBindingAliasRecord,
    binding: Option<&BindingRecord>,
    live_status: Option<&BindingLiveStatus>,
    resolution: Option<&BindingResolution>,
    reason: &str,
    message: String,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        message,
        json!({
            "remembered_alias": remembered_alias,
            "binding": binding,
            "live_status": live_status,
            "resolution": resolution,
            "refresh_paths": binding.map(refresh_paths_for_binding),
            "rub_home": rub_home.display().to_string(),
            "reason": reason,
        }),
    )
}

#[cfg(test)]
mod tests;
