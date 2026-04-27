use super::{
    forget_remembered_alias, project_remembered_alias_list, read_remembered_alias_registry,
    rebind_remembered_alias, remember_binding_alias, remembered_aliases_referencing_binding,
    resolve_remembered_alias, write_remembered_alias_registry,
};
use crate::binding_ctl::write_binding_registry;
use crate::commands::RememberedBindingAliasKindArg;
use rub_core::error::ErrorCode;
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCreatedVia, BindingPersistencePolicy,
    BindingRecord, BindingRegistryData, BindingScope, RememberedBindingAliasKind,
    RememberedBindingAliasRecord, RememberedBindingAliasRegistryData,
};
use rub_daemon::rub_paths::RubPaths;
use std::path::{Path, PathBuf};

fn temp_home() -> PathBuf {
    std::env::temp_dir().join(format!("rub-binding-memory-{}", uuid::Uuid::now_v7()))
}

fn sample_binding(alias: &str, home: &Path) -> BindingRecord {
    BindingRecord {
        alias: alias.to_string(),
        scope: BindingScope::RubHomeLocal,
        rub_home_reference: home.display().to_string(),
        session_reference: None,
        attachment_identity: Some("profile:Work".to_string()),
        profile_directory_reference: Some("Profile 1".to_string()),
        user_data_dir_reference: Some("/Users/test/Chrome".to_string()),
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

fn seed_binding_registry(home: &Path) {
    write_binding_registry(
        home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![sample_binding("old-admin", home)],
        },
    )
    .unwrap();
}

#[test]
fn remembered_alias_registry_defaults_to_v1_empty_registry() {
    let home = temp_home();
    let registry = read_remembered_alias_registry(&home).unwrap();
    assert_eq!(registry.schema_version, 1);
    assert!(registry.aliases.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn write_remembered_alias_registry_surfaces_rub_home_directory_durability_failure() {
    let root = std::env::temp_dir().join(format!(
        "rub-binding-memory-dir-fence-{}",
        uuid::Uuid::now_v7()
    ));
    let home = root.join("home");
    let _ = std::fs::remove_dir_all(&root);
    crate::local_registry::force_directory_sync_failure_once_for_test(&home);

    let registry = RememberedBindingAliasRegistryData {
        schema_version: 1,
        aliases: vec![RememberedBindingAliasRecord {
            alias: "finance".to_string(),
            binding_alias: "old-admin".to_string(),
            kind: RememberedBindingAliasKind::Workspace,
            created_at: "2026-04-14T12:00:00Z".to_string(),
            updated_at: "2026-04-14T12:00:00Z".to_string(),
        }],
    };
    let envelope = write_remembered_alias_registry(&home, &registry)
        .expect_err("remembered binding registry must reject unconfirmed RUB_HOME directory fence")
        .into_envelope();

    assert_eq!(envelope.code, ErrorCode::IoError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|reason| reason.as_str()),
        Some("remembered_binding_rub_home_create_failed")
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
fn write_remembered_alias_registry_rejects_published_only_file_commit() {
    let home = temp_home();
    let registry_path = RubPaths::new(&home).remembered_bindings_path();
    crate::local_registry::force_published_write_outcome_once_for_test(&registry_path);

    let registry = RememberedBindingAliasRegistryData {
        schema_version: 1,
        aliases: vec![RememberedBindingAliasRecord {
            alias: "finance".to_string(),
            binding_alias: "old-admin".to_string(),
            kind: RememberedBindingAliasKind::Workspace,
            created_at: "2026-04-14T12:00:00Z".to_string(),
            updated_at: "2026-04-14T12:00:00Z".to_string(),
        }],
    };
    let envelope = write_remembered_alias_registry(&home, &registry)
        .expect_err("remembered binding registry must reject published-only file commit")
        .into_envelope();

    assert_eq!(envelope.code, ErrorCode::IoError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|reason| reason.as_str()),
        Some("remembered_binding_registry_write_failed")
    );
    assert!(
        envelope.message.contains("durability was not confirmed"),
        "{}",
        envelope.message
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remember_binding_alias_requires_existing_binding() {
    let home = temp_home();
    let error = remember_binding_alias(
        &home,
        "finance",
        "missing",
        RememberedBindingAliasKindArg::Workspace,
    )
    .expect_err("missing target binding should fail");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remember_and_resolve_binding_alias_projects_target() {
    let home = temp_home();
    seed_binding_registry(&home);

    let created = remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
    assert_eq!(created["result"]["remembered_alias"]["kind"], "workspace");

    let resolved = resolve_remembered_alias(&home, "finance").unwrap();
    assert_eq!(resolved["result"]["target"]["kind"], "resolved");
    assert_eq!(resolved["result"]["target"]["binding_alias"], "old-admin");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rebind_updates_target_and_forget_removes_alias() {
    let home = temp_home();
    write_binding_registry(
        &home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![
                sample_binding("old-admin", &home),
                sample_binding("ops", &home),
            ],
        },
    )
    .unwrap();
    remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Account,
    )
    .unwrap();

    let rebound = rebind_remembered_alias(&home, "finance", "ops", None).unwrap();
    assert_eq!(rebound["result"]["previous_binding_alias"], "old-admin");
    assert_eq!(rebound["result"]["target"]["binding_alias"], "ops");

    let references = remembered_aliases_referencing_binding(&home, "ops").unwrap();
    assert_eq!(references, vec!["finance".to_string()]);

    forget_remembered_alias(&home, "finance").unwrap();
    let registry = read_remembered_alias_registry(&home).unwrap();
    assert!(registry.aliases.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_list_surfaces_missing_target_binding() {
    let home = temp_home();
    write_remembered_alias_registry(
        &home,
        &RememberedBindingAliasRegistryData {
            schema_version: 1,
            aliases: vec![RememberedBindingAliasRecord {
                alias: "finance".to_string(),
                kind: RememberedBindingAliasKind::Workspace,
                binding_alias: "missing".to_string(),
                created_at: "2026-04-14T12:00:00Z".to_string(),
                updated_at: "2026-04-14T12:00:00Z".to_string(),
            }],
        },
    )
    .unwrap();

    let projection = project_remembered_alias_list(&home).unwrap();
    assert_eq!(
        projection["result"]["items"][0]["target"]["kind"],
        "missing_binding"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_projections_surface_live_registry_authority_failure_metadata() {
    let home = temp_home();
    seed_binding_registry(&home);
    remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
    std::fs::write(
        RubPaths::new(&home).registry_path(),
        "{ invalid live registry json",
    )
    .unwrap();

    let list_projection = project_remembered_alias_list(&home).unwrap();
    assert_eq!(
        list_projection["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );

    let resolved = resolve_remembered_alias(&home, "finance").unwrap();
    assert_eq!(
        resolved["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn remembered_alias_write_projections_surface_live_registry_authority_failure_metadata() {
    let home = temp_home();
    seed_binding_registry(&home);
    std::fs::write(
        RubPaths::new(&home).registry_path(),
        "{ invalid live registry json",
    )
    .unwrap();

    let created = remember_binding_alias(
        &home,
        "finance",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
    assert_eq!(
        created["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );

    write_binding_registry(
        &home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![
                sample_binding("old-admin", &home),
                sample_binding("ops", &home),
            ],
        },
    )
    .unwrap();
    let rebound = rebind_remembered_alias(&home, "finance", "ops", None).unwrap();
    assert_eq!(
        rebound["result"]["live_registry_error"]["code"],
        "DAEMON_START_FAILED"
    );

    let _ = std::fs::remove_dir_all(home);
}
