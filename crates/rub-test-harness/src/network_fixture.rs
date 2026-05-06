use crate::http_fixture::{local_http_origin, normalize_url_path, request_path, write_response};

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
        local_http_origin(self.addr)
    }

    /// Resolve an absolute fixture URL for a route, accepting either relative
    /// or already rooted paths.
    pub fn url_for(&self, path: &str) -> String {
        format!("{}{}", self.url(), normalize_url_path(path))
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

#[cfg(test)]
mod tests {
    use super::NetworkInspectionFixtureServer;
    use crate::http_fixture::{get, http_authority};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

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
        let authority = http_authority(&server.url()).to_string();
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
