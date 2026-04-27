use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingAuthInputMode, BindingLiveStatus, BindingPersistencePolicy, BindingRecord,
    BindingRegistryData, BindingResolution, BindingResolutionMatch, BindingStatus,
    RememberedBindingAliasTarget,
};
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{RegistryAuthoritySnapshot, RegistryEntry, RegistryEntryLiveness};
use serde_json::{Value, json};
use std::path::Path;

use super::{
    binding_alias_subject, binding_path_state, binding_registry_subject, normalize_binding_alias,
    read_binding_registry,
};

pub(crate) fn project_binding_list(rub_home: &Path) -> Result<Value, RubError> {
    let state = load_binding_resolution_state(rub_home)?;
    let items = state
        .registry
        .bindings
        .iter()
        .map(|binding| {
            let (live_status, resolution) = project_live_status(binding, state.live_snapshot());
            json!({
                "binding": binding,
                "live_status": live_status,
                "resolution": resolution,
            })
        })
        .collect::<Vec<_>>();

    let mut projection = json!({
        "subject": binding_registry_subject(rub_home),
        "result": {
            "schema_version": state.registry.schema_version,
            "items": items,
        }
    });
    if let Some(error) = state.live_registry_error_value() {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn project_binding_inspect(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let normalized = normalize_binding_alias(alias)?;
    let state = load_binding_resolution_state(rub_home)?;
    let binding = state
        .registry
        .bindings
        .iter()
        .find(|binding| binding.alias == normalized)
        .cloned()
        .ok_or_else(|| binding_alias_not_found_error(rub_home, &normalized))?;
    let (live_status, resolution) = project_live_status(&binding, state.live_snapshot());

    let mut projection = json!({
        "subject": binding_alias_subject(rub_home, &normalized),
        "result": {
            "binding": binding,
            "live_status": live_status,
            "resolution": resolution,
        }
    });
    if let Some(error) = state.live_registry_error_value() {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn load_binding_resolution_state(
    rub_home: &Path,
) -> Result<BindingResolutionState, RubError> {
    let registry = read_binding_registry(rub_home)?;
    load_binding_resolution_state_from_registry(rub_home, registry)
}

pub(crate) fn load_binding_resolution_state_from_registry(
    rub_home: &Path,
    registry: BindingRegistryData,
) -> Result<BindingResolutionState, RubError> {
    let (live_snapshot, live_snapshot_error) = match load_live_registry_snapshot(rub_home) {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(error)),
    };
    Ok(BindingResolutionState {
        registry,
        live_snapshot,
        live_snapshot_error,
    })
}

pub(crate) fn resolve_binding_target_from_state(
    binding_alias: &str,
    state: &BindingResolutionState,
) -> Result<RememberedBindingAliasTarget, RubError> {
    let normalized = normalize_binding_alias(binding_alias)?;
    let Some(binding) = state
        .registry
        .bindings
        .iter()
        .find(|binding| binding.alias == normalized)
        .cloned()
    else {
        return Ok(RememberedBindingAliasTarget::MissingBinding {
            binding_alias: normalized,
        });
    };
    let (live_status, resolution) = project_live_status(&binding, state.live_snapshot());
    Ok(RememberedBindingAliasTarget::Resolved {
        binding_alias: normalized,
        binding: Box::new(binding),
        live_status,
        resolution,
    })
}

pub(crate) fn binding_alias_not_found_error(rub_home: &Path, alias: &str) -> RubError {
    let paths = RubPaths::new(rub_home);
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Binding alias not found: {alias}"),
        json!({
            "alias": alias,
            "registry_path": paths.bindings_path().display().to_string(),
            "registry_path_state": binding_path_state(
                "cli.binding.subject.registry_path",
                "cli_binding_registry",
                "binding_registry_file",
            ),
            "reason": "binding_alias_not_found",
        }),
    )
}

pub(crate) fn load_live_registry_snapshot(
    rub_home: &Path,
) -> Result<RegistryAuthoritySnapshot, RubError> {
    crate::daemon_ctl::registry_authority_snapshot(rub_home)
}

pub(crate) struct BindingResolutionState {
    registry: BindingRegistryData,
    live_snapshot: Option<RegistryAuthoritySnapshot>,
    live_snapshot_error: Option<RubError>,
}

impl BindingResolutionState {
    pub(crate) fn live_snapshot(&self) -> Option<&RegistryAuthoritySnapshot> {
        self.live_snapshot.as_ref()
    }

    pub(crate) fn live_snapshot_error(&self) -> Option<&RubError> {
        self.live_snapshot_error.as_ref()
    }

    pub(crate) fn live_registry_error_value(&self) -> Option<Value> {
        self.live_snapshot_error
            .as_ref()
            .map(project_live_registry_error)
    }
}

pub(crate) fn project_live_registry_error(error: &RubError) -> Value {
    match error {
        RubError::Domain(envelope) => json!({
            "code": envelope.code,
            "message": envelope.message,
            "context": envelope.context,
            "suggestion": envelope.suggestion,
        }),
        RubError::Io(io_error) => json!({
            "code": ErrorCode::IoError,
            "message": io_error.to_string(),
            "context": Value::Null,
            "suggestion": ErrorCode::IoError.suggestion(),
        }),
        RubError::Json(json_error) => json!({
            "code": ErrorCode::JsonError,
            "message": json_error.to_string(),
            "context": Value::Null,
            "suggestion": ErrorCode::JsonError.suggestion(),
        }),
        RubError::Internal(message) => json!({
            "code": ErrorCode::InternalError,
            "message": message,
            "context": Value::Null,
            "suggestion": ErrorCode::InternalError.suggestion(),
        }),
    }
}

pub(crate) fn project_live_status(
    binding: &BindingRecord,
    live_snapshot: Option<&RegistryAuthoritySnapshot>,
) -> (BindingLiveStatus, BindingResolution) {
    let (durability_scope, reattachment_mode) =
        binding.persistence_policy.durability_and_reattachment();
    let human_refresh_available = matches!(
        binding.auth_provenance.auth_input_mode,
        BindingAuthInputMode::Human | BindingAuthInputMode::Mixed | BindingAuthInputMode::Unknown
    );

    let Some(snapshot) = live_snapshot else {
        return (
            BindingLiveStatus {
                status: BindingStatus::LiveStatusUnavailable,
                status_reason: Some("live_registry_unavailable".to_string()),
                live_session_present: false,
                runtime_refresh_required: true,
                human_refresh_available,
                verification_required: true,
                durability_scope,
                reattachment_mode,
            },
            BindingResolution::LiveStatusUnavailable,
        );
    };

    let matches = find_live_matches(binding, snapshot);
    if matches.len() == 1 {
        let matched = &matches[0];
        return (
            BindingLiveStatus {
                status: BindingStatus::LiveSessionPresent,
                status_reason: Some(format!("live_session_matched_by_{}", matched.matched_by)),
                live_session_present: true,
                runtime_refresh_required: false,
                human_refresh_available,
                verification_required: false,
                durability_scope,
                reattachment_mode,
            },
            BindingResolution::LiveMatch {
                matched_by: matched.matched_by.to_string(),
                session_id: matched.entry.session_id.clone(),
                session_name: matched.entry.session_name.clone(),
            },
        );
    }

    if matches.len() > 1 {
        return (
            BindingLiveStatus {
                status: BindingStatus::VerificationRequired,
                status_reason: Some("multiple_live_sessions_match_binding".to_string()),
                live_session_present: false,
                runtime_refresh_required: true,
                human_refresh_available,
                verification_required: true,
                durability_scope,
                reattachment_mode,
            },
            BindingResolution::AmbiguousLiveMatch {
                matches: matches
                    .into_iter()
                    .map(|matched| BindingResolutionMatch {
                        matched_by: matched.matched_by.to_string(),
                        session_id: matched.entry.session_id,
                        session_name: matched.entry.session_name,
                    })
                    .collect(),
            },
        );
    }

    let (status, reason) = match binding.persistence_policy {
        BindingPersistencePolicy::ExternalReattachmentRequired => (
            BindingStatus::ExternalReattachmentRequired,
            "external_attachment_requires_reattachment",
        ),
        BindingPersistencePolicy::RubHomeLocalEphemeral => (
            BindingStatus::EphemeralBinding,
            "temp_home_binding_is_ephemeral",
        ),
        BindingPersistencePolicy::RubHomeLocalDurable => (
            BindingStatus::VerificationRequired,
            "durable_binding_requires_verification",
        ),
    };

    (
        BindingLiveStatus {
            status,
            status_reason: Some(reason.to_string()),
            live_session_present: false,
            runtime_refresh_required: true,
            human_refresh_available,
            verification_required: true,
            durability_scope,
            reattachment_mode,
        },
        BindingResolution::NoLiveMatch,
    )
}

struct LiveBindingMatch {
    matched_by: &'static str,
    entry: RegistryEntry,
}

fn find_live_matches(
    binding: &BindingRecord,
    snapshot: &RegistryAuthoritySnapshot,
) -> Vec<LiveBindingMatch> {
    let entries = snapshot
        .active_entry_snapshots()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.liveness,
                RegistryEntryLiveness::Live
                    | RegistryEntryLiveness::BusyOrUnknown
                    | RegistryEntryLiveness::ProbeContractFailure
            )
        })
        .map(|entry| entry.entry)
        .collect::<Vec<_>>();
    let attachment_identity_authority = binding.attachment_identity.as_ref().or(binding
        .auth_provenance
        .captured_from_attachment_identity
        .as_ref());

    let exact_session_matches = entries
        .iter()
        .filter(|entry| {
            binding.session_reference.as_ref().is_some_and(|reference| {
                entry.session_id == reference.session_id
                    && entry.session_name == reference.session_name
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if !exact_session_matches.is_empty() {
        return exact_session_matches
            .into_iter()
            .map(|entry| LiveBindingMatch {
                matched_by: "session_reference",
                entry,
            })
            .collect();
    }

    let attachment_matches = entries
        .iter()
        .filter(|entry| {
            attachment_identity_authority
                .as_ref()
                .zip(entry.attachment_identity.as_ref())
                .is_some_and(|(left, right)| left.as_str() == right.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    if !attachment_matches.is_empty() {
        return attachment_matches
            .into_iter()
            .map(|entry| LiveBindingMatch {
                matched_by: "attachment_identity",
                entry,
            })
            .collect();
    }

    if attachment_identity_authority.is_some_and(|identity| identity.starts_with("profile:")) {
        return Vec::new();
    }

    entries
        .into_iter()
        .filter(|entry| {
            binding
                .user_data_dir_reference
                .as_ref()
                .zip(entry.user_data_dir.as_ref())
                .is_some_and(|(left, right)| left == right)
        })
        .map(|entry| LiveBindingMatch {
            matched_by: "user_data_dir_reference",
            entry,
        })
        .collect()
}
