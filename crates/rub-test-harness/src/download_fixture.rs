use crate::http_fixture::{local_http_origin, request_path, write_response};

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const STREAM_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// Lightweight loopback HTTP fixture that serves deterministic browser-backed
/// download scenarios for E2E tests.
pub struct DownloadFixtureServer {
    addr: SocketAddr,
    shutdown_tx: Option<mpsc::Sender<()>>,
    shutdown_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DownloadFixtureServer {
    /// Start a loopback HTTP server that owns deterministic download fixtures
    /// for the lifetime of this value.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("download fixture bind");
        listener
            .set_nonblocking(true)
            .expect("download fixture nonblocking");
        let addr = listener.local_addr().expect("download fixture local addr");
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_loop_flag = Arc::clone(&shutdown_flag);

        let handle = thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => handle_request(&mut stream, &shutdown_loop_flag),
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
            shutdown_flag,
            handle: Some(handle),
        }
    }

    /// Base URL for this fixture authority.
    pub fn url(&self) -> String {
        local_http_origin(self.addr)
    }
}

impl Drop for DownloadFixtureServer {
    fn drop(&mut self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_request(stream: &mut TcpStream, shutdown_flag: &Arc<AtomicBool>) {
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
        "/fast.csv" => write_response(
            stream,
            "200 OK",
            "application/octet-stream",
            &[("Content-Disposition", "attachment; filename=\"report.csv\"")],
            b"id,name\n1,Ada Lovelace\n",
        ),
        "/slow.csv" => write_streaming_attachment(stream, "slow-report.csv", shutdown_flag),
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
<head><title>Download Fixture</title></head>
<body>
  <a id="download-fast"
     href="/fast.csv"
     download="report.csv"
     onclick="document.body.dataset.fastDownload='started'">
    Download Fast Report
  </a>
  <a id="download-slow"
     href="/slow.csv"
     download="slow-report.csv"
     onclick="document.body.dataset.slowDownload='started'">
    Download Slow Report
  </a>
</body>
</html>"#
}

fn write_streaming_attachment(
    stream: &mut TcpStream,
    filename: &str,
    shutdown_flag: &Arc<AtomicBool>,
) {
    const CHUNK_SIZE: usize = 16 * 1024;
    const CHUNK_COUNT: usize = 32;
    const TOTAL_BYTES: usize = CHUNK_SIZE * CHUNK_COUNT;

    let _ = stream.set_write_timeout(Some(STREAM_WRITE_TIMEOUT));
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\nContent-Length: {TOTAL_BYTES}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }

    let chunk = vec![b'Z'; CHUNK_SIZE];
    for _ in 0..CHUNK_COUNT {
        if shutdown_flag.load(Ordering::SeqCst) {
            return;
        }
        if stream.write_all(&chunk).is_err() {
            return;
        }
        let _ = stream.flush();
        thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(test)]
mod tests {
    use super::DownloadFixtureServer;
    use crate::http_fixture::{get, http_authority};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn fixture_serves_fast_download_attachment() {
        let server = DownloadFixtureServer::start();
        let response = get(&server.url(), "/fast.csv");

        assert!(response.contains("200 OK"), "{response}");
        assert!(response.contains("Content-Disposition: attachment; filename=\"report.csv\""));
        assert!(response.contains("Ada Lovelace"), "{response}");
    }

    #[test]
    fn fixture_serves_download_landing_page() {
        let server = DownloadFixtureServer::start();
        let response = get(&server.url(), "/");

        assert!(response.contains("200 OK"), "{response}");
        assert!(response.contains("id=\"download-fast\""), "{response}");
        assert!(response.contains("id=\"download-slow\""), "{response}");
    }

    #[test]
    fn drop_does_not_hang_on_half_open_connection() {
        let server = DownloadFixtureServer::start();
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

    #[test]
    fn drop_does_not_hang_on_slow_stream_connection() {
        let server = DownloadFixtureServer::start();
        let authority = http_authority(&server.url()).to_string();
        let mut stream = TcpStream::connect(&authority).expect("connect fixture server");
        write!(
            stream,
            "GET /slow.csv HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");

        let mut header_buf = [0u8; 256];
        let _ = stream.read(&mut header_buf).expect("read response header");

        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            drop(server);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fixture shutdown should not hang on active slow stream");
    }
}
