use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const REQUEST_HEAD_READ_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_REQUEST_HEAD_BYTES: usize = 8192;

pub(crate) fn local_http_origin(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

#[cfg(test)]
pub(crate) fn http_authority(url: &str) -> &str {
    url.trim_start_matches("http://")
}

pub(crate) fn normalize_url_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

pub(crate) fn request_path(stream: &mut TcpStream) -> Option<String> {
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

pub(crate) fn write_response(
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
pub(crate) fn get(url: &str, path: &str) -> String {
    let authority = http_authority(url);
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
