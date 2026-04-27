use crate::commands::{BindingCaptureAuthInputArg, EffectiveCli};
use crate::daemon_ctl;
use crate::session_policy::{
    materialize_connection_request_with_deadline, parse_connection_request,
    requested_attachment_identity, requires_existing_session_validation,
    resolve_attachment_identity_with_deadline,
    validate_existing_session_connection_request_via_authority_probe_with_deadline,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCaptureCandidateInfo, BindingCreatedVia,
    BindingLiveStatus, BindingRecord, BindingResolution, BindingScope,
};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{
    binding_alias_subject, load_live_registry_snapshot, mutate_binding_registry,
    normalize_binding_alias, project_live_registry_error, project_live_status,
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

    let binding = mutate_binding_registry(&cli.rub_home, |registry| {
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
        Ok(binding)
    })?;
    let (live_snapshot, live_registry_error) = match load_live_registry_snapshot(&cli.rub_home) {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(project_live_registry_error(&error))),
    };
    let (live_status, resolution) = project_live_status(&binding, live_snapshot.as_ref());
    Ok(binding_capture_projection(BindingCaptureProjectionInput {
        rub_home: &cli.rub_home,
        alias: &alias,
        mode,
        binding: &binding,
        candidate: &candidate,
        live_status: &live_status,
        resolution: &resolution,
        live_registry_error,
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
    let timeout_ms = cli.timeout.max(1);
    let started_at = Instant::now();
    let deadline = crate::timeout_budget::deadline_from_start(started_at, timeout_ms);
    let connection_request = materialize_connection_request_with_deadline(
        &parse_connection_request(cli)?,
        Some(deadline),
        Some(timeout_ms),
    )
    .await?;
    let daemon_args = crate::daemon_args(cli, &connection_request);
    let attachment_identity = authoritative_binding_capture_attachment_identity(
        cli,
        &connection_request,
        deadline,
        timeout_ms,
    )
    .await?;

    let (mut client, daemon_session_id, authority_socket_path) =
        match daemon_ctl::detect_or_connect_hardened_until(
            &cli.rub_home,
            &cli.session,
            daemon_ctl::TransientSocketPolicy::FailAfterLock,
            deadline,
            timeout_ms,
        )
        .await?
        {
            daemon_ctl::DaemonConnection::Connected {
                client,
                daemon_session_id,
                authority_socket_path,
            } => (client, daemon_session_id, authority_socket_path),
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
        validate_existing_session_connection_request_via_authority_probe_with_deadline(
            cli,
            &connection_request,
            authority_socket_path.as_path(),
            cli.session_id.as_deref().or(daemon_session_id.as_deref()),
            deadline,
            timeout_ms,
        )
        .await?;
    }

    let request = binding_capture_candidate_request(timeout_ms);
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

async fn authoritative_binding_capture_attachment_identity(
    cli: &EffectiveCli,
    request: &crate::session_policy::ConnectionRequest,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<Option<String>, RubError> {
    match request {
        crate::session_policy::ConnectionRequest::CdpUrl { .. }
        | crate::session_policy::ConnectionRequest::AutoDiscover => {
            resolve_attachment_identity_with_deadline(
                cli,
                request,
                None,
                deadline,
                timeout_ms,
                "binding_capture_attachment_identity_resolution",
            )
            .await
        }
        _ => Ok(requested_attachment_identity(cli, request)),
    }
}

pub(super) fn binding_capture_candidate_request(timeout_ms: u64) -> IpcRequest {
    IpcRequest::new(
        "runtime",
        json!({ "sub": "binding-capture-candidate" }),
        timeout_ms,
    )
    .with_command_id(Uuid::now_v7().to_string())
    .expect("UUID command_id must be valid")
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
                    created_via: match auth_input {
                        BindingCaptureAuthInputArg::Cli => BindingCreatedVia::CliAuthCompleted,
                        BindingCaptureAuthInputArg::Mixed => BindingCreatedVia::MixedAuthCompleted,
                    },
                    auth_input_mode: BindingAuthInputMode::Unknown,
                    capture_fence: Some(match auth_input {
                        BindingCaptureAuthInputArg::Cli => "explicit_cli_auth_capture".to_string(),
                        BindingCaptureAuthInputArg::Mixed => {
                            "explicit_mixed_auth_capture".to_string()
                        }
                    }),
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

struct BindingCaptureProjectionInput<'a> {
    rub_home: &'a Path,
    alias: &'a str,
    mode: BindingWriteMode,
    binding: &'a BindingRecord,
    candidate: &'a BindingCaptureCandidateInfo,
    live_status: &'a BindingLiveStatus,
    resolution: &'a BindingResolution,
    live_registry_error: Option<Value>,
}

fn binding_capture_projection(input: BindingCaptureProjectionInput<'_>) -> Value {
    let mut projection = json!({
        "subject": binding_alias_subject(input.rub_home, input.alias),
        "result": {
            "mode": match input.mode {
                BindingWriteMode::Capture { .. } => "capture",
                BindingWriteMode::BindCurrent => "bind_current",
            },
            "binding": input.binding,
            "live_status": input.live_status,
            "resolution": input.resolution,
            "capture_candidate": input.candidate,
        }
    });
    if let Some(error) = input.live_registry_error {
        projection["result"]["live_registry_error"] = error;
    }
    projection
}

#[cfg(test)]
mod tests {
    use super::{
        BindingCaptureProjectionInput, BindingWriteMode,
        authoritative_binding_capture_attachment_identity, binding_capture_projection,
        build_binding_record_from_candidate,
    };
    use crate::commands::{Commands, EffectiveCli, RequestedLaunchPolicy};
    use crate::session_policy::ConnectionRequest;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        AuthState, BindingAuthInputMode, BindingAuthProvenance, BindingCaptureAttachmentInfo,
        BindingCaptureAuthEvidence, BindingCaptureCandidateInfo, BindingCaptureDiagnostics,
        BindingCaptureDurabilityInfo, BindingCaptureFenceInfo, BindingCaptureFenceStatus,
        BindingCaptureLiveCorrelation, BindingCaptureSessionInfo, BindingCreatedVia,
        BindingDurabilityScope, BindingPersistencePolicy, BindingReattachmentMode,
        BindingResolution, BindingSessionReference, BindingSessionReferenceKind, BindingStatus,
        StateInspectorStatus,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn cli_with(command: Commands) -> EffectiveCli {
        EffectiveCli {
            session: "default".to_string(),
            session_id: None,
            rub_home: PathBuf::from("/tmp/rub-test"),
            timeout: 30_000,
            headed: false,
            ignore_cert_errors: false,
            user_data_dir: None,
            hide_infobars: true,
            json_pretty: false,
            verbose: false,
            trace: false,
            command,
            cdp_url: None,
            connect: false,
            profile: None,
            profile_resolved_path: None,
            use_alias: None,
            no_stealth: false,
            humanize: false,
            humanize_speed: "normal".to_string(),
            requested_launch_policy: RequestedLaunchPolicy::default(),
            effective_launch_policy: RequestedLaunchPolicy::default(),
        }
    }

    #[tokio::test]
    async fn binding_capture_attachment_identity_resolution_uses_remaining_deadline() {
        let cli = cli_with(Commands::Doctor);
        let error = authoritative_binding_capture_attachment_identity(
            &cli,
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222/json/version".to_string(),
            },
            Instant::now() - Duration::from_millis(1),
            1_500,
        )
        .await
        .expect_err("expired binding capture budget must fail before live identity resolution");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(|value| value.as_str()),
            Some("binding_capture_attachment_identity_resolution")
        );
    }

    #[test]
    fn binding_capture_projection_surfaces_live_registry_error_metadata() {
        let candidate = BindingCaptureCandidateInfo {
            session: BindingCaptureSessionInfo {
                session_id: "sess-default".to_string(),
                session_name: "default".to_string(),
                rub_home_reference: "/tmp/rub-test".to_string(),
                rub_home_temp_owned: false,
            },
            attachment: BindingCaptureAttachmentInfo {
                attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
                connection_target: None,
                profile_directory_reference: Some("/tmp/work/Profile 3".to_string()),
                user_data_dir_reference: Some("/tmp/work".to_string()),
            },
            capture_fence: BindingCaptureFenceInfo {
                status: BindingCaptureFenceStatus::CaptureReady,
                capture_eligible: true,
                bind_current_eligible: true,
                capture_fence: None,
                status_reason: None,
            },
            auth_evidence: BindingCaptureAuthEvidence {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Authenticated,
                cookie_count: 3,
                auth_signals: vec!["cookie".to_string()],
                degraded_reason: None,
            },
            durability: BindingCaptureDurabilityInfo {
                persistence_policy: BindingPersistencePolicy::RubHomeLocalDurable,
                durability_scope: BindingDurabilityScope::RubHomeLocalDurable,
                reattachment_mode: BindingReattachmentMode::ManagedReacquirable,
                status_reason: None,
            },
            live_correlation: BindingCaptureLiveCorrelation {
                session_reference: BindingSessionReference {
                    kind: BindingSessionReferenceKind::LiveSessionHint,
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                },
                attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
            },
            auth_provenance_hint: BindingAuthProvenance {
                created_via: BindingCreatedVia::BoundExistingRuntime,
                auth_input_mode: BindingAuthInputMode::Unknown,
                capture_fence: None,
                captured_from_session: Some("default".to_string()),
                captured_from_attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
            },
            diagnostics: BindingCaptureDiagnostics::default(),
        };
        let binding = build_binding_record_from_candidate(
            "finance",
            &candidate,
            BindingWriteMode::BindCurrent,
        );
        let projection = binding_capture_projection(BindingCaptureProjectionInput {
            rub_home: &PathBuf::from("/tmp/rub-test"),
            alias: "finance",
            mode: BindingWriteMode::BindCurrent,
            binding: &binding,
            candidate: &candidate,
            live_status: &rub_core::model::BindingLiveStatus {
                status: BindingStatus::LiveStatusUnavailable,
                status_reason: Some("live_registry_unavailable".to_string()),
                live_session_present: false,
                runtime_refresh_required: true,
                human_refresh_available: true,
                verification_required: true,
                durability_scope: BindingDurabilityScope::RubHomeLocalDurable,
                reattachment_mode: BindingReattachmentMode::ManagedReacquirable,
            },
            resolution: &BindingResolution::LiveStatusUnavailable,
            live_registry_error: Some(json!({
                "code": "DAEMON_START_FAILED",
                "message": "Failed to resolve registry authority",
            })),
        });
        assert_eq!(
            projection["result"]["live_registry_error"]["code"],
            "DAEMON_START_FAILED"
        );
    }
}
