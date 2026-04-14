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
    set_secret_value(&home, "RUB_TOKEN", Some("hunter2"), None, false).expect("set should succeed");

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
    remove_secret_value(&home, "RUB_TOKEN").expect("remove should succeed");

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
