//! External CDP attachment and local discovery helpers.

use chromiumoxide::browser::Browser;
use rub_core::error::{ErrorCode, RubError};
use tokio::time::{Duration, Instant, sleep};

use std::collections::HashMap;

const DISCOVERY_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// A candidate CDP endpoint discovered on the local machine.
#[derive(Debug, Clone)]
pub struct CdpCandidate {
    pub port: u16,
    pub url: String,
    pub ws_url: String,
    pub browser_version: String,
}

pub fn normalize_external_connect_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub async fn canonical_external_browser_identity(url: &str) -> Result<String, RubError> {
    canonical_external_browser_identity_until(url, Instant::now() + DISCOVERY_HTTP_TIMEOUT).await
}

pub async fn canonical_external_browser_identity_until(
    url: &str,
    deadline: Instant,
) -> Result<String, RubError> {
    let connect_url = resolve_cdp_connect_url_until(url, deadline).await?;
    Ok(normalize_external_connect_url(&connect_url))
}

pub async fn resolve_unique_local_cdp_candidate() -> Result<CdpCandidate, RubError> {
    resolve_unique_local_cdp_candidate_until(Instant::now() + DISCOVERY_HTTP_TIMEOUT).await
}

pub async fn resolve_unique_local_cdp_candidate_until(
    deadline: Instant,
) -> Result<CdpCandidate, RubError> {
    let candidates = discover_local_cdp_until(deadline).await?;
    match candidates.len() {
        0 => Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            "No local Chrome with CDP enabled found on ports 9222-9229",
        )),
        1 => Ok(candidates.into_iter().next().expect("one candidate")),
        count => Err(RubError::domain(
            ErrorCode::CdpConnectionAmbiguous,
            format!(
                "Found {count} Chrome instances. Use --cdp-url with a specific endpoint: {}",
                candidates
                    .iter()
                    .map(|candidate| {
                        format!("port {} ({})", candidate.port, candidate.browser_version)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

pub async fn connect_external_browser(
    url: &str,
) -> Result<(Browser, chromiumoxide::handler::Handler, String), RubError> {
    connect_external_browser_until(url, Instant::now() + Duration::from_secs(15)).await
}

pub(crate) async fn connect_external_browser_until(
    url: &str,
    deadline: Instant,
) -> Result<(Browser, chromiumoxide::handler::Handler, String), RubError> {
    loop {
        let mut candidate_errors = Vec::new();

        let connect_url = if url.starts_with("http://") {
            let remaining = attach_remaining_budget(deadline)?.min(DISCOVERY_HTTP_TIMEOUT);
            match resolve_cdp_connect_url_with_timeout(url, remaining).await {
                Ok(connect_url) => normalize_external_connect_url(&connect_url),
                Err(error) => {
                    candidate_errors.push(error.into_envelope().message);
                    if Instant::now() >= deadline {
                        return Err(RubError::domain(
                            ErrorCode::CdpConnectionFailed,
                            format!(
                                "Failed to connect to external browser at {url}: {}",
                                candidate_errors.join("; ")
                            ),
                        ));
                    }
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
            }
        } else {
            normalize_external_connect_url(url)
        };

        match tokio::time::timeout(
            attach_remaining_budget(deadline)?,
            Browser::connect(&connect_url),
        )
        .await
        {
            Ok(Ok((browser, handler))) => return Ok((browser, handler, connect_url)),
            Ok(Err(error)) => {
                candidate_errors.push(format!("{connect_url}: {error}"));
            }
            Err(_) => {
                candidate_errors.push(format!("{connect_url}: timed out during Browser::connect"));
            }
        }

        if url.starts_with("http://") && connect_url != url {
            match tokio::time::timeout(attach_remaining_budget(deadline)?, Browser::connect(url))
                .await
            {
                Ok(Ok((browser, handler))) => return Ok((browser, handler, connect_url)),
                Ok(Err(error)) => {
                    candidate_errors.push(format!("{url}: {error}"));
                }
                Err(_) => {
                    candidate_errors.push(format!("{url}: timed out during Browser::connect"));
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!(
                    "Failed to connect to external browser at {url}: {}",
                    candidate_errors.join("; ")
                ),
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Scan local ports 9222-9229 for Chrome instances with CDP enabled.
///
/// Returns all discovered candidates. Use with `--connect` (auto-discover)
/// or to populate `rub doctor` output.
pub async fn discover_local_cdp() -> Vec<CdpCandidate> {
    discover_local_cdp_until(Instant::now() + DISCOVERY_HTTP_TIMEOUT)
        .await
        .unwrap_or_default()
}

async fn discover_local_cdp_until(deadline: Instant) -> Result<Vec<CdpCandidate>, RubError> {
    let mut candidates = Vec::new();

    for port in 9222..=9229 {
        let remaining = attach_remaining_budget(deadline)?;
        let url = format!("http://127.0.0.1:{port}/json/version");
        let timeout_result = http_get_text_until(
            &url,
            Instant::now() + remaining.min(Duration::from_millis(200)),
        )
        .await;

        if let Ok(body) = timeout_result
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(ws_url) = json["webSocketDebuggerUrl"].as_str()
        {
            candidates.push(CdpCandidate {
                port,
                url: format!("http://127.0.0.1:{port}"),
                ws_url: ws_url.to_string(),
                browser_version: json["Browser"].as_str().unwrap_or("unknown").to_string(),
            });
        }
    }

    Ok(candidates)
}

async fn http_get_text_with_timeout(
    url: &str,
    timeout_budget: Duration,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    http_get_text_until(url, Instant::now() + timeout_budget).await
}

fn attach_remaining_budget(deadline: Instant) -> Result<Duration, RubError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            "Failed to connect to external browser: deadline exhausted",
        ));
    }
    Ok(remaining)
}

fn remaining_budget(
    deadline: Instant,
) -> Result<Duration, Box<dyn std::error::Error + Send + Sync>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "HTTP request deadline exhausted",
        )));
    }
    Ok(remaining)
}

async fn http_get_text_until(
    url: &str,
    deadline: Instant,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use std::io::{self, ErrorKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let url_no_scheme = url.strip_prefix("http://").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "URL must start with http://")
    })?;
    let (host_port, path) = url_no_scheme.split_once('/').unwrap_or((url_no_scheme, ""));
    let path = format!("/{path}");

    let mut stream =
        tokio::time::timeout(remaining_budget(deadline)?, TcpStream::connect(host_port))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP connect timed out"))??;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    tokio::time::timeout(
        remaining_budget(deadline)?,
        stream.write_all(request.as_bytes()),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP request write timed out"))??;

    let mut response = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        let read = tokio::time::timeout(remaining_budget(deadline)?, stream.read(&mut chunk))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "HTTP response read timed out")
            })??;

        let eof_reached = read == 0;
        if !eof_reached {
            response.extend_from_slice(&chunk[..read]);
        }
        match parse_http_response_body(&response, eof_reached) {
            Ok(body) => return String::from_utf8(body).map_err(Into::into),
            Err(error)
                if matches!(
                    error.downcast_ref::<io::Error>().map(io::Error::kind),
                    Some(ErrorKind::UnexpectedEof | ErrorKind::InvalidData)
                ) => {}
            Err(error) => return Err(error),
        }
        if eof_reached {
            break;
        }
    }

    let body = parse_http_response_body(&response, true)?;
    String::from_utf8(body).map_err(Into::into)
}

#[cfg(test)]
async fn resolve_cdp_connect_url(url: &str) -> Result<String, RubError> {
    resolve_cdp_connect_url_with_timeout(url, DISCOVERY_HTTP_TIMEOUT).await
}

async fn resolve_cdp_connect_url_with_timeout(
    url: &str,
    timeout_budget: Duration,
) -> Result<String, RubError> {
    resolve_cdp_connect_url_until(url, Instant::now() + timeout_budget).await
}

async fn resolve_cdp_connect_url_until(url: &str, deadline: Instant) -> Result<String, RubError> {
    if url.starts_with("ws://") || url.starts_with("wss://") {
        return Ok(url.to_string());
    }

    if !url.starts_with("http://") {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("Unsupported CDP URL scheme for '{url}'. Use http:// or ws://"),
        ));
    }

    let discovery_url = cdp_discovery_endpoint(url);
    let remaining = attach_remaining_budget(deadline)?;
    resolve_cdp_connect_url_once(&discovery_url, remaining).await
}

async fn resolve_cdp_connect_url_once(
    discovery_url: &str,
    timeout_budget: Duration,
) -> Result<String, RubError> {
    let body = http_get_text_with_timeout(discovery_url, timeout_budget)
        .await
        .map_err(|e| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!("Failed to query CDP discovery endpoint {discovery_url}: {e}"),
            )
        })?;
    let json = serde_json::from_str::<serde_json::Value>(&body).map_err(|e| {
        RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("Invalid JSON from CDP discovery endpoint {discovery_url}: {e}"),
        )
    })?;
    json["webSocketDebuggerUrl"]
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!(
                    "CDP discovery endpoint {discovery_url} did not return webSocketDebuggerUrl"
                ),
            )
        })
}

fn cdp_discovery_endpoint(url: &str) -> String {
    if url.ends_with("/json/version") {
        url.to_string()
    } else {
        format!("{}/json/version", url.trim_end_matches('/'))
    }
}

fn parse_http_response_body(
    response: &[u8],
    eof_reached: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use std::io;

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Malformed HTTP response"))?;
    let header_bytes = &response[..header_end];
    let body = &response[header_end + 4..];
    let header_text = std::str::from_utf8(header_bytes)?;
    let mut lines = header_text.split("\r\n");

    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing HTTP status line"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing HTTP status code"))?
        .parse::<u16>()?;
    if status_code != 200 {
        return Err(io::Error::other(format!("Unexpected HTTP status {status_code}")).into());
    }

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    if headers.get("transfer-encoding").is_some_and(|value| {
        value
            .split(',')
            .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
    }) {
        return decode_chunked_body(body);
    }

    if let Some(length) = headers.get("content-length") {
        let expected_len = length.parse::<usize>().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid Content-Length header: {e}"),
            )
        })?;
        if body.len() < expected_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "HTTP body shorter than Content-Length: expected {expected_len}, got {}",
                    body.len()
                ),
            )
            .into());
        }
        return Ok(body[..expected_len].to_vec());
    }

    if !eof_reached {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Close-delimited HTTP body not committed before EOF",
        )
        .into());
    }

    Ok(body.to_vec())
}

fn decode_chunked_body(
    mut body: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use std::io;

    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Malformed chunked body"))?;
        let size_line = std::str::from_utf8(&body[..line_end])?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let chunk_size = usize::from_str_radix(size_hex, 16).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid chunk size: {e}"),
            )
        })?;
        body = &body[line_end + 2..];

        if chunk_size == 0 {
            return Ok(decoded);
        }

        if body.len() < chunk_size + 2 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Chunked body ended before declared chunk size",
            )
            .into());
        }

        decoded.extend_from_slice(&body[..chunk_size]);
        if &body[chunk_size..chunk_size + 2] != b"\r\n" {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "Chunk missing trailing CRLF").into(),
            );
        }
        body = &body[chunk_size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_external_browser_identity, cdp_discovery_endpoint, decode_chunked_body,
        http_get_text_until, parse_http_response_body, resolve_cdp_connect_url,
        resolve_cdp_connect_url_with_timeout,
    };
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, Instant};

    #[test]
    fn parse_http_response_body_accepts_content_length() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"Browser\":\"x\"}";
        let body = parse_http_response_body(response, false).unwrap();
        assert_eq!(body, br#"{"Browser":"x"}"#);
    }

    #[test]
    fn parse_http_response_body_rejects_non_200_status() {
        let response = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}";
        let error = parse_http_response_body(response, true).unwrap_err();
        assert!(error.to_string().contains("Unexpected HTTP status 404"));
    }

    #[test]
    fn parse_http_response_body_requires_eof_for_close_delimited_bodies() {
        let response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"Browser\":\"x\"}";
        let error = parse_http_response_body(response, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Close-delimited HTTP body not committed before EOF")
        );
        let body = parse_http_response_body(response, true).unwrap();
        assert_eq!(body, br#"{"Browser":"x"}"#);
    }

    #[test]
    fn decode_chunked_body_supports_simple_payload() {
        let decoded = decode_chunked_body(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap();
        assert_eq!(decoded, b"Wikipedia");
    }

    #[test]
    fn cdp_discovery_endpoint_appends_json_version_for_origins() {
        assert_eq!(
            cdp_discovery_endpoint("http://127.0.0.1:9222"),
            "http://127.0.0.1:9222/json/version"
        );
        assert_eq!(
            cdp_discovery_endpoint("http://127.0.0.1:9222/json/version"),
            "http://127.0.0.1:9222/json/version"
        );
    }

    #[tokio::test]
    async fn resolve_cdp_connect_url_keeps_websocket_urls() {
        let ws_url = "ws://127.0.0.1:9222/devtools/browser/test";
        assert_eq!(resolve_cdp_connect_url(ws_url).await.unwrap(), ws_url);
    }

    #[tokio::test]
    async fn resolve_cdp_connect_url_times_out_when_endpoint_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let error = resolve_cdp_connect_url_with_timeout(
            &format!("http://{address}"),
            Duration::from_millis(50),
        )
        .await
        .expect_err("stalled endpoint should time out");
        assert!(
            error
                .to_string()
                .contains("Failed to query CDP discovery endpoint"),
            "{error}"
        );

        server.await.expect("server join");
    }

    #[tokio::test]
    async fn http_get_text_until_accepts_complete_response_without_waiting_for_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 512];
            let _ = stream.readable().await;
            let _ = stream.try_read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"Browser\":\"x\"}")
                .await
                .expect("write response");
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let body = http_get_text_until(
            &format!("http://{address}/json/version"),
            Instant::now() + Duration::from_millis(100),
        )
        .await
        .expect("complete HTTP response should not require EOF");
        assert_eq!(body, r#"{"Browser":"x"}"#);

        server.await.expect("server join");
    }

    #[tokio::test]
    async fn http_get_text_until_waits_for_eof_for_close_delimited_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 512];
            let _ = stream.readable().await;
            let _ = stream.try_read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"Browser\":\"x\"}")
                .await
                .expect("write response");
            tokio::time::sleep(Duration::from_millis(75)).await;
        });

        let body = http_get_text_until(
            &format!("http://{address}/json/version"),
            Instant::now() + Duration::from_millis(250),
        )
        .await
        .expect("close-delimited HTTP response should commit at EOF");
        assert_eq!(body, r#"{"Browser":"x"}"#);

        server.await.expect("server join");
    }

    #[tokio::test]
    async fn canonical_external_browser_identity_converges_http_and_ws_inputs() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let ws_url = format!("ws://{address}/devtools/browser/test");
        let server = tokio::spawn({
            let ws_url = ws_url.clone();
            async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.expect("accept");
                    let mut request = [0u8; 1024];
                    let _ = stream.read(&mut request).await.expect("read request");
                    let body = format!(r#"{{"webSocketDebuggerUrl":"{ws_url}"}}"#);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .await
                        .expect("write response");
                }
            }
        });

        let http_origin = format!("http://{address}");
        let http_discovery = format!("{http_origin}/json/version");
        assert_eq!(
            canonical_external_browser_identity(&http_origin)
                .await
                .expect("canonical http origin"),
            ws_url
        );
        assert_eq!(
            canonical_external_browser_identity(&http_discovery)
                .await
                .expect("canonical discovery endpoint"),
            ws_url
        );
        assert_eq!(
            canonical_external_browser_identity(&ws_url)
                .await
                .expect("canonical websocket endpoint"),
            ws_url
        );

        server.await.expect("server join");
    }
}
