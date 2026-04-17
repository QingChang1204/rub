use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalAssetPathIdentity {
    authority_path: PathBuf,
    file_identity: Option<FileIdentity>,
}

impl LocalAssetPathIdentity {
    pub(crate) fn resolve(path: &Path) -> std::io::Result<Self> {
        let authority_path = resolve_local_asset_authority_path(path)?;
        let file_identity = FileIdentity::from_path(&authority_path)?;
        Ok(Self {
            authority_path,
            file_identity,
        })
    }

    pub(crate) fn authority_path(&self) -> &Path {
        &self.authority_path
    }

    pub(crate) fn conflicts_with(&self, other: &Self) -> bool {
        self.authority_path == other.authority_path
            || matches!(
                (self.file_identity, other.file_identity),
                (Some(left), Some(right)) if left == right
            )
    }
}

pub(crate) fn resolve_local_asset_authority_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = absolutize_and_normalize(path)?;
    if absolute.exists() {
        return std::fs::canonicalize(&absolute);
    }

    let (existing_ancestor, suffix) = split_existing_ancestor(&absolute)?;
    let mut authority = std::fs::canonicalize(existing_ancestor)?;
    for component in suffix {
        authority.push(component);
    }
    Ok(authority)
}

fn absolutize_and_normalize(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(normalize_path_lexically(&absolute))
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && normalized.as_os_str().is_empty() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(value) => normalized.push(value),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn split_existing_ancestor(path: &Path) -> std::io::Result<(&Path, Vec<OsString>)> {
    let mut current = path;
    let mut suffix = Vec::new();
    while !current.exists() {
        let Some(name) = current.file_name() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Cannot resolve a stable local asset authority for {}",
                    path.display()
                ),
            ));
        };
        suffix.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Cannot resolve a stable local asset authority for {} because no existing ancestor was found",
                    path.display()
                ),
            ));
        };
        current = parent;
    }
    suffix.reverse();
    Ok((current, suffix))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl FileIdentity {
    fn from_path(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt;

        match std::fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity;

#[cfg(not(unix))]
impl FileIdentity {
    fn from_path(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::metadata(path) {
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalAssetPathIdentity, resolve_local_asset_authority_path};

    #[test]
    fn resolves_relative_and_absolute_spellings_to_same_authority() {
        let root = std::env::temp_dir().join(format!(
            "rub-local-asset-authority-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).expect("create root");

        let alternate = root.join("nested").join("..").join("file.json");
        let relative = LocalAssetPathIdentity::resolve(&alternate).expect("relative authority");
        let absolute =
            LocalAssetPathIdentity::resolve(&root.join("file.json")).expect("absolute authority");

        assert!(relative.conflicts_with(&absolute));
        assert_eq!(relative.authority_path(), absolute.authority_path());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_symlinked_parent_to_canonical_authority() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("rub-local-asset-symlink-{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&root);
        let actual = root.join("actual");
        let alias = root.join("alias");
        std::fs::create_dir_all(&actual).expect("create actual");
        symlink(&actual, &alias).expect("create alias");

        let actual_path = resolve_local_asset_authority_path(&actual.join("rule.json"))
            .expect("actual authority");
        let alias_path =
            resolve_local_asset_authority_path(&alias.join("rule.json")).expect("alias authority");
        assert_eq!(actual_path, alias_path);

        let _ = std::fs::remove_dir_all(root);
    }
}
