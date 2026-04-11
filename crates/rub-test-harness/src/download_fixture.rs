use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const REQUEST_HEAD_READ_TIMEOUT: Duration = Duration::from_millis(250);
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_REQUEST_HEAD_BYTES: usize = 8192;

pub struct DownloadFixtureServer {
    addr: SocketAddr,
    shutdown_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl DownloadFixtureServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("download fixture bind");
        listener
            .set_nonblocking(true)
            .expect("download fixture nonblocking");
        let addr = listener.local_addr().expect("download fixture local addr");
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
}

impl Drop for DownloadFixtureServer {
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
        "/fast.csv" => write_response(
            stream,
            "200 OK",
            "application/octet-stream",
            &[("Content-Disposition", "attachment; filename=\"report.csv\"")],
            b"id,name\n1,Ada Lovelace\n",
        ),
        "/slow.csv" => write_streaming_attachment(stream, "slow-report.csv"),
        _ => write_response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[],
            b"missing",
        ),
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

fn write_streaming_attachment(stream: &mut TcpStream, filename: &str) {
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
