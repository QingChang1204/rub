use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const REQUEST_HEAD_READ_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_REQUEST_HEAD_BYTES: usize = 8192;

/// Lightweight loopback HTTP fixture that owns deterministic request/response
/// traces for browser-backed network observation tests.
pub struct NetworkInspectionFixtureServer {
    addr: SocketAddr,
    shutdown_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl NetworkInspectionFixtureServer {
    /// Start a loopback HTTP server for request-observation scenarios.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("network fixture bind");
        listener
            .set_nonblocking(true)
            .expect("network fixture nonblocking");
        let addr = listener.local_addr().expect("network fixture local addr");
        let (shutdown_tx, shutdown_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => handle_request(&mut stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    /// Base URL for this fixture authority.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Resolve an absolute fixture URL for a route, accepting either relative
    /// or already rooted paths.
    pub fn url_for(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, normalize_url_path(path))
    }
}

impl Drop for NetworkInspectionFixtureServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_request(stream: &mut TcpStream) {
    // The accept loop stays nonblocking so the fixture can observe shutdown,
    // but individual request streams must block until the client sends a head.
    let _ = stream.set_nonblocking(false);
    let Some(path) = request_path(stream) else {
        return;
    };
    match path.as_str() {
        "/" => write_response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            &[],
            fixture_html().as_bytes(),
        ),
        "/api/orders" => write_response(
            stream,
            "200 OK",
            "application/json",
            &[("X-Fixture-Status", "ok")],
            br#"{"ok":true,"orderId":42}"#,
        ),
        "/api/missing" => write_response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[("X-Fixture-Status", "missing")],
            b"missing-order",
        ),
        "/api/error" => write_response(
            stream,
            "500 Internal Server Error",
            "application/json",
            &[("X-Fixture-Status", "error")],
            br#"{"ok":false,"reason":"boom"}"#,
        ),
        _ => write_response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[],
            b"missing",
        ),
    }
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

fn request_path(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(REQUEST_HEAD_READ_TIMEOUT));
    let mut buf = Vec::new();
    let mut scratch = [0u8; 1024];
    loop {
        match stream.read(&mut scratch) {
            Ok(0) if buf.is_empty() => return None,
            Ok(0) => break,
            Ok(read) => {
                buf.extend_from_slice(&scratch[..read]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n")
                    || buf.windows(2).any(|window| window == b"\n\n")
                    || buf.len() >= MAX_REQUEST_HEAD_BYTES
                {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                if buf.is_empty() {
                    return None;
                }
                break;
            }
            Err(_) => return None,
        }
    }
    Some(
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string(),
    )
}

fn fixture_html() -> &'static str {
    r#"<!DOCTYPE html>
<html>
<head><title>Network Inspection Fixture</title></head>
<body>
  <button id="request-batch" onclick="
    Promise.allSettled([
      fetch('/api/orders', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'x-rub-trace': 'fixture'
        },
        body: JSON.stringify({ orderId: 42 })
      }),
      fetch('/api/missing'),
      fetch('/api/error')
    ]).then(async (results) => {
      const summaries = await Promise.all(results.map(async (result) => {
        if (result.status !== 'fulfilled') {
          return 'rejected';
        }
        return `${result.value.status}:${await result.value.text()}`;
      }));
      document.getElementById('status').textContent = summaries.join('|');
      document.body.dataset.done = '1';
    });
  ">
    Trigger Requests
  </button>
  <div id="status">idle</div>
</body>
</html>"#
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut headers = vec![
        format!("HTTP/1.1 {status}"),
        format!("Content-Type: {content_type}"),
        format!("Content-Length: {}", body.len()),
        "Connection: close".to_string(),
    ];
    for (name, value) in extra_headers {
        headers.push(format!("{name}: {value}"));
    }
    headers.push(String::new());
    headers.push(String::new());

    let _ = stream.write_all(headers.join("\r\n").as_bytes());
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod tests {
    use super::NetworkInspectionFixtureServer;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn get(url: &str, path: &str) -> String {
        let authority = url.trim_start_matches("http://");
        let mut stream = TcpStream::connect(authority).expect("connect fixture server");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    #[test]
    fn fixture_serves_expected_network_observation_routes() {
        let server = NetworkInspectionFixtureServer::start();
        let response = get(&server.url(), "/api/orders");

        assert!(response.contains("200 OK"), "{response}");
        assert!(response.contains("X-Fixture-Status: ok"), "{response}");
        assert!(response.contains("\"orderId\":42"), "{response}");
    }

    #[test]
    fn url_for_normalizes_relative_and_empty_paths() {
        let server = NetworkInspectionFixtureServer::start();

        assert_eq!(server.url_for(""), format!("{}/", server.url()));
        assert_eq!(
            server.url_for("api/orders"),
            format!("{}/api/orders", server.url())
        );
        assert_eq!(
            server.url_for("/api/error"),
            format!("{}/api/error", server.url())
        );
    }

    #[test]
    fn drop_does_not_hang_on_half_open_connection() {
        let server = NetworkInspectionFixtureServer::start();
        let authority = server.url().trim_start_matches("http://").to_string();
        let _stream = TcpStream::connect(&authority).expect("connect fixture server");
        let (done_tx, done_rx) = mpsc::channel();

        thread::spawn(move || {
            drop(server);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fixture shutdown should not hang on half-open connections");
    }
}
