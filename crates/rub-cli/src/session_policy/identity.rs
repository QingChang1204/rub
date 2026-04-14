use super::{ConnectionRequest, EffectiveCli};
use rub_core::error::RubError;
use std::path::{Component, Path, PathBuf};

pub(crate) fn requested_user_data_dir(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> Option<String> {
    match request {
        ConnectionRequest::Profile { user_data_root, .. } => Some(user_data_root.clone()),
        ConnectionRequest::UserDataDir { path } => Some(normalize_identity_path(path)),
        // Only managed sessions own a local user-data-dir authority. External
        // CDP attachment must not inherit local profile state from config
        // defaults because that would pollute shutdown/profile ownership.
        ConnectionRequest::None => cli.user_data_dir.clone(),
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => None,
    }
}

pub(crate) fn requested_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> Option<String> {
    match request {
        ConnectionRequest::Profile { resolved_path, .. } => {
            Some(format!("profile:{resolved_path}"))
        }
        ConnectionRequest::CdpUrl { url } => Some(format!("cdp:{}", normalize_cdp_identity(url))),
        ConnectionRequest::AutoDiscover => Some("auto_discover:local_cdp".to_string()),
        ConnectionRequest::UserDataDir { path } => {
            Some(format!("user_data_dir:{}", normalize_identity_path(path)))
        }
        ConnectionRequest::None => requested_user_data_dir(cli, request)
            .map(|path| format!("user_data_dir:{}", normalize_identity_path(&path))),
    }
}

fn attachment_identity_kind(identity: Option<&str>) -> Option<&str> {
    identity.and_then(|identity| identity.split_once(':').map(|(kind, _)| kind))
}

fn request_attachment_kind(request: &ConnectionRequest) -> Option<&'static str> {
    match request {
        ConnectionRequest::None => None,
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => Some("cdp"),
        ConnectionRequest::UserDataDir { .. } => Some("user_data_dir"),
        ConnectionRequest::Profile { .. } => Some("profile"),
    }
}

pub(super) fn request_needs_live_attachment_resolution(
    current_attachment_identity: Option<&str>,
    request: &ConnectionRequest,
) -> bool {
    matches!(request_attachment_kind(request), Some("cdp"))
        && attachment_identity_kind(current_attachment_identity) == Some("cdp")
}

pub(crate) async fn resolve_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    effective_user_data_dir: Option<&str>,
) -> Result<Option<String>, RubError> {
    match request {
        ConnectionRequest::Profile { resolved_path, .. } => {
            Ok(Some(format!("profile:{resolved_path}")))
        }
        ConnectionRequest::CdpUrl { url } => Ok(Some(format!(
            "cdp:{}",
            rub_cdp::attachment::canonical_external_browser_identity(url).await?
        ))),
        ConnectionRequest::AutoDiscover => {
            let candidate = rub_cdp::attachment::resolve_unique_local_cdp_candidate().await?;
            Ok(Some(format!(
                "cdp:{}",
                rub_cdp::attachment::canonical_external_browser_identity(&candidate.ws_url).await?
            )))
        }
        ConnectionRequest::UserDataDir { path } => Ok(Some(format!(
            "user_data_dir:{}",
            normalize_identity_path(path)
        ))),
        ConnectionRequest::None => {
            let effective_path = effective_user_data_dir
                .map(str::to_string)
                .or_else(|| requested_user_data_dir(cli, request));
            Ok(effective_path
                .as_deref()
                .map(|path| format!("user_data_dir:{}", normalize_identity_path(path))))
        }
    }
}

pub(crate) async fn effective_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    effective_user_data_dir: Option<&str>,
) -> Result<Option<String>, RubError> {
    match request {
        ConnectionRequest::None => Ok(effective_user_data_dir
            .map(|path| format!("user_data_dir:{}", normalize_identity_path(path)))),
        _ => resolve_attachment_identity(cli, request, effective_user_data_dir).await,
    }
}

pub(crate) fn normalize_identity_path(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    if let Ok(canonical) = absolute.canonicalize() {
        return canonical.to_string_lossy().into_owned();
    }

    let mut normalized = if absolute.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized.to_string_lossy().into_owned()
}

pub(crate) fn normalize_cdp_identity(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/').to_string();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if trimmed.ends_with("/json/version") {
            trimmed
        } else {
            format!("{trimmed}/json/version")
        }
    } else {
        trimmed
    }
}
