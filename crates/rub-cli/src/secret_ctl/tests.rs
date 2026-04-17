use super::{
    SecretEffectiveSource, effective_secret_source, inspect_secret_value, project_secret_list,
    remove_secret_value, set_secret_value,
};
use rub_core::secrets_env::load_secrets_env_file;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn set_secret_value_creates_and_lists_secret_names_without_returning_values() {
    let home = unique_home();
    let set = set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false)
        .expect("set should succeed");
    assert_eq!(set["result"]["projection_state"]["durability"], "durable");
    assert_eq!(
        set["result"]["projection_state"]["persist_action"],
        "written"
    );
    assert!(set["result"].get("durability_confirmed").is_none());

    let listed = project_secret_list(&home).expect("list should succeed");
    let items = listed["result"]["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "RUB_TOKEN");
    assert_eq!(items[0]["effective_source"], "rub_home_secrets_env");
    assert_eq!(items[0]["environment_override_present"], false);
    assert!(listed.to_string().contains("RUB_TOKEN"));
    assert!(!listed.to_string().contains("hunter2"));
}

#[test]
fn remove_secret_value_deletes_file_when_store_becomes_empty() {
    let home = unique_home();
    set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false).expect("set should succeed");
    let removed = remove_secret_value(&home, "RUB_TOKEN").expect("remove should succeed");
    assert_eq!(
        removed["result"]["projection_state"]["durability"],
        "durable"
    );
    assert_eq!(
        removed["result"]["projection_state"]["persist_action"],
        "removed"
    );

    let path = rub_daemon::rub_paths::RubPaths::new(&home).secrets_env_path();
    assert!(!path.exists(), "empty secrets store should remove file");
}

#[test]
fn set_secret_value_persists_exact_secret_name_mapping() {
    let home = unique_home();
    set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false).expect("set should succeed");

    let stored =
        load_secrets_env_file(&rub_daemon::rub_paths::RubPaths::new(&home).secrets_env_path())
            .expect("load should succeed");
    assert_eq!(stored["RUB_TOKEN"], "hunter2");
}

#[test]
fn inspect_secret_value_reports_local_provenance_without_exposing_value() {
    let home = unique_home();
    set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false).expect("set should succeed");

    let inspected = inspect_secret_value(&home, "RUB_TOKEN").expect("inspect should succeed");
    assert_eq!(inspected["result"]["secret"]["name"], "RUB_TOKEN");
    assert_eq!(inspected["result"]["secret"]["reference"], "$RUB_TOKEN");
    assert_eq!(inspected["result"]["secret"]["local_store_present"], true);
    assert_eq!(
        inspected["result"]["secret"]["environment_override_present"],
        false
    );
    assert_eq!(
        inspected["result"]["secret"]["effective_source"],
        "rub_home_secrets_env"
    );
    assert!(!inspected.to_string().contains("hunter2"));
}

#[test]
fn set_secret_value_marks_rub_home_directory_when_home_create_fails() {
    let home = unique_home();
    std::fs::remove_dir_all(&home).expect("remove temp directory");
    std::fs::write(&home, b"not-a-directory").expect("write temp file");

    let error = set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false)
        .expect_err("rub_home create failure should propagate")
        .into_envelope();
    assert_eq!(error.code, rub_core::error::ErrorCode::IoError);
    let context = error.context.expect("context");
    assert_eq!(context["reason"], "secret_registry_rub_home_create_failed");
    assert_eq!(context["path_state"]["path_kind"], "rub_home_directory");

    let _ = std::fs::remove_file(home);
}

#[test]
fn project_secret_list_marks_secrets_env_file_when_load_fails() {
    let home = unique_home();
    let secrets_path = rub_daemon::rub_paths::RubPaths::new(&home).secrets_env_path();
    std::fs::remove_file(&secrets_path).ok();
    std::fs::create_dir_all(&secrets_path).expect("directory in place of secrets.env");

    let error = project_secret_list(&home)
        .expect_err("load failure should propagate")
        .into_envelope();
    let context = error.context.expect("context");
    assert_eq!(context["reason"], "secret_registry_load_failed");
    assert_eq!(context["path_state"]["path_kind"], "secrets_env_file");
}

#[test]
fn effective_secret_source_prefers_environment_over_local_store() {
    assert_eq!(
        effective_secret_source(true, true),
        SecretEffectiveSource::Environment
    );
    assert_eq!(
        effective_secret_source(true, false),
        SecretEffectiveSource::RubHomeSecretsEnv
    );
    assert_eq!(
        effective_secret_source(false, false),
        SecretEffectiveSource::Unresolved
    );
}

fn unique_home() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "rub-secret-ctl-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create temp home");
    root
}
