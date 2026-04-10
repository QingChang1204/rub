use super::projection::{cookie_artifact, cookie_payload, cookie_subject, cookies_subject};
use super::*;
use crate::router::request_args::parse_json_args;
use rub_core::fs::atomic_write_bytes;
use rub_core::model::Cookie;

#[derive(Clone, Copy, Debug)]
enum CookieAction {
    Get,
    Set,
    Clear,
    Export,
    Import,
}

impl CookieAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        let sub = args.get("sub").and_then(|v| v.as_str()).ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "cookies requires a subcommand: get, set, clear, export, import",
            )
        })?;
        match sub {
            "get" => Ok(Self::Get),
            "set" => Ok(Self::Set),
            "clear" => Ok(Self::Clear),
            "export" => Ok(Self::Export),
            "import" => Ok(Self::Import),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Unknown cookies subcommand '{other}'. Valid: get, set, clear, export, import"
                ),
            )),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CookiesUrlArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CookiesPathArgs {
    #[serde(rename = "sub")]
    _sub: String,
    pub(super) path: String,
    #[serde(default, rename = "path_state")]
    _path_state: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CookieSetArgs {
    #[serde(rename = "sub")]
    _sub: String,
    name: String,
    value: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default = "default_cookie_path")]
    path: String,
    #[serde(default)]
    secure: bool,
    #[serde(default)]
    http_only: bool,
    #[serde(default = "default_cookie_same_site")]
    same_site: String,
    #[serde(default)]
    expires: Option<f64>,
}

pub(super) async fn cmd_cookies(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    match CookieAction::parse(args)? {
        CookieAction::Get => {
            let parsed = parse_json_args::<CookiesUrlArgs>(args, "cookies get")?;
            let cookies = router.browser.get_cookies(parsed.url.as_deref()).await?;
            Ok(cookie_payload(
                cookies_subject(parsed.url.as_deref()),
                serde_json::json!({
                    "cookies": cookies,
                }),
                None,
            ))
        }
        CookieAction::Set => {
            let parsed = parse_json_args::<CookieSetArgs>(args, "cookies set")?;
            if !matches!(
                parsed.same_site.as_str(),
                "Strict" | "strict" | "Lax" | "lax" | "None" | "none"
            ) {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    format!(
                        "Invalid sameSite value '{}'. Valid: Strict, Lax, None",
                        parsed.same_site
                    ),
                ));
            }
            let cookie = Cookie {
                name: parsed.name,
                value: parsed.value,
                domain: parsed.domain.unwrap_or_default(),
                path: parsed.path,
                secure: parsed.secure,
                http_only: parsed.http_only,
                same_site: parsed.same_site,
                expires: parsed.expires,
            };
            router.browser.set_cookie(&cookie).await?;
            Ok(cookie_payload(
                cookie_subject(&cookie),
                serde_json::json!({
                    "cookie": cookie,
                }),
                None,
            ))
        }
        CookieAction::Clear => {
            let parsed = parse_json_args::<CookiesUrlArgs>(args, "cookies clear")?;
            router.browser.delete_cookies(parsed.url.as_deref()).await?;
            Ok(cookie_payload(
                cookies_subject(parsed.url.as_deref()),
                serde_json::json!({
                    "cleared": true,
                }),
                None,
            ))
        }
        CookieAction::Export => {
            let parsed = parse_json_args::<CookiesPathArgs>(args, "cookies export")?;
            let cookies = router.browser.get_cookies(None).await?;
            let json = serde_json::to_string_pretty(&cookies)
                .map_err(|e| RubError::Internal(format!("Serialize cookies failed: {e}")))?;
            let commit_outcome =
                atomic_write_bytes(std::path::Path::new(&parsed.path), json.as_bytes(), 0o600)
                    .map_err(|e| RubError::Internal(format!("Cannot write file: {e}")))?;

            Ok(cookie_payload(
                cookies_subject(None),
                serde_json::json!({
                    "count": cookies.len(),
                }),
                Some(cookie_artifact(
                    &parsed.path,
                    "output",
                    output_artifact_durability(commit_outcome),
                )),
            ))
        }
        CookieAction::Import => {
            let parsed = parse_json_args::<CookiesPathArgs>(args, "cookies import")?;
            let data = std::fs::read_to_string(&parsed.path).map_err(|e| {
                RubError::domain(ErrorCode::FileNotFound, format!("Cannot read file: {e}"))
            })?;
            let cookies: Vec<Cookie> = serde_json::from_str(&data).map_err(|e| {
                RubError::domain(ErrorCode::InvalidInput, format!("Invalid JSON: {e}"))
            })?;
            let previous_cookies = router.browser.get_cookies(None).await?;
            let count = cookies.len();
            for (index, cookie) in cookies.iter().enumerate() {
                if let Err(error) = router.browser.set_cookie(cookie).await {
                    let rollback = restore_cookie_batch(router, &previous_cookies).await;
                    return Err(cookie_import_error(
                        &parsed.path,
                        index,
                        error,
                        rollback.err(),
                    ));
                }
            }
            Ok(cookie_payload(
                cookies_subject(None),
                serde_json::json!({
                    "imported": count,
                }),
                Some(cookie_artifact(
                    &parsed.path,
                    "input",
                    INPUT_ARTIFACT_DURABILITY,
                )),
            ))
        }
    }
}

fn default_cookie_path() -> String {
    "/".to_string()
}

fn default_cookie_same_site() -> String {
    "Lax".to_string()
}

async fn restore_cookie_batch(router: &DaemonRouter, cookies: &[Cookie]) -> Result<(), RubError> {
    router.browser.delete_cookies(None).await?;
    for cookie in cookies {
        router.browser.set_cookie(cookie).await?;
    }
    Ok(())
}

fn cookie_import_error(
    path: &str,
    index: usize,
    import_error: RubError,
    rollback_error: Option<RubError>,
) -> RubError {
    match rollback_error {
        Some(rollback_error) => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cookie import failed at index {index}: {import_error}"),
            serde_json::json!({
                "path": path,
                "cookie_index": index,
                "rollback_failed": true,
                "rollback_error": rollback_error.into_envelope(),
            }),
        ),
        None => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cookie import failed at index {index}: {import_error}"),
            serde_json::json!({
                "path": path,
                "cookie_index": index,
                "rollback_failed": false,
            }),
        ),
    }
}
