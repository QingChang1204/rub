pub(super) fn normalize_open_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") {
        return trimmed.to_string();
    }

    let authority = trimmed
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    let host = authority
        .split_once(':')
        .map(|(host, _)| host)
        .unwrap_or(authority);

    if host.is_empty() || host.contains(char::is_whitespace) {
        return trimmed.to_string();
    }

    if is_loopback_host(host) {
        return format!("http://{trimmed}");
    }

    if looks_like_web_host(host) {
        return format!("https://{trimmed}");
    }

    if has_explicit_scheme(trimmed) {
        return trimmed.to_string();
    }

    trimmed.to_string()
}

fn has_explicit_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let prefix = &value[..colon];
    !prefix.is_empty()
        && !prefix.contains('.')
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]") || host == "::1"
}

fn looks_like_web_host(host: &str) -> bool {
    host.contains('.')
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::normalize_open_url;

    #[test]
    fn prefixes_https_for_bare_domains() {
        assert_eq!(normalize_open_url("example.com"), "https://example.com");
        assert_eq!(
            normalize_open_url("example.com/path?q=1"),
            "https://example.com/path?q=1"
        );
        assert_eq!(
            normalize_open_url("example.com:8443/path"),
            "https://example.com:8443/path"
        );
    }

    #[test]
    fn prefixes_http_for_local_hosts() {
        assert_eq!(
            normalize_open_url("localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_open_url("127.0.0.1:9222/json/version"),
            "http://127.0.0.1:9222/json/version"
        );
    }

    #[test]
    fn preserves_explicit_schemes() {
        assert_eq!(
            normalize_open_url("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_open_url("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(normalize_open_url("about:blank"), "about:blank");
        assert_eq!(
            normalize_open_url("data:text/html,<h1>hi</h1>"),
            "data:text/html,<h1>hi</h1>"
        );
        assert_eq!(
            normalize_open_url("file:///tmp/example.html"),
            "file:///tmp/example.html"
        );
        assert_eq!(normalize_open_url("chrome://version"), "chrome://version");
    }
}
