use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingAuthInputMode, BindingLiveStatus, BindingPersistencePolicy, BindingRecord,
    BindingResolution, BindingResolutionMatch, BindingStatus, RememberedBindingAliasTarget,
};
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{RegistryAuthoritySnapshot, RegistryEntry, registry_authority_snapshot};
use serde_json::{Value, json};
use std::path::Path;

use super::{
    binding_alias_subject, binding_path_state, binding_registry_subject, normalize_binding_alias,
    read_binding_registry,
};

pub(crate) fn project_binding_list(rub_home: &Path) -> Result<Value, RubError> {
    let registry = read_binding_registry(rub_home)?;
    let live_snapshot = load_live_registry_snapshot(rub_home);
    let items = registry
        .bindings
        .iter()
        .map(|binding| {
            let (live_status, resolution) = project_live_status(binding, live_snapshot.as_ref());
            json!({
                "binding": binding,
                "live_status": live_status,
                "resolution": resolution,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "subject": binding_registry_subject(rub_home),
        "result": {
            "schema_version": registry.schema_version,
            "items": items,
        }
    }))
}

pub(crate) fn project_binding_inspect(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let normalized = normalize_binding_alias(alias)?;
    let registry = read_binding_registry(rub_home)?;
    let binding = registry
        .bindings
        .iter()
        .find(|binding| binding.alias == normalized)
        .cloned()
        .ok_or_else(|| binding_alias_not_found_error(rub_home, &normalized))?;
    let live_snapshot = load_live_registry_snapshot(rub_home);
    let (live_status, resolution) = project_live_status(&binding, live_snapshot.as_ref());

    Ok(json!({
        "subject": binding_alias_subject(rub_home, &normalized),
        "result": {
            "binding": binding,
            "live_status": live_status,
            "resolution": resolution,
        }
    }))
}

pub(crate) fn resolve_binding_target(
    rub_home: &Path,
    binding_alias: &str,
) -> Result<RememberedBindingAliasTarget, RubError> {
    let normalized = normalize_binding_alias(binding_alias)?;
    let registry = read_binding_registry(rub_home)?;
    let Some(binding) = registry
        .bindings
        .iter()
        .find(|binding| binding.alias == normalized)
        .cloned()
    else {
        return Ok(RememberedBindingAliasTarget::MissingBinding {
            binding_alias: normalized,
        });
    };
    let live_snapshot = load_live_registry_snapshot(rub_home);
    let (live_status, resolution) = project_live_status(&binding, live_snapshot.as_ref());
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

pub(crate) fn load_live_registry_snapshot(rub_home: &Path) -> Option<RegistryAuthoritySnapshot> {
    registry_authority_snapshot(rub_home).ok()
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
    let entries = snapshot.active_entries();

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
            binding
                .attachment_identity
                .as_ref()
                .zip(entry.attachment_identity.as_ref())
                .is_some_and(|(left, right)| left == right)
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

    if binding
        .attachment_identity
        .as_deref()
        .is_some_and(|identity| identity.starts_with("profile:"))
    {
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
