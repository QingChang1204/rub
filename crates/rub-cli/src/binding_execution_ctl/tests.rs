use super::resolve_command_execution_binding;
use crate::binding_ctl::write_binding_registry;
use crate::binding_memory_ctl::remember_binding_alias;
use crate::commands::{
    Commands, EffectiveCli, RememberedBindingAliasKindArg, RequestedLaunchPolicy,
};
use rub_core::error::ErrorCode;
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCreatedVia, BindingExecutionMode,
    BindingPersistencePolicy, BindingRecord, BindingRegistryData, BindingScope,
    BindingSessionReference, BindingSessionReferenceKind, RememberedBindingAliasKind,
};
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

fn temp_home() -> PathBuf {
    std::env::temp_dir().join(format!("rub-binding-execution-{}", Uuid::now_v7()))
}

fn cli(home: &Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: false,
        profile: None,
        use_alias: Some("finance".to_string()),
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: RequestedLaunchPolicy::default(),
        effective_launch_policy: RequestedLaunchPolicy::default(),
    }
}

fn write_binding(home: &Path, persistence_policy: BindingPersistencePolicy) {
    write_binding_registry(
        home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![BindingRecord {
                alias: "old-admin".to_string(),
                scope: BindingScope::RubHomeLocal,
                rub_home_reference: home.display().to_string(),
                session_reference: Some(BindingSessionReference {
                    kind: BindingSessionReferenceKind::LiveSessionHint,
                    session_id: "sess-work".to_string(),
                    session_name: "work".to_string(),
                }),
                attachment_identity: Some("user_data_dir:/tmp/work".to_string()),
                profile_directory_reference: Some("/tmp/work/Default".to_string()),
                user_data_dir_reference: Some("/tmp/work".to_string()),
                auth_provenance: BindingAuthProvenance {
                    created_via: BindingCreatedVia::BoundExistingRuntime,
                    auth_input_mode: BindingAuthInputMode::Unknown,
                    capture_fence: None,
                    captured_from_session: Some("work".to_string()),
                    captured_from_attachment_identity: Some("user_data_dir:/tmp/work".to_string()),
                },
                persistence_policy,
                created_at: "2026-04-14T00:00:00Z".to_string(),
                last_captured_at: "2026-04-14T00:00:00Z".to_string(),
            }],
        },
    )
    .unwrap();
    remember_binding_alias(
        home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
}

fn write_profile_binding(home: &Path, persistence_policy: BindingPersistencePolicy) {
    write_binding_registry(
        home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![BindingRecord {
                alias: "old-admin".to_string(),
                scope: BindingScope::RubHomeLocal,
                rub_home_reference: home.display().to_string(),
                session_reference: Some(BindingSessionReference {
                    kind: BindingSessionReferenceKind::LiveSessionHint,
                    session_id: "sess-work".to_string(),
                    session_name: "work".to_string(),
                }),
                attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
                profile_directory_reference: Some("/tmp/work/Profile 3".to_string()),
                user_data_dir_reference: Some("/tmp/work".to_string()),
                auth_provenance: BindingAuthProvenance {
                    created_via: BindingCreatedVia::BoundExistingRuntime,
                    auth_input_mode: BindingAuthInputMode::Unknown,
                    capture_fence: None,
                    captured_from_session: Some("work".to_string()),
                    captured_from_attachment_identity: Some(
                        "profile:/tmp/work/Profile 3".to_string(),
                    ),
                },
                persistence_policy,
                created_at: "2026-04-14T00:00:00Z".to_string(),
                last_captured_at: "2026-04-14T00:00:00Z".to_string(),
            }],
        },
    )
    .unwrap();
    remember_binding_alias(
        home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
}

#[test]
fn remembered_alias_live_match_reuses_live_session() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_binding(&home, BindingPersistencePolicy::RubHomeLocalDurable);
    let runtime = RubPaths::new(&home).session_runtime("work", "sess-work");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit parent"),
    )
    .unwrap();
    std::fs::write(runtime.startup_committed_path(), "sess-work").unwrap();
    if let Some(parent) = runtime.socket_path().parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let _server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0_u8; 512];
            let _ = stream.read(&mut buffer);
            thread::sleep(Duration::from_millis(1_000));
        }
    });
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-work".to_string(),
                session_name: "work".to_string(),
                pid: std::process::id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-14T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: Some("/tmp/work".to_string()),
                attachment_identity: Some("user_data_dir:/tmp/work".to_string()),
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let resolved = resolve_command_execution_binding(&cli(&home)).unwrap();
    assert_eq!(resolved.cli.session, "work");
    assert!(resolved.cli.session_id.is_some());
    assert!(resolved.cli.use_alias.is_none());
    assert!(resolved.cli.user_data_dir.is_none());
    assert!(resolved.cli.requested_launch_policy.user_data_dir.is_none());
    assert!(resolved.cli.effective_launch_policy.user_data_dir.is_none());
    let projection = resolved.projection.expect("projection");
    assert_eq!(projection.binding_alias, "old-admin");
    assert_eq!(projection.effective_session_name, "work");
    assert_eq!(
        projection.remembered_alias_kind,
        RememberedBindingAliasKind::Workspace
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_live_match_clears_default_user_data_dir_from_reuse_path() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_binding(&home, BindingPersistencePolicy::RubHomeLocalDurable);
    let runtime = RubPaths::new(&home).session_runtime("work", "sess-work");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit parent"),
    )
    .unwrap();
    std::fs::write(runtime.startup_committed_path(), "sess-work").unwrap();
    if let Some(parent) = runtime.socket_path().parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let _server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0_u8; 512];
            let _ = stream.read(&mut buffer);
            thread::sleep(Duration::from_millis(1_000));
        }
    });
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-work".to_string(),
                session_name: "work".to_string(),
                pid: std::process::id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-14T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: Some("/tmp/work".to_string()),
                attachment_identity: Some("user_data_dir:/tmp/work".to_string()),
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let mut cli = cli(&home);
    cli.user_data_dir = Some("/tmp/config-default-profile-root".to_string());
    cli.effective_launch_policy.user_data_dir =
        Some("/tmp/config-default-profile-root".to_string());

    let resolved = resolve_command_execution_binding(&cli).unwrap();
    assert!(resolved.cli.user_data_dir.is_none());
    assert!(resolved.cli.requested_launch_policy.user_data_dir.is_none());
    assert!(resolved.cli.effective_launch_policy.user_data_dir.is_none());

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_no_live_match_launches_bound_runtime_from_user_data_dir() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_binding(&home, BindingPersistencePolicy::RubHomeLocalDurable);

    let resolved = resolve_command_execution_binding(&cli(&home)).unwrap();
    assert_eq!(resolved.cli.session, "default");
    assert_eq!(resolved.cli.user_data_dir.as_deref(), Some("/tmp/work"));
    assert_eq!(
        resolved
            .cli
            .requested_launch_policy
            .user_data_dir
            .as_deref(),
        Some("/tmp/work")
    );
    let projection = resolved.projection.expect("projection");
    assert_eq!(
        projection.effective_user_data_dir.as_deref(),
        Some("/tmp/work")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_no_live_match_launches_bound_profile_without_collapsing_to_root() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_profile_binding(&home, BindingPersistencePolicy::RubHomeLocalDurable);

    let resolved = resolve_command_execution_binding(&cli(&home)).unwrap();
    assert_eq!(resolved.cli.session, "default");
    assert_eq!(resolved.cli.profile.as_deref(), Some("Profile 3"));
    assert!(resolved.cli.user_data_dir.is_none());
    assert!(resolved.cli.requested_launch_policy.user_data_dir.is_none());
    let projection = resolved.projection.expect("projection");
    assert_eq!(projection.mode, BindingExecutionMode::LaunchBoundProfile);
    assert_eq!(
        projection.effective_profile_dir_name.as_deref(),
        Some("Profile 3")
    );
    assert!(projection.effective_user_data_dir.is_none());

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_profile_binding_without_profile_dir_fails_closed() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_profile_binding(&home, BindingPersistencePolicy::RubHomeLocalDurable);

    let mut registry = crate::binding_ctl::read_binding_registry(&home).unwrap();
    registry.bindings[0].profile_directory_reference = None;
    crate::binding_ctl::write_binding_registry(&home, &registry).unwrap();

    let error = resolve_command_execution_binding(&cli(&home))
        .expect_err("profile binding without reusable profile dir should fail closed")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(
        error.context.expect("error context")["reason"],
        "remembered_alias_has_no_reusable_launch_target"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_external_binding_requires_explicit_refresh() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_binding(
        &home,
        BindingPersistencePolicy::ExternalReattachmentRequired,
    );

    let error = resolve_command_execution_binding(&cli(&home))
        .expect_err("external binding should fail closed")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let context = error.context.expect("error context");
    assert_eq!(
        context["reason"],
        "remembered_alias_requires_external_reattachment"
    );

    let _ = std::fs::remove_dir_all(home);
}
