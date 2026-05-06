use std::path::{Component, Path, PathBuf};

pub(crate) fn canonical_existing_or_normalized_absolute_path(path: impl AsRef<Path>) -> PathBuf {
    let normalized = normalize_path_lexically(&absolute_path(path.as_ref()));
    normalized
        .canonicalize()
        .ok()
        .or_else(|| canonicalize_existing_ancestor(&normalized))
        .unwrap_or(normalized)
}

pub(crate) fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut probe = path;
    let mut suffix = Vec::new();

    while !probe.exists() {
        let component = probe.file_name()?.to_os_string();
        suffix.push(component);
        probe = probe.parent()?;
    }

    let mut canonical = probe.canonicalize().ok()?;
    for component in suffix.iter().rev() {
        canonical.push(component);
    }
    Some(normalize_path_lexically(&canonical))
}
