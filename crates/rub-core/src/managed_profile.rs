use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::process::extract_flag_value;

pub const MANAGED_PROFILE_PREFIX: &str = "rub-chrome";
pub const MANAGED_PROFILE_TEMP_OWNERSHIP_MARKER: &str = ".rub-managed-profile-temp-owned";

fn normalize_managed_profile_scope(scope: &str) -> String {
    if scope.is_empty() {
        return "hex-empty".to_string();
    }
    let mut encoded = String::with_capacity("hex-".len() + scope.len() * 2);
    encoded.push_str("hex-");
    for byte in scope.as_bytes() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn push_temp_root_aliases(paths: &mut Vec<PathBuf>, root: PathBuf) {
    push_unique_path(paths, root.clone());
    if let Ok(canonical) = std::fs::canonicalize(&root) {
        push_unique_path(paths, canonical);
    }
    if let Some(stripped) = root.to_string_lossy().strip_prefix("/private") {
        push_unique_path(paths, PathBuf::from(stripped));
    } else {
        push_unique_path(
            paths,
            PathBuf::from("/private").join(root.strip_prefix("/").unwrap_or(&root)),
        );
    }
}

pub fn managed_profile_temp_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_temp_root_aliases(&mut roots, std::env::temp_dir());
    push_temp_root_aliases(&mut roots, PathBuf::from("/tmp"));
    roots
}

pub fn is_managed_profile_dir_name(name: &str) -> bool {
    name.starts_with(&format!("{MANAGED_PROFILE_PREFIX}-"))
}

pub fn is_managed_profile_path(path: &Path) -> bool {
    managed_profile_identity_suffix(path).is_some()
}

pub fn projected_managed_profile_path_for_scope(scope: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{MANAGED_PROFILE_PREFIX}-{}",
        normalize_managed_profile_scope(scope)
    ))
}

pub fn projected_managed_profile_path_for_session(session_id: &str) -> PathBuf {
    projected_managed_profile_path_for_scope(&format!("session-{session_id}"))
}

pub fn extract_managed_profile_path_from_command(command: &str) -> Option<PathBuf> {
    let user_data_dir = PathBuf::from(extract_flag_value(command, "--user-data-dir")?);
    is_managed_profile_path(&user_data_dir).then_some(user_data_dir)
}

pub fn managed_profile_paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    if !is_managed_profile_path(left) || !is_managed_profile_path(right) {
        return false;
    }

    if std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
    {
        return true;
    }

    normalize_private_alias(left) == normalize_private_alias(right)
}

pub fn managed_profile_temp_ownership_marker_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join(MANAGED_PROFILE_TEMP_OWNERSHIP_MARKER)
}

pub fn has_temp_owned_managed_profile_marker(profile_dir: &Path) -> bool {
    managed_profile_temp_ownership_marker_path(profile_dir).exists()
}

pub fn is_temp_owned_managed_profile_path(profile_dir: &Path) -> bool {
    is_managed_profile_path(profile_dir) && has_temp_owned_managed_profile_marker(profile_dir)
}

pub fn sync_temp_owned_managed_profile_marker(
    profile_dir: &Path,
    temp_owned: bool,
) -> std::io::Result<()> {
    let marker = managed_profile_temp_ownership_marker_path(profile_dir);
    if temp_owned {
        std::fs::create_dir_all(profile_dir)?;
        std::fs::write(marker, b"temp_owned=1\n")?;
        return Ok(());
    }

    match std::fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn managed_profile_identity_suffix(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name().and_then(|name| name.to_str())?;
    if !is_managed_profile_dir_name(file_name) {
        return None;
    }

    managed_profile_temp_roots()
        .into_iter()
        .find_map(|root| path.strip_prefix(&root).ok().map(PathBuf::from))
}

fn normalize_private_alias(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("/private") {
        PathBuf::from("/").join(stripped)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_managed_profile_path_from_command, has_temp_owned_managed_profile_marker,
        is_managed_profile_dir_name, is_managed_profile_path, is_temp_owned_managed_profile_path,
        managed_profile_paths_equivalent, managed_profile_temp_ownership_marker_path,
        managed_profile_temp_roots, normalize_private_alias,
        projected_managed_profile_path_for_scope, projected_managed_profile_path_for_session,
        sync_temp_owned_managed_profile_marker,
    };
    use std::path::PathBuf;

    #[test]
    fn projected_managed_profile_path_keeps_scope_stable() {
        let path = projected_managed_profile_path_for_scope("session:abc/123");
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "rub-chrome-hex-73657373696f6e3a6162632f313233")
        );
    }

    #[test]
    fn projected_managed_profile_path_is_injective_for_punctuation() {
        let colon = projected_managed_profile_path_for_scope("a:b");
        let question = projected_managed_profile_path_for_scope("a?b");
        let literal = projected_managed_profile_path_for_scope("a_3ab");

        assert_ne!(colon, question);
        assert_ne!(colon, literal);
        assert_ne!(question, literal);
    }

    #[test]
    fn session_scoped_profile_path_is_recognized_as_managed_authority() {
        let path = projected_managed_profile_path_for_session("sess-123");
        assert!(is_managed_profile_path(&path));
    }

    #[test]
    fn legacy_pid_scoped_profile_path_is_still_recognized() {
        let path = std::env::temp_dir().join("rub-chrome-4242");
        assert!(is_managed_profile_path(&path));
        assert!(is_managed_profile_dir_name("rub-chrome-4242"));
    }

    #[test]
    fn command_profile_extraction_returns_exact_managed_profile_path() {
        let profile = projected_managed_profile_path_for_session("sess-extract");
        let command = format!(
            "Google Chrome --user-data-dir=\"{}\" --remote-debugging-port=0",
            profile.display()
        );
        assert_eq!(
            extract_managed_profile_path_from_command(&command),
            Some(profile)
        );
    }

    #[test]
    fn managed_profile_temp_roots_include_canonical_temp_dir_aliases() {
        let roots = managed_profile_temp_roots();
        assert!(roots.contains(&std::env::temp_dir()));
        assert!(roots.contains(&PathBuf::from("/tmp")));
    }

    #[test]
    fn temp_owned_marker_tracks_cleanup_ownership_without_changing_identity() {
        let profile = projected_managed_profile_path_for_session("sess-owned-marker");
        let _ = std::fs::remove_dir_all(&profile);

        assert!(is_managed_profile_path(&profile));
        assert!(!is_temp_owned_managed_profile_path(&profile));

        sync_temp_owned_managed_profile_marker(&profile, true).expect("create temp-owned marker");
        assert!(has_temp_owned_managed_profile_marker(&profile));
        assert!(is_temp_owned_managed_profile_path(&profile));

        sync_temp_owned_managed_profile_marker(&profile, false).expect("remove temp-owned marker");
        assert!(!has_temp_owned_managed_profile_marker(&profile));
        assert!(!is_temp_owned_managed_profile_path(&profile));

        let _ = std::fs::remove_file(managed_profile_temp_ownership_marker_path(&profile));
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn explicit_tmp_profile_shape_does_not_imply_temp_owned_cleanup_authority() {
        let profile = std::env::temp_dir().join("rub-chrome-my-workspace");
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(&profile).expect("create explicit tmp profile");

        assert!(is_managed_profile_path(&profile));
        assert!(!is_temp_owned_managed_profile_path(&profile));

        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn temp_alias_paths_compare_as_same_managed_profile_identity() {
        let tmp_alias = PathBuf::from("/tmp/rub-chrome-session-alias");
        let private_alias = PathBuf::from("/private/tmp/rub-chrome-session-alias");
        assert!(managed_profile_paths_equivalent(&tmp_alias, &private_alias));
    }

    #[test]
    fn private_var_temp_alias_paths_compare_as_same_managed_profile_identity() {
        let alias = std::env::temp_dir().join("rub-chrome-session-private-alias");
        let private_alias = if let Ok(stripped) = alias.strip_prefix("/private") {
            PathBuf::from("/").join(stripped)
        } else {
            PathBuf::from("/private").join(alias.strip_prefix("/").unwrap_or(&alias))
        };
        assert!(managed_profile_paths_equivalent(&alias, &private_alias));
    }

    #[test]
    fn same_suffix_under_different_temp_roots_is_not_same_managed_profile_authority() {
        let name = "rub-chrome-session-suffix-collision";
        let tmp_alias = PathBuf::from("/tmp").join(name);
        let env_temp_alias = std::env::temp_dir().join(name);

        assert!(is_managed_profile_path(&tmp_alias));
        assert!(is_managed_profile_path(&env_temp_alias));

        let same_root_alias =
            normalize_private_alias(&tmp_alias) == normalize_private_alias(&env_temp_alias);
        assert_eq!(
            managed_profile_paths_equivalent(&tmp_alias, &env_temp_alias),
            same_root_alias,
            "managed profile equivalence must not collapse same suffix across distinct temp roots"
        );
    }
}
