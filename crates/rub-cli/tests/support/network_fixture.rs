use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct NetworkInspectionFixtureServer {
    addr: SocketAddr,
    shutdown_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl NetworkInspectionFixtureServer {
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

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

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
    let path = request_path(stream);
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

fn request_path(stream: &mut TcpStream) -> String {
    let mut buf = vec![0u8; 8192];
    let read = stream.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..read])
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string()
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
