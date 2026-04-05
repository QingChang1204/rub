use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{
    CookieSameSite, DeleteCookiesParams, GetCookiesParams, SetCookieParams, TimeSinceEpoch,
};
use rub_core::error::RubError;
use rub_core::model::Cookie;
use std::sync::Arc;

pub(crate) async fn get(page: &Arc<Page>, url: Option<&str>) -> Result<Vec<Cookie>, RubError> {
    let params = if let Some(url) = url {
        GetCookiesParams::builder().url(url).build()
    } else {
        GetCookiesParams::default()
    };
    let response = page
        .execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("GetCookies failed: {e}")))?;

    let cookies = response
        .result
        .cookies
        .into_iter()
        .map(|c| Cookie {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            secure: c.secure,
            http_only: c.http_only,
            same_site: c
                .same_site
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "None".to_string()),
            expires: if c.expires > 0.0 {
                Some(c.expires)
            } else {
                None
            },
        })
        .collect();
    Ok(cookies)
}

pub(crate) async fn set(page: &Arc<Page>, cookie: &Cookie) -> Result<(), RubError> {
    let mut params = SetCookieParams::builder()
        .name(&cookie.name)
        .value(&cookie.value);
    if let Some(url) = page.url().await.ok().flatten().map(|url| url.to_string()) {
        params = params.url(url);
    }
    if !cookie.domain.is_empty() {
        params = params.domain(&cookie.domain);
    }
    if !cookie.path.is_empty() {
        params = params.path(&cookie.path);
    }
    if cookie.secure {
        params = params.secure(true);
    }
    if cookie.http_only {
        params = params.http_only(true);
    }
    if let Some(same_site) = parse_same_site(&cookie.same_site) {
        params = params.same_site(same_site);
    }
    if let Some(expires) = cookie.expires {
        params = params.expires(TimeSinceEpoch::new(expires));
    }
    let params = params
        .build()
        .map_err(|e| RubError::Internal(format!("Build SetCookie failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("SetCookie failed: {e}")))?;
    Ok(())
}

pub(crate) async fn delete(page: &Arc<Page>, url: Option<&str>) -> Result<(), RubError> {
    let get_params = if let Some(url) = url {
        GetCookiesParams::builder().url(url).build()
    } else {
        GetCookiesParams::default()
    };
    let cookies = page
        .execute(get_params)
        .await
        .map_err(|e| RubError::Internal(format!("GetCookies failed: {e}")))?
        .result
        .cookies;

    for c in cookies {
        let mut params = DeleteCookiesParams::builder().name(&c.name);
        if !c.domain.is_empty() {
            params = params.domain(&c.domain);
        }
        if !c.path.is_empty() {
            params = params.path(&c.path);
        }
        let params = params
            .build()
            .map_err(|e| RubError::Internal(format!("Build DeleteCookies failed: {e}")))?;
        page.execute(params)
            .await
            .map_err(|e| RubError::Internal(format!("DeleteCookies failed: {e}")))?;
    }
    Ok(())
}

fn parse_same_site(value: &str) -> Option<CookieSameSite> {
    match value {
        "Strict" | "strict" => Some(CookieSameSite::Strict),
        "Lax" | "lax" => Some(CookieSameSite::Lax),
        "None" | "none" => Some(CookieSameSite::None),
        _ => None,
    }
}
