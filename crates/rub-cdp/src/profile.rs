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
/// 1. Unique exact match on display name or directory name (case-insensitive)
/// 2. Unique prefix match on display name
pub fn resolve_profile(name: &str) -> Result<ChromeProfile, RubError> {
    let profiles = list_profiles()?;
    resolve_profile_from_candidates(name, &profiles)
}

fn resolve_profile_from_candidates(
    name: &str,
    profiles: &[ChromeProfile],
) -> Result<ChromeProfile, RubError> {
    if profiles.is_empty() {
        return Err(RubError::domain(
            ErrorCode::ProfileNotFound,
            "No Chrome profiles found",
        ));
    }

    let name_lower = name.trim().to_lowercase();

    // Exact display-name / dir-name match, but fail closed if the exact token
    // resolves to multiple profiles across the two namespaces.
    let exact_matches = profiles
        .iter()
        .filter(|p| {
            p.display_name.to_lowercase() == name_lower || p.dir_name.to_lowercase() == name_lower
        })
        .cloned()
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0].clone());
    }
    if exact_matches.len() > 1 {
        let candidates = exact_matches
            .iter()
            .map(|p| format!("'{}' ({})", p.display_name, p.dir_name))
            .collect::<Vec<_>>();
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Profile '{}' is ambiguous across display and directory names. Matches: {}",
                name,
                candidates.join(", ")
            ),
        ));
    }

    // Unique prefix match on display name
    let prefix_matches = profiles
        .iter()
        .filter(|p| p.display_name.to_lowercase().starts_with(&name_lower))
        .cloned()
        .collect::<Vec<_>>();
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches[0].clone());
    }
    if prefix_matches.len() > 1 {
        let candidates = prefix_matches
            .iter()
            .map(|p| format!("'{}' ({})", p.display_name, p.dir_name))
            .collect::<Vec<_>>();
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Profile '{}' is ambiguous. Matches: {}",
                name,
                candidates.join(", ")
            ),
        ));
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

#[cfg(test)]
mod tests {
    use super::{ChromeProfile, resolve_profile_from_candidates};
    use rub_core::error::ErrorCode;
    use std::path::PathBuf;

    fn profile(dir_name: &str, display_name: &str) -> ChromeProfile {
        ChromeProfile {
            dir_name: dir_name.to_string(),
            display_name: display_name.to_string(),
            path: PathBuf::from(format!("/tmp/chrome/{dir_name}")),
        }
    }

    #[test]
    fn resolve_profile_prefers_exact_matches() {
        let profiles = vec![
            profile("Profile 1", "Work"),
            profile("Profile 2", "Work Finance"),
        ];

        let resolved = resolve_profile_from_candidates("Work", &profiles).unwrap();
        assert_eq!(resolved.dir_name, "Profile 1");

        let resolved_dir = resolve_profile_from_candidates("profile 2", &profiles).unwrap();
        assert_eq!(resolved_dir.display_name, "Work Finance");
    }

    #[test]
    fn resolve_profile_rejects_ambiguous_prefix_matches() {
        let profiles = vec![
            profile("Profile 1", "Work"),
            profile("Profile 2", "Work Finance"),
        ];

        let error = resolve_profile_from_candidates("wo", &profiles)
            .expect_err("ambiguous prefix should fail closed")
            .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("ambiguous"), "{}", error.message);
        assert!(error.message.contains("Work"), "{}", error.message);
        assert!(error.message.contains("Work Finance"), "{}", error.message);
    }

    #[test]
    fn resolve_profile_accepts_unique_prefix_match() {
        let profiles = vec![
            profile("Profile 1", "Personal"),
            profile("Profile 2", "Work Finance"),
        ];

        let resolved = resolve_profile_from_candidates("work f", &profiles).unwrap();
        assert_eq!(resolved.dir_name, "Profile 2");
    }

    #[test]
    fn resolve_profile_rejects_exact_cross_namespace_collision() {
        let profiles = vec![
            profile("Profile 2", "Personal"),
            profile("Profile 3", "Profile 2"),
        ];

        let error = resolve_profile_from_candidates("profile 2", &profiles)
            .expect_err("cross-namespace exact collision should fail closed")
            .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("ambiguous"), "{}", error.message);
        assert!(error.message.contains("Profile 2"), "{}", error.message);
        assert!(error.message.contains("Profile 3"), "{}", error.message);
    }
}
