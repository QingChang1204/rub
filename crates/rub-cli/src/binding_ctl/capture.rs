use crate::commands::{BindingCaptureAuthInputArg, EffectiveCli};
use crate::daemon_ctl;
use crate::session_policy::{
    materialize_connection_request, parse_connection_request, requested_attachment_identity,
    requires_existing_session_validation, validate_existing_session_connection_request,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCaptureCandidateInfo, BindingCreatedVia,
    BindingRecord, BindingScope,
};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{
    binding_alias_subject, load_live_registry_snapshot, normalize_binding_alias,
    project_live_status, read_binding_registry, write_binding_registry,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingWriteMode {
    Capture {
        auth_input: Option<BindingCaptureAuthInputArg>,
    },
    BindCurrent,
}

pub(crate) async fn capture_binding_alias(
    cli: &EffectiveCli,
    alias: &str,
    mode: BindingWriteMode,
) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let candidate = fetch_binding_capture_candidate(cli).await?;
    validate_capture_mode(cli, &alias, &candidate, mode)?;

    let mut registry = read_binding_registry(&cli.rub_home)?;
    if registry
        .bindings
        .iter()
        .any(|binding| binding.alias == alias)
    {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Binding alias already exists: {alias}"),
            json!({
                "alias": alias,
                "reason": "binding_alias_already_exists",
            }),
        ));
    }

    let binding = build_binding_record_from_candidate(&alias, &candidate, mode);
    registry.bindings.push(binding.clone());
    write_binding_registry(&cli.rub_home, &registry)?;
    let live_snapshot = load_live_registry_snapshot(&cli.rub_home);
    let (live_status, resolution) = project_live_status(&binding, live_snapshot.as_ref());

    Ok(json!({
        "subject": binding_alias_subject(&cli.rub_home, &alias),
        "result": {
            "mode": match mode {
                BindingWriteMode::Capture { .. } => "capture",
                BindingWriteMode::BindCurrent => "bind_current",
            },
            "binding": binding,
            "live_status": live_status,
            "resolution": resolution,
            "capture_candidate": candidate,
        }
    }))
}

fn validate_capture_mode(
    cli: &EffectiveCli,
    alias: &str,
    candidate: &BindingCaptureCandidateInfo,
    mode: BindingWriteMode,
) -> Result<(), RubError> {
    match mode {
        BindingWriteMode::Capture { auth_input } => match auth_input {
            Some(BindingCaptureAuthInputArg::Cli)
                if candidate.capture_fence.capture_eligible
                    && matches!(
                        candidate.auth_provenance_hint.auth_input_mode,
                        BindingAuthInputMode::Human
                    ) =>
            {
                Err(binding_capture_unavailable_error(
                    &cli.rub_home,
                    alias,
                    candidate,
                    "binding_capture_cli_auth_conflicts_with_human_control_fence",
                ))
            }
            Some(_) if !candidate.capture_fence.bind_current_eligible => {
                Err(binding_capture_unavailable_error(
                    &cli.rub_home,
                    alias,
                    candidate,
                    "binding_capture_unavailable_for_active_human_control",
                ))
            }
            None if !candidate.capture_fence.capture_eligible => {
                Err(binding_capture_unavailable_error(
                    &cli.rub_home,
                    alias,
                    candidate,
                    "binding_capture_requires_explicit_auth_completion_fence",
                ))
            }
            _ => Ok(()),
        },
        BindingWriteMode::BindCurrent if !candidate.capture_fence.bind_current_eligible => {
            Err(binding_capture_unavailable_error(
                &cli.rub_home,
                alias,
                candidate,
                "binding_bind_current_unavailable_for_active_human_control",
            ))
        }
        _ => Ok(()),
    }
}

async fn fetch_binding_capture_candidate(
    cli: &EffectiveCli,
) -> Result<BindingCaptureCandidateInfo, RubError> {
    let connection_request =
        materialize_connection_request(&parse_connection_request(cli)?).await?;
    let daemon_args = crate::daemon_args(cli, &connection_request);
    let attachment_identity = requested_attachment_identity(cli, &connection_request);

    let (mut client, daemon_session_id) = match daemon_ctl::detect_or_connect_hardened(
        &cli.rub_home,
        &cli.session,
        daemon_ctl::TransientSocketPolicy::FailAfterLock,
    )
    .await?
    {
        daemon_ctl::DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => (client, daemon_session_id),
        daemon_ctl::DaemonConnection::NeedStart => {
            return Err(RubError::domain_with_context(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Binding capture requires an existing session authority for '{}'",
                    cli.session
                ),
                json!({
                    "session": cli.session,
                    "rub_home": cli.rub_home.display().to_string(),
                    "reason": "binding_capture_requires_existing_session",
                }),
            ));
        }
    };

    if requires_existing_session_validation(true, &connection_request, cli) {
        validate_existing_session_connection_request(cli, &connection_request).await?;
    }

    let timeout_ms = cli.timeout.max(1);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let request = IpcRequest::new(
        "runtime",
        json!({ "sub": "binding-capture-candidate" }),
        timeout_ms,
    );
    let response = daemon_ctl::send_existing_request_with_replay_recovery(
        &mut client,
        &request,
        deadline,
        &cli.rub_home,
        &cli.session,
        daemon_session_id.as_deref(),
    )
    .await
    .map_err(|error| {
        let mut envelope = error.into_envelope();
        envelope.message = format!(
            "Failed to fetch binding capture candidate for session '{}': {}",
            cli.session, envelope.message
        );
        if let Some(context) = envelope
            .context
            .as_mut()
            .and_then(|value| value.as_object_mut())
        {
            context.insert("daemon_args".to_string(), json!(daemon_args));
            context.insert(
                "attachment_identity".to_string(),
                json!(attachment_identity),
            );
            context.insert(
                "daemon_session_id".to_string(),
                json!(daemon_session_id.as_deref()),
            );
        }
        RubError::Domain(envelope)
    })?;

    match response.status {
        ResponseStatus::Success => {
            let data = response.data.ok_or_else(|| {
                RubError::domain(
                    ErrorCode::IpcProtocolError,
                    "missing binding capture candidate payload in success response",
                )
            })?;
            let runtime = data.get("runtime").cloned().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::IpcProtocolError,
                    "binding capture candidate response missing runtime projection",
                )
            })?;
            serde_json::from_value(runtime).map_err(RubError::from)
        }
        ResponseStatus::Error => Err(response.error.map(RubError::Domain).unwrap_or_else(|| {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                "missing error envelope in binding capture candidate response",
            )
        })),
    }
}

fn binding_capture_unavailable_error(
    rub_home: &Path,
    alias: &str,
    candidate: &BindingCaptureCandidateInfo,
    reason: &str,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Binding '{alias}' cannot be captured from the current runtime"),
        json!({
            "alias": alias,
            "rub_home": rub_home.display().to_string(),
            "capture_fence": candidate.capture_fence,
            "durability": candidate.durability,
            "reason": reason,
        }),
    )
}

pub(crate) fn build_binding_record_from_candidate(
    alias: &str,
    candidate: &BindingCaptureCandidateInfo,
    mode: BindingWriteMode,
) -> BindingRecord {
    let now = rfc3339_now();
    let auth_provenance = build_binding_auth_provenance(candidate, mode);

    BindingRecord {
        alias: alias.to_string(),
        scope: BindingScope::RubHomeLocal,
        rub_home_reference: candidate.session.rub_home_reference.clone(),
        session_reference: Some(candidate.live_correlation.session_reference.clone()),
        attachment_identity: candidate.attachment.attachment_identity.clone(),
        profile_directory_reference: candidate.attachment.profile_directory_reference.clone(),
        user_data_dir_reference: candidate.attachment.user_data_dir_reference.clone(),
        auth_provenance,
        persistence_policy: candidate.durability.persistence_policy,
        created_at: now.clone(),
        last_captured_at: now,
    }
}

fn build_binding_auth_provenance(
    candidate: &BindingCaptureCandidateInfo,
    mode: BindingWriteMode,
) -> BindingAuthProvenance {
    match mode {
        BindingWriteMode::Capture { auth_input: None } => candidate.auth_provenance_hint.clone(),
        BindingWriteMode::Capture {
            auth_input: Some(auth_input),
        } => {
            let mut provenance = if candidate.capture_fence.capture_eligible {
                candidate.auth_provenance_hint.clone()
            } else {
                BindingAuthProvenance {
                    created_via: BindingCreatedVia::BoundExistingRuntime,
                    auth_input_mode: BindingAuthInputMode::Unknown,
                    capture_fence: None,
                    captured_from_session: candidate
                        .auth_provenance_hint
                        .captured_from_session
                        .clone(),
                    captured_from_attachment_identity: candidate
                        .auth_provenance_hint
                        .captured_from_attachment_identity
                        .clone(),
                }
            };
            provenance.auth_input_mode = match auth_input {
                BindingCaptureAuthInputArg::Cli => BindingAuthInputMode::Cli,
                BindingCaptureAuthInputArg::Mixed => BindingAuthInputMode::Mixed,
            };
            provenance
        }
        BindingWriteMode::BindCurrent => BindingAuthProvenance {
            created_via: BindingCreatedVia::BoundExistingRuntime,
            auth_input_mode: BindingAuthInputMode::Unknown,
            capture_fence: None,
            captured_from_session: candidate.auth_provenance_hint.captured_from_session.clone(),
            captured_from_attachment_identity: candidate
                .auth_provenance_hint
                .captured_from_attachment_identity
                .clone(),
        },
    }
}

fn rfc3339_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}
