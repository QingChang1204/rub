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
