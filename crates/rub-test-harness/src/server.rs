use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;

const MAX_REQUEST_HEAD_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_HEAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REQUEST_BODY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct ParsedRequest {
    head: String,
    _body: Vec<u8>,
}

#[derive(Clone)]
struct RouteDefinition {
    method: Option<String>,
    path: String,
    content_type: &'static str,
    body: &'static str,
}

/// A minimal embedded HTTP server for integration tests.
/// Serves static HTML content on a random local port.
pub struct TestServer {
    addr: SocketAddr,
    shutdown_tx: Option<watch::Sender<bool>>,
    shutdown_done_rx: Option<oneshot::Receiver<()>>,
    server_task: Option<tokio::task::JoinHandle<()>>,
    runtime_handle: tokio::runtime::Handle,
}

impl TestServer {
    /// Start a test server serving the given routes.
    /// Each route is a (path, content_type, body) tuple.
    pub async fn start(routes: Vec<(&'static str, &'static str, &'static str)>) -> Self {
        let routes = routes
            .into_iter()
            .map(|(route, content_type, body)| {
                validate_route_definition(route);
                parse_route_definition(route, content_type, body)
            })
            .collect::<Vec<_>>();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (shutdown_done_tx, shutdown_done_rx) = oneshot::channel();
        let handler_shutdown_tx = shutdown_tx.clone();

        let server_task = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            loop {
                tokio::select! {
                    Ok((mut stream, _)) = listener.accept() => {
                        let routes = routes.clone();
                        let mut handler_shutdown = handler_shutdown_tx.subscribe();
                        handlers.spawn(async move {
                            let request = match read_request(&mut stream, &mut handler_shutdown).await {
                                Ok(Some(request)) => request,
                                Ok(None) | Err(_) => {
                                    return;
                                }
                            };
                            let Some((method, request_target)) = parse_request_line(&request.head) else {
                                return;
                            };
                            let path = normalize_request_target(&request_target);

                            let route = routes
                                .iter()
                                .find(|route| route_matches_definition(route, &method, &path))
                                .map(|route| (route.content_type, route.body));

                            let (status, content_type, body) = match route {
                                Some((content_type, body)) => ("200 OK", content_type, body),
                                None => ("404 Not Found", "text/plain", "404 Not Found"),
                            };
                            let response = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                    changed = shutdown_rx.changed() => {
                        if changed.is_ok() && *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }

            handlers.abort_all();
            while handlers.join_next().await.is_some() {}
            let _ = shutdown_done_tx.send(());
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            shutdown_done_rx: Some(shutdown_done_rx),
            server_task: Some(server_task),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    /// Get the base URL for this test server.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Get the URL for a specific path.
    pub fn url_for(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, normalize_url_path(path))
    }

    pub async fn stop_async(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(rx) = self.shutdown_done_rx.take() {
            let _ = rx.await;
        }
        if let Some(task) = self.server_task.take() {
            let _ = task.await;
        }
    }
}

fn parse_request_line(request: &str) -> Option<(String, String)> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    Some((method, target))
}

fn normalize_request_target(target: &str) -> String {
    target.split('?').next().unwrap_or(target).to_string()
}

fn normalize_url_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn route_matches_definition(route: &RouteDefinition, method: &str, path: &str) -> bool {
    route
        .method
        .as_deref()
        .map(|expected| expected.eq_ignore_ascii_case(method))
        .unwrap_or(true)
        && route.path == path
}

fn validate_route_definition(route: &str) {
    let mut parts = route.split_whitespace();
    let Some(first) = parts.next() else {
        panic!("TestServer route definitions must not be empty");
    };
    let path = parts.next().unwrap_or(first);
    assert!(
        parts.next().is_none(),
        "TestServer routes must be either '<path>' or '<METHOD path>' without extra tokens: {route}"
    );
    assert!(
        !path.contains('?'),
        "TestServer routes must register canonical path-only targets; query strings are matched by the request, not the route definition: {route}"
    );
}

fn parse_route_definition(
    route: &str,
    content_type: &'static str,
    body: &'static str,
) -> RouteDefinition {
    let mut parts = route.split_whitespace();
    let first = parts
        .next()
        .expect("validated route definitions must be non-empty");
    let second = parts.next();
    RouteDefinition {
        method: second.map(|_| first.to_ascii_uppercase()),
        path: normalize_url_path(second.unwrap_or(first)),
        content_type,
        body,
    }
}

async fn read_request<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<ParsedRequest>, std::io::Error> {
    let (head, prefetched_body) = match read_request_head(stream, shutdown).await? {
        Some(request) => request,
        None => return Ok(None),
    };
    let content_length = parse_content_length(&head)?;
    let body = read_request_body(stream, shutdown, content_length, prefetched_body).await?;
    Ok(Some(ParsedRequest { head, _body: body }))
}

async fn read_request_head<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<(String, Vec<u8>)>, std::io::Error> {
    let mut buf = Vec::new();
    let mut scratch = [0u8; 1024];
    loop {
        let n = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "server shutdown interrupted request head read",
                    ));
                }
                continue;
            }
            read = tokio::time::timeout(REQUEST_HEAD_READ_TIMEOUT, stream.read(&mut scratch)) => {
                match read {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "request head timed out before header terminator",
                        ));
                    }
                }
            }
        };
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request head ended before header terminator",
            ));
        }
        buf.extend_from_slice(&scratch[..n]);
        if let Some(header_end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let split_at = header_end + 4;
            let prefetched_body = buf.split_off(split_at);
            let head = String::from_utf8_lossy(&buf).into_owned();
            return Ok(Some((head, prefetched_body)));
        }
        if let Some(header_end) = buf.windows(2).position(|window| window == b"\n\n") {
            let split_at = header_end + 2;
            let prefetched_body = buf.split_off(split_at);
            let head = String::from_utf8_lossy(&buf).into_owned();
            return Ok(Some((head, prefetched_body)));
        }
        if buf.len() >= MAX_REQUEST_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head exceeded maximum size before header terminator",
            ));
        }
    }
}

fn parse_content_length(head: &str) -> Result<usize, std::io::Error> {
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            let content_length = value
                .trim()
                .parse::<usize>()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
            return match content_length {
                Ok(length) if length > MAX_REQUEST_BODY_BYTES => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("request body exceeded maximum size of {MAX_REQUEST_BODY_BYTES} bytes"),
                )),
                Ok(length) => Ok(length),
                Err(error) => Err(error),
            };
        }
    }
    Ok(0)
}

async fn read_request_body<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    shutdown: &mut watch::Receiver<bool>,
    content_length: usize,
    prefetched_body: Vec<u8>,
) -> Result<Vec<u8>, std::io::Error> {
    if content_length == 0 {
        return Ok(Vec::new());
    }
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("request body exceeded maximum size of {MAX_REQUEST_BODY_BYTES} bytes"),
        ));
    }

    let mut body = vec![0u8; content_length];
    let prefetched = prefetched_body.len().min(content_length);
    body[..prefetched].copy_from_slice(&prefetched_body[..prefetched]);
    let mut read = prefetched;
    while read < content_length {
        let n = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "server shutdown interrupted request body read",
                    ));
                }
                continue;
            }
            result = tokio::time::timeout(
                REQUEST_BODY_READ_TIMEOUT,
                stream.read(&mut body[read..]),
            ) => {
                match result {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "request body timed out before declared content-length was read",
                        ));
                    }
                }
            }
        };
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request body ended before declared content-length was read",
            ));
        }
        read += n;
    }
    Ok(body)
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        let shutdown_done_rx = self.shutdown_done_rx.take();
        let server_task = self.server_task.take();
        if shutdown_done_rx.is_none() && server_task.is_none() {
            return;
        }
        let handle = self.runtime_handle.clone();

        if tokio::runtime::Handle::try_current().is_ok() {
            let waiter = std::thread::spawn(move || {
                handle.block_on(async move {
                    if let Some(rx) = shutdown_done_rx {
                        let _ = rx.await;
                    }
                    if let Some(task) = server_task {
                        let _ = task.await;
                    }
                });
            });
            let _ = waiter.join();
        } else {
            handle.block_on(async move {
                if let Some(rx) = shutdown_done_rx {
                    let _ = rx.await;
                }
                if let Some(task) = server_task {
                    let _ = task.await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_REQUEST_BODY_BYTES, TestServer, parse_content_length};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn matched_route_body_does_not_force_404_status() {
        let server = TestServer::start(vec![("/ok", "text/plain", "404 Not Found")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        stream
            .write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.stop_async().await;
    }

    #[tokio::test]
    async fn server_reads_full_request_head_before_matching_path() {
        let server = TestServer::start(vec![("/long", "text/plain", "ok")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        let request = format!(
            "GET /long HTTP/1.1\r\nHost: localhost\r\nX-Long: {}\r\n\r\n",
            "a".repeat(5000)
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.stop_async().await;
    }

    #[tokio::test]
    async fn stop_async_cancels_half_open_connections() {
        let server = TestServer::start(vec![("/ok", "text/plain", "ok")]).await;
        let _stream = TcpStream::connect(server.addr).await.expect("connect");

        tokio::time::timeout(std::time::Duration::from_secs(1), server.stop_async())
            .await
            .expect("shutdown should not hang on half-open connections");
    }

    #[tokio::test]
    async fn route_matching_honors_method_and_ignores_query_string() {
        let server = TestServer::start(vec![("POST /submit", "text/plain", "posted")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        stream
            .write_all(b"GET /submit?draft=1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write get request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");

        let mut post_stream = TcpStream::connect(server.addr).await.expect("connect post");
        post_stream
            .write_all(b"POST /submit?draft=1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write post request");
        let mut post_response = Vec::new();
        post_stream
            .read_to_end(&mut post_response)
            .await
            .expect("read post response");
        let post_response = String::from_utf8_lossy(&post_response);
        assert!(
            post_response.starts_with("HTTP/1.1 200 OK"),
            "{post_response}"
        );
        server.stop_async().await;
    }

    #[tokio::test]
    async fn server_consumes_declared_request_body_before_responding() {
        let server = TestServer::start(vec![("POST /upload", "text/plain", "ok")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        let request = b"POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\nping";
        stream.write_all(request).await.expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.stop_async().await;
    }

    #[tokio::test]
    async fn lf_only_request_head_preserves_prefetched_body_bytes() {
        let server = TestServer::start(vec![("POST /upload", "text/plain", "ok")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        let request = b"POST /upload HTTP/1.1\nHost: localhost\nContent-Length: 4\n\nping";
        stream.write_all(request).await.expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.stop_async().await;
    }

    #[tokio::test]
    #[should_panic(expected = "canonical path-only targets")]
    async fn query_routes_are_rejected_at_registration() {
        let _server = TestServer::start(vec![("/submit?from=test", "text/plain", "posted")]).await;
    }

    #[tokio::test]
    #[should_panic(expected = "without extra tokens")]
    async fn extra_route_tokens_are_rejected_at_registration() {
        let _server = TestServer::start(vec![("GET /submit extra", "text/plain", "posted")]).await;
    }

    #[tokio::test]
    async fn url_for_normalizes_relative_paths() {
        let server = TestServer::start(vec![("/page", "text/plain", "ok")]).await;
        assert_eq!(server.url_for("page"), server.url_for("/page"));
        assert_eq!(server.url_for(""), format!("{}/", server.url()));
        server.stop_async().await;
    }

    #[tokio::test]
    async fn slashless_route_registration_shares_canonical_path_authority_with_url_for() {
        let server = TestServer::start(vec![("page", "text/plain", "ok")]).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        stream
            .write_all(b"GET /page HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.stop_async().await;
    }

    #[test]
    fn oversized_content_length_is_rejected_before_allocation() {
        let head = format!(
            "POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            MAX_REQUEST_BODY_BYTES + 1
        );
        let error =
            parse_content_length(&head).expect_err("oversized request body must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("request body exceeded maximum size")
        );
    }
}
