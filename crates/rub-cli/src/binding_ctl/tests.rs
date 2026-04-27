use super::capture::binding_capture_candidate_request;
use super::{
    BindingWriteMode, build_binding_record_from_candidate, normalize_binding_alias,
    project_binding_inspect, project_binding_list, project_live_status, read_binding_registry,
    remove_binding_alias, rename_binding_alias, write_binding_registry,
};
use crate::binding_memory_ctl::remember_binding_alias;
use crate::commands::BindingCaptureAuthInputArg;
use crate::commands::RememberedBindingAliasKindArg;
use rub_core::error::ErrorCode;
use rub_core::model::{
    AuthState, BindingAuthInputMode, BindingAuthProvenance, BindingCaptureAttachmentInfo,
    BindingCaptureAuthEvidence, BindingCaptureCandidateInfo, BindingCaptureDiagnostics,
    BindingCaptureDurabilityInfo, BindingCaptureFenceInfo, BindingCaptureFenceStatus,
    BindingCaptureLiveCorrelation, BindingCaptureSessionInfo, BindingCreatedVia,
    BindingDurabilityScope, BindingPersistencePolicy, BindingReattachmentMode, BindingRecord,
    BindingRegistryData, BindingResolution, BindingScope, BindingSessionReference,
    BindingSessionReferenceKind, BindingStatus, StateInspectorStatus,
};
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{
    RegistryAuthoritySnapshot, RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot,
    RegistrySessionSnapshot,
};
use std::path::{Path, PathBuf};

fn temp_home() -> PathBuf {
    std::env::temp_dir().join(format!("rub-binding-ctl-{}", uuid::Uuid::now_v7()))
}

fn sample_binding(alias: &str, home: &Path) -> BindingRecord {
    BindingRecord {
        alias: alias.to_string(),
        scope: BindingScope::RubHomeLocal,
        rub_home_reference: home.display().to_string(),
        session_reference: None,
        attachment_identity: Some(
            "profile:/Users/me/Library/Application Support/Google/Chrome".to_string(),
        ),
        profile_directory_reference: Some("Profile 3".to_string()),
        user_data_dir_reference: Some("/tmp/profile-root".to_string()),
        auth_provenance: BindingAuthProvenance {
            created_via: BindingCreatedVia::BoundExistingRuntime,
            auth_input_mode: BindingAuthInputMode::Unknown,
            capture_fence: None,
            captured_from_session: None,
            captured_from_attachment_identity: None,
        },
        persistence_policy: BindingPersistencePolicy::RubHomeLocalDurable,
        created_at: "2026-04-14T12:00:00Z".to_string(),
        last_captured_at: "2026-04-14T12:00:00Z".to_string(),
    }
}

#[test]
fn normalize_binding_alias_rejects_invalid_shapes() {
    assert_eq!(normalize_binding_alias("old_admin").unwrap(), "old_admin");
    assert!(normalize_binding_alias("../bad").is_err());
    assert!(normalize_binding_alias("bad/name").is_err());
    assert!(normalize_binding_alias("bad name").is_err());
}

#[test]
fn read_binding_registry_defaults_to_v1_empty_registry() {
    let home = temp_home();
    let registry = read_binding_registry(&home).expect("empty registry should load");
    assert_eq!(registry.schema_version, 1);
    assert!(registry.bindings.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn binding_capture_candidate_request_carries_command_id_for_replay_recovery() {
    let request = binding_capture_candidate_request(1_500);
    assert_eq!(request.command, "runtime");
    assert_eq!(request.args["sub"], "binding-capture-candidate");
    assert_eq!(request.timeout_ms, 1_500);
    assert!(
        request.command_id.is_some(),
        "binding capture candidate request must carry command_id before entering replay recovery"
    );
}

#[test]
fn write_binding_registry_sorts_aliases() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![
            sample_binding("zeta", &home),
            sample_binding("alpha", &home),
        ],
    };
    write_binding_registry(&home, &registry).expect("registry should write");
    let loaded = read_binding_registry(&home).expect("registry should reload");
    assert_eq!(loaded.bindings[0].alias, "alpha");
    assert_eq!(loaded.bindings[1].alias, "zeta");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn write_binding_registry_surfaces_rub_home_directory_durability_failure() {
    let root = std::env::temp_dir().join(format!(
        "rub-binding-ctl-dir-fence-{}",
        uuid::Uuid::now_v7()
    ));
    let home = root.join("home");
    let _ = std::fs::remove_dir_all(&root);
    crate::local_registry::force_directory_sync_failure_once_for_test(&home);

    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("alpha", &home)],
    };
    let envelope = write_binding_registry(&home, &registry)
        .expect_err("binding registry must reject unconfirmed RUB_HOME directory fence")
        .into_envelope();

    assert_eq!(envelope.code, ErrorCode::IoError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|reason| reason.as_str()),
        Some("binding_rub_home_create_failed")
    );
    assert!(
        envelope
            .message
            .contains("forced local registry directory sync failure"),
        "{}",
        envelope.message
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_binding_registry_rejects_published_only_file_commit() {
    let home = temp_home();
    let registry_path = RubPaths::new(&home).bindings_path();
    crate::local_registry::force_published_write_outcome_once_for_test(&registry_path);

    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("alpha", &home)],
    };
    let envelope = write_binding_registry(&home, &registry)
        .expect_err("binding registry must reject published-only file commit")
        .into_envelope();

    assert_eq!(envelope.code, ErrorCode::IoError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|reason| reason.as_str()),
        Some("binding_registry_write_failed")
    );
    assert!(
        envelope.message.contains("durability was not confirmed"),
        "{}",
        envelope.message
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn binding_list_projects_conservative_verification_required_status() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("old-admin", &home)],
    };
    write_binding_registry(&home, &registry).unwrap();

    let projection = project_binding_list(&home).expect("list projection");
    assert_eq!(projection["result"]["schema_version"], 1);
    assert_eq!(
        projection["result"]["items"][0]["live_status"]["status"],
        "verification_required"
    );
    assert_eq!(
        projection["result"]["items"][0]["resolution"]["kind"],
        "no_live_match"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn binding_inspect_projects_external_reattachment_requirement() {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.persistence_policy = BindingPersistencePolicy::ExternalReattachmentRequired;
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![binding],
    };
    write_binding_registry(&home, &registry).unwrap();

    let projection = project_binding_inspect(&home, "old-admin").expect("inspect projection");
    assert_eq!(
        projection["result"]["live_status"]["status"],
        "external_reattachment_required"
    );
    assert_eq!(projection["result"]["resolution"]["kind"], "no_live_match");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn binding_projections_surface_live_registry_authority_failure_metadata() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("old-admin", &home)],
    };
    write_binding_registry(&home, &registry).unwrap();
    std::fs::write(
        RubPaths::new(&home).registry_path(),
        "{ invalid live registry json",
    )
    .unwrap();

    let list_projection = project_binding_list(&home).expect("list projection");
    assert_eq!(
        list_projection["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );
    assert_eq!(
        list_projection["result"]["items"][0]["resolution"]["kind"],
        "live_status_unavailable"
    );

    let inspect_projection = project_binding_inspect(&home, "old-admin").expect("inspect");
    assert_eq!(
        inspect_projection["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );
    assert_eq!(
        inspect_projection["result"]["resolution"]["kind"],
        "live_status_unavailable"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn project_live_status_returns_typed_live_match_resolution() {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.session_reference = Some(BindingSessionReference {
        kind: BindingSessionReferenceKind::LiveSessionHint,
        session_id: "sess-1".to_string(),
        session_name: "default".to_string(),
    });
    binding.attachment_identity = Some("profile:Work".to_string());
    binding.user_data_dir_reference = Some("/Users/test/Chrome".to_string());
    let snapshot = RegistryAuthoritySnapshot {
        sessions: vec![RegistrySessionSnapshot {
            session_name: "default".to_string(),
            entries: vec![RegistryEntrySnapshot {
                entry: RegistryEntry {
                    session_id: "sess-1".to_string(),
                    session_name: "default".to_string(),
                    pid: 4242,
                    socket_path: "/tmp/rub.sock".to_string(),
                    created_at: "2026-04-14T00:00:00Z".to_string(),
                    ipc_protocol_version: "1".to_string(),
                    user_data_dir: Some("/Users/test/Chrome".to_string()),
                    attachment_identity: Some("profile:Work".to_string()),
                    connection_target: None,
                },
                liveness: RegistryEntryLiveness::Live,
                pid_live: true,
            }],
        }],
    };

    let (live_status, resolution) = project_live_status(&binding, Some(&snapshot));
    assert_eq!(live_status.status, BindingStatus::LiveSessionPresent);
    assert_eq!(
        resolution,
        BindingResolution::LiveMatch {
            matched_by: "session_reference".to_string(),
            session_id: "sess-1".to_string(),
            session_name: "default".to_string(),
        }
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn project_live_status_does_not_reuse_degraded_registry_authority() {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.session_reference = Some(BindingSessionReference {
        kind: BindingSessionReferenceKind::LiveSessionHint,
        session_id: "sess-1".to_string(),
        session_name: "default".to_string(),
    });
    binding.attachment_identity = Some("profile:Work".to_string());
    let snapshot = RegistryAuthoritySnapshot {
        sessions: vec![RegistrySessionSnapshot {
            session_name: "default".to_string(),
            entries: vec![RegistryEntrySnapshot {
                entry: RegistryEntry {
                    session_id: "sess-1".to_string(),
                    session_name: "default".to_string(),
                    pid: 4242,
                    socket_path: "/tmp/rub.sock".to_string(),
                    created_at: "2026-04-14T00:00:00Z".to_string(),
                    ipc_protocol_version: "1".to_string(),
                    user_data_dir: Some("/Users/test/Chrome".to_string()),
                    attachment_identity: Some("profile:Work".to_string()),
                    connection_target: None,
                },
                liveness: RegistryEntryLiveness::ProtocolIncompatible,
                pid_live: true,
            }],
        }],
    };

    let (live_status, resolution) = project_live_status(&binding, Some(&snapshot));

    assert_eq!(live_status.status, BindingStatus::VerificationRequired);
    assert!(!live_status.live_session_present);
    assert_eq!(resolution, BindingResolution::NoLiveMatch);
}

#[test]
fn project_live_status_does_not_fallback_to_user_data_dir_for_profile_bindings() {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.attachment_identity = Some("profile:/Users/test/Chrome/Profile 3".to_string());
    binding.profile_directory_reference = Some("/Users/test/Chrome/Profile 3".to_string());
    binding.user_data_dir_reference = Some("/Users/test/Chrome".to_string());
    let snapshot = RegistryAuthoritySnapshot {
        sessions: vec![RegistrySessionSnapshot {
            session_name: "default".to_string(),
            entries: vec![RegistryEntrySnapshot {
                entry: RegistryEntry {
                    session_id: "sess-other".to_string(),
                    session_name: "default".to_string(),
                    pid: 4242,
                    socket_path: "/tmp/rub.sock".to_string(),
                    created_at: "2026-04-14T00:00:00Z".to_string(),
                    ipc_protocol_version: "1".to_string(),
                    user_data_dir: Some("/Users/test/Chrome".to_string()),
                    attachment_identity: Some("profile:/Users/test/Chrome/Profile 7".to_string()),
                    connection_target: None,
                },
                liveness: RegistryEntryLiveness::Live,
                pid_live: true,
            }],
        }],
    };

    let (live_status, resolution) = project_live_status(&binding, Some(&snapshot));
    assert_eq!(live_status.status, BindingStatus::VerificationRequired);
    assert_eq!(resolution, BindingResolution::NoLiveMatch);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn project_live_status_uses_captured_attachment_identity_fallback_when_binding_field_is_missing() {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.attachment_identity = None;
    binding.profile_directory_reference = Some("/Users/test/Chrome/Profile 3".to_string());
    binding.user_data_dir_reference = Some("/Users/test/Chrome".to_string());
    binding.auth_provenance.captured_from_attachment_identity =
        Some("profile:/Users/test/Chrome/Profile 3".to_string());
    let snapshot = RegistryAuthoritySnapshot {
        sessions: vec![RegistrySessionSnapshot {
            session_name: "default".to_string(),
            entries: vec![RegistryEntrySnapshot {
                entry: RegistryEntry {
                    session_id: "sess-match".to_string(),
                    session_name: "default".to_string(),
                    pid: 4242,
                    socket_path: "/tmp/rub.sock".to_string(),
                    created_at: "2026-04-14T00:00:00Z".to_string(),
                    ipc_protocol_version: "1".to_string(),
                    user_data_dir: Some("/Users/test/Chrome".to_string()),
                    attachment_identity: Some("profile:/Users/test/Chrome/Profile 3".to_string()),
                    connection_target: None,
                },
                liveness: RegistryEntryLiveness::Live,
                pid_live: true,
            }],
        }],
    };

    let (live_status, resolution) = project_live_status(&binding, Some(&snapshot));
    assert_eq!(live_status.status, BindingStatus::LiveSessionPresent);
    assert_eq!(
        resolution,
        BindingResolution::LiveMatch {
            matched_by: "attachment_identity".to_string(),
            session_id: "sess-match".to_string(),
            session_name: "default".to_string(),
        }
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn project_live_status_does_not_drift_to_user_data_dir_when_only_captured_attachment_identity_exists()
 {
    let home = temp_home();
    let mut binding = sample_binding("old-admin", &home);
    binding.attachment_identity = None;
    binding.profile_directory_reference = Some("/Users/test/Chrome/Profile 3".to_string());
    binding.user_data_dir_reference = Some("/Users/test/Chrome".to_string());
    binding.auth_provenance.captured_from_attachment_identity =
        Some("profile:/Users/test/Chrome/Profile 3".to_string());
    let snapshot = RegistryAuthoritySnapshot {
        sessions: vec![RegistrySessionSnapshot {
            session_name: "default".to_string(),
            entries: vec![RegistryEntrySnapshot {
                entry: RegistryEntry {
                    session_id: "sess-other".to_string(),
                    session_name: "default".to_string(),
                    pid: 4242,
                    socket_path: "/tmp/rub.sock".to_string(),
                    created_at: "2026-04-14T00:00:00Z".to_string(),
                    ipc_protocol_version: "1".to_string(),
                    user_data_dir: Some("/Users/test/Chrome".to_string()),
                    attachment_identity: Some("profile:/Users/test/Chrome/Profile 7".to_string()),
                    connection_target: None,
                },
                liveness: RegistryEntryLiveness::Live,
                pid_live: true,
            }],
        }],
    };

    let (live_status, resolution) = project_live_status(&binding, Some(&snapshot));
    assert_eq!(live_status.status, BindingStatus::VerificationRequired);
    assert_eq!(resolution, BindingResolution::NoLiveMatch);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rename_binding_alias_rejects_duplicate_target() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![
            sample_binding("old-admin", &home),
            sample_binding("other", &home),
        ],
    };
    write_binding_registry(&home, &registry).unwrap();

    let error =
        rename_binding_alias(&home, "old-admin", "other").expect_err("duplicate alias should fail");
    assert_eq!(
        error.into_envelope().code,
        rub_core::error::ErrorCode::InvalidInput
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rename_binding_alias_rejects_referenced_binding_target() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("old-admin", &home)],
    };
    write_binding_registry(&home, &registry).unwrap();
    remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();

    let error = rename_binding_alias(&home, "old-admin", "new-admin")
        .expect_err("referenced binding rename should fail");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, rub_core::error::ErrorCode::InvalidInput);
    let context = envelope
        .context
        .expect("rename guard should include context");
    assert_eq!(
        context["reason"],
        "binding_alias_referenced_by_remembered_aliases"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remove_binding_alias_updates_registry() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("old-admin", &home)],
    };
    write_binding_registry(&home, &registry).unwrap();

    remove_binding_alias(&home, "old-admin").expect("remove should succeed");
    let loaded = read_binding_registry(&home).unwrap();
    assert!(loaded.bindings.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remove_binding_alias_rejects_referenced_binding_target() {
    let home = temp_home();
    let registry = BindingRegistryData {
        schema_version: 1,
        bindings: vec![sample_binding("old-admin", &home)],
    };
    write_binding_registry(&home, &registry).unwrap();
    remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Account,
    )
    .unwrap();

    let error = remove_binding_alias(&home, "old-admin")
        .expect_err("referenced binding remove should fail");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, rub_core::error::ErrorCode::InvalidInput);
    let context = envelope
        .context
        .expect("remove guard should include context");
    assert_eq!(
        context["reason"],
        "binding_alias_referenced_by_remembered_aliases"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn build_binding_record_from_capture_candidate_preserves_capture_provenance() {
    let candidate = BindingCaptureCandidateInfo {
        session: BindingCaptureSessionInfo {
            session_id: "sess-1".to_string(),
            session_name: "default".to_string(),
            rub_home_reference: "/tmp/rub-home".to_string(),
            rub_home_temp_owned: false,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: Some("profile:Work".to_string()),
            connection_target: None,
            profile_directory_reference: Some("/Users/test/Profile 2".to_string()),
            user_data_dir_reference: Some("/Users/test/Chrome".to_string()),
        },
        capture_fence: BindingCaptureFenceInfo {
            status: BindingCaptureFenceStatus::CaptureReady,
            capture_eligible: true,
            bind_current_eligible: true,
            capture_fence: Some("handoff_complete".to_string()),
            status_reason: Some("human_verification_handoff_completed".to_string()),
        },
        auth_evidence: BindingCaptureAuthEvidence {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 4,
            auth_signals: vec!["cookies_present".to_string()],
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
                session_id: "sess-1".to_string(),
                session_name: "default".to_string(),
            },
            attachment_identity: Some("profile:Work".to_string()),
        },
        auth_provenance_hint: BindingAuthProvenance {
            created_via: BindingCreatedVia::HandoffCompleted,
            auth_input_mode: BindingAuthInputMode::Human,
            capture_fence: Some("handoff_complete".to_string()),
            captured_from_session: Some("default".to_string()),
            captured_from_attachment_identity: Some("profile:Work".to_string()),
        },
        diagnostics: BindingCaptureDiagnostics::default(),
    };

    let binding = build_binding_record_from_candidate(
        "old-admin",
        &candidate,
        BindingWriteMode::Capture { auth_input: None },
    );
    assert_eq!(binding.alias, "old-admin");
    assert_eq!(
        binding.auth_provenance.created_via,
        BindingCreatedVia::HandoffCompleted
    );
    assert_eq!(
        binding.auth_provenance.capture_fence.as_deref(),
        Some("handoff_complete")
    );
    assert_eq!(
        binding.persistence_policy,
        BindingPersistencePolicy::RubHomeLocalDurable
    );
}

#[test]
fn build_binding_record_from_bind_current_drops_capture_claim() {
    let candidate = BindingCaptureCandidateInfo {
        session: BindingCaptureSessionInfo {
            session_id: "sess-1".to_string(),
            session_name: "default".to_string(),
            rub_home_reference: "/tmp/rub-home".to_string(),
            rub_home_temp_owned: false,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: Some("profile:Work".to_string()),
            connection_target: None,
            profile_directory_reference: None,
            user_data_dir_reference: None,
        },
        capture_fence: BindingCaptureFenceInfo {
            status: BindingCaptureFenceStatus::BindCurrentOnly,
            capture_eligible: false,
            bind_current_eligible: true,
            capture_fence: None,
            status_reason: Some("explicit_auth_completion_fence_missing".to_string()),
        },
        auth_evidence: BindingCaptureAuthEvidence {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 1,
            auth_signals: vec![],
            degraded_reason: None,
        },
        durability: BindingCaptureDurabilityInfo {
            persistence_policy: BindingPersistencePolicy::RubHomeLocalEphemeral,
            durability_scope: BindingDurabilityScope::RubHomeLocalEphemeral,
            reattachment_mode: BindingReattachmentMode::TempHomeEphemeral,
            status_reason: Some("temp_owned_rub_home_is_ephemeral".to_string()),
        },
        live_correlation: BindingCaptureLiveCorrelation {
            session_reference: BindingSessionReference {
                kind: BindingSessionReferenceKind::LiveSessionHint,
                session_id: "sess-1".to_string(),
                session_name: "default".to_string(),
            },
            attachment_identity: Some("profile:Work".to_string()),
        },
        auth_provenance_hint: BindingAuthProvenance {
            created_via: BindingCreatedVia::HandoffCompleted,
            auth_input_mode: BindingAuthInputMode::Human,
            capture_fence: Some("handoff_complete".to_string()),
            captured_from_session: Some("default".to_string()),
            captured_from_attachment_identity: Some("profile:Work".to_string()),
        },
        diagnostics: BindingCaptureDiagnostics::default(),
    };

    let binding =
        build_binding_record_from_candidate("old-admin", &candidate, BindingWriteMode::BindCurrent);
    assert_eq!(
        binding.auth_provenance.created_via,
        BindingCreatedVia::BoundExistingRuntime
    );
    assert_eq!(
        binding.auth_provenance.auth_input_mode,
        BindingAuthInputMode::Unknown
    );
    assert!(binding.auth_provenance.capture_fence.is_none());
    assert_eq!(
        binding.persistence_policy,
        BindingPersistencePolicy::RubHomeLocalEphemeral
    );
}

#[test]
fn build_binding_record_from_explicit_cli_capture_uses_operator_cli_fence() {
    let candidate = BindingCaptureCandidateInfo {
        session: BindingCaptureSessionInfo {
            session_id: "sess-1".to_string(),
            session_name: "default".to_string(),
            rub_home_reference: "/tmp/rub-home".to_string(),
            rub_home_temp_owned: false,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: Some("profile:Work".to_string()),
            connection_target: None,
            profile_directory_reference: Some("/Users/test/Profile 2".to_string()),
            user_data_dir_reference: Some("/Users/test/Chrome".to_string()),
        },
        capture_fence: BindingCaptureFenceInfo {
            status: BindingCaptureFenceStatus::BindCurrentOnly,
            capture_eligible: false,
            bind_current_eligible: true,
            capture_fence: None,
            status_reason: Some("explicit_auth_completion_fence_missing".to_string()),
        },
        auth_evidence: BindingCaptureAuthEvidence {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 4,
            auth_signals: vec!["cookies_present".to_string()],
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
                session_id: "sess-1".to_string(),
                session_name: "default".to_string(),
            },
            attachment_identity: Some("profile:Work".to_string()),
        },
        auth_provenance_hint: BindingAuthProvenance {
            created_via: BindingCreatedVia::Unknown,
            auth_input_mode: BindingAuthInputMode::Unknown,
            capture_fence: None,
            captured_from_session: Some("default".to_string()),
            captured_from_attachment_identity: Some("profile:Work".to_string()),
        },
        diagnostics: BindingCaptureDiagnostics::default(),
    };

    let binding = build_binding_record_from_candidate(
        "old-admin",
        &candidate,
        BindingWriteMode::Capture {
            auth_input: Some(BindingCaptureAuthInputArg::Cli),
        },
    );
    assert_eq!(
        binding.auth_provenance.created_via,
        BindingCreatedVia::CliAuthCompleted
    );
    assert_eq!(
        binding.auth_provenance.auth_input_mode,
        BindingAuthInputMode::Cli
    );
    assert_eq!(
        binding.auth_provenance.capture_fence.as_deref(),
        Some("explicit_cli_auth_capture")
    );
}

#[test]
fn build_binding_record_from_explicit_mixed_capture_preserves_real_capture_fence() {
    let candidate = BindingCaptureCandidateInfo {
        session: BindingCaptureSessionInfo {
            session_id: "sess-1".to_string(),
            session_name: "default".to_string(),
            rub_home_reference: "/tmp/rub-home".to_string(),
            rub_home_temp_owned: false,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: Some("profile:Work".to_string()),
            connection_target: None,
            profile_directory_reference: Some("/Users/test/Profile 2".to_string()),
            user_data_dir_reference: Some("/Users/test/Chrome".to_string()),
        },
        capture_fence: BindingCaptureFenceInfo {
            status: BindingCaptureFenceStatus::CaptureReady,
            capture_eligible: true,
            bind_current_eligible: true,
            capture_fence: Some("handoff_complete".to_string()),
            status_reason: Some("human_verification_handoff_completed".to_string()),
        },
        auth_evidence: BindingCaptureAuthEvidence {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 4,
            auth_signals: vec!["cookies_present".to_string()],
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
                session_id: "sess-1".to_string(),
                session_name: "default".to_string(),
            },
            attachment_identity: Some("profile:Work".to_string()),
        },
        auth_provenance_hint: BindingAuthProvenance {
            created_via: BindingCreatedVia::HandoffCompleted,
            auth_input_mode: BindingAuthInputMode::Human,
            capture_fence: Some("handoff_complete".to_string()),
            captured_from_session: Some("default".to_string()),
            captured_from_attachment_identity: Some("profile:Work".to_string()),
        },
        diagnostics: BindingCaptureDiagnostics::default(),
    };

    let binding = build_binding_record_from_candidate(
        "old-admin",
        &candidate,
        BindingWriteMode::Capture {
            auth_input: Some(BindingCaptureAuthInputArg::Mixed),
        },
    );
    assert_eq!(
        binding.auth_provenance.created_via,
        BindingCreatedVia::HandoffCompleted
    );
    assert_eq!(
        binding.auth_provenance.auth_input_mode,
        BindingAuthInputMode::Mixed
    );
    assert_eq!(
        binding.auth_provenance.capture_fence.as_deref(),
        Some("handoff_complete")
    );
}
