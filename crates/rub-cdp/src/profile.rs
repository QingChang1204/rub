//! Chrome profile discovery and resolution.
//!
//! Resolves `--profile <name>` to a Chrome user-data-dir path by reading
//! Chrome's `Local State` JSON file.

use std::path::PathBuf;

use rub_core::error::{ErrorCode, RubError};

/// A Chrome profile entry read from `Local State`.
#[derive(Debug, Clone)]
pub struct ChromeProfile {
    /// Internal directory name (e.g., "Default", "Profile 1").
    pub dir_name: String,
    /// User-visible profile name (e.g., "Personal", "Work").
    pub display_name: String,
    /// Full path to the profile directory.
    pub path: PathBuf,
}

/// Platform-specific Chrome user data root directory.
pub fn chrome_user_data_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome")
        })
    }

    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
            .map(|c| c.join("google-chrome"))
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA").ok().map(|d| {
            PathBuf::from(d)
                .join("Google")
                .join("Chrome")
                .join("User Data")
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// List all Chrome profiles by reading `Local State`.
pub fn list_profiles() -> Result<Vec<ChromeProfile>, RubError> {
    let root = chrome_user_data_root().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ProfileNotFound,
            "Could not determine Chrome user data directory for this platform",
        )
    })?;

    let local_state_path = root.join("Local State");
    if !local_state_path.exists() {
        return Err(RubError::domain(
            ErrorCode::ProfileNotFound,
            format!(
                "Chrome Local State not found at {}",
                local_state_path.display()
            ),
        ));
    }

    let contents = std::fs::read_to_string(&local_state_path).map_err(|e| {
        RubError::domain(
            ErrorCode::ProfileNotFound,
            format!("Failed to read Local State: {e}"),
        )
    })?;

    let json: serde_json::Value = serde_json::from_str(&contents).map_err(|e| {
        RubError::domain(
            ErrorCode::ProfileNotFound,
            format!("Failed to parse Local State JSON: {e}"),
        )
    })?;

    let info_cache = json["profile"]["info_cache"].as_object().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ProfileNotFound,
            "No profile.info_cache in Local State",
        )
    })?;

    let mut profiles = Vec::new();
    for (dir_name, profile_data) in info_cache {
        let display_name = profile_data["name"]
            .as_str()
            .unwrap_or(dir_name)
            .to_string();
        let path = root.join(dir_name);
        profiles.push(ChromeProfile {
            dir_name: dir_name.clone(),
            display_name,
            path,
        });
    }

    Ok(profiles)
}

/// Resolve a profile name to its directory path.
///
/// Matching priority:
/// 1. Exact match on display name (case-insensitive)
/// 2. Exact match on directory name (case-insensitive)
/// 3. Prefix match on display name
pub fn resolve_profile(name: &str) -> Result<ChromeProfile, RubError> {
    let profiles = list_profiles()?;

    if profiles.is_empty() {
        return Err(RubError::domain(
            ErrorCode::ProfileNotFound,
            "No Chrome profiles found",
        ));
    }

    let name_lower = name.to_lowercase();

    // Exact display name match
    if let Some(p) = profiles
        .iter()
        .find(|p| p.display_name.to_lowercase() == name_lower)
    {
        return Ok(p.clone());
    }

    // Exact dir name match
    if let Some(p) = profiles
        .iter()
        .find(|p| p.dir_name.to_lowercase() == name_lower)
    {
        return Ok(p.clone());
    }

    // Prefix match on display name
    if let Some(p) = profiles
        .iter()
        .find(|p| p.display_name.to_lowercase().starts_with(&name_lower))
    {
        return Ok(p.clone());
    }

    let available: Vec<String> = profiles
        .iter()
        .map(|p| format!("'{}' ({})", p.display_name, p.dir_name))
        .collect();
    Err(RubError::domain(
        ErrorCode::ProfileNotFound,
        format!(
            "Profile '{}' not found. Available: {}",
            name,
            available.join(", ")
        ),
    ))
}
