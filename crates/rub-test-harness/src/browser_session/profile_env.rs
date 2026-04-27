use std::path::{Path, PathBuf};

pub fn prepare_fake_profile_env() -> (PathBuf, PathBuf, Vec<(String, String)>) {
    let base = std::env::temp_dir().join(format!(
        "rub-profile-fixture-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let root = profile_root_for_test_base(&base);
    std::fs::create_dir_all(root.join("Default")).unwrap();
    std::fs::write(
        root.join("Local State"),
        r#"{
  "profile": {
    "info_cache": {
      "Default": {
        "name": "Default"
      }
    }
  }
}"#,
    )
    .unwrap();
    let envs = profile_envs_for_test_base(&base);
    let resolved_profile = root
        .join("Default")
        .canonicalize()
        .unwrap_or_else(|_| root.join("Default"));
    (base, resolved_profile, envs)
}

#[cfg(target_os = "macos")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome")
}

#[cfg(target_os = "linux")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("xdg").join("google-chrome")
}

#[cfg(target_os = "windows")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("LocalAppData")
        .join("Google")
        .join("Chrome")
        .join("User Data")
}

#[cfg(target_os = "macos")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![("HOME".to_string(), base.display().to_string())]
}

#[cfg(target_os = "linux")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![(
        "XDG_CONFIG_HOME".to_string(),
        base.join("xdg").display().to_string(),
    )]
}

#[cfg(target_os = "windows")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![(
        "LOCALAPPDATA".to_string(),
        base.join("LocalAppData").display().to_string(),
    )]
}
