use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::BufReader;
use tokio::net::{
    UnixListener, UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::codec::NdJsonCodec;
use crate::protocol::{IpcProtocolDecodeError, IpcRequest, IpcResponse};

/// Daemon-side IPC server. Listens on a Unix socket.
pub struct IpcServer {
    listener: UnixListener,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    dev: u64,
    ino: u64,
}

pub struct SocketBindGuard {
    #[cfg(unix)]
    _lock_file: std::fs::File,
}

static STALE_SOCKET_QUARANTINE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl IpcServer {
    /// Bind to a Unix socket at the given path.
    pub async fn bind(socket_path: &Path) -> Result<Self, std::io::Error> {
        let _bind_guard = prepare_socket_path_for_bind(socket_path).await?;

        let listener = UnixListener::bind(socket_path)?;
        Ok(Self { listener })
    }

    /// Accept one incoming connection.
    pub async fn accept(&self) -> Result<IpcConnection, std::io::Error> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(IpcConnection::new(stream))
    }
}

pub async fn prepare_socket_path_for_bind(socket_path: &Path) -> io::Result<SocketBindGuard> {
    let socket_path = socket_path.to_path_buf();
    tokio::task::spawn_blocking(move || prepare_socket_path_for_bind_blocking(&socket_path))
        .await
        .map_err(|error| io::Error::other(format!("socket bind prep join failed: {error}")))?
}

fn prepare_socket_path_for_bind_blocking(socket_path: &Path) -> io::Result<SocketBindGuard> {
    let bind_guard = acquire_socket_bind_guard(socket_path)?;
    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(bind_guard),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Refusing to bind IPC socket over symlink path {}",
                socket_path.display()
            ),
        ));
    }
    #[cfg(unix)]
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Refusing to replace non-socket IPC path {} during bind",
                socket_path.display()
            ),
        ));
    }
    #[cfg(unix)]
    let original_identity = socket_identity(&metadata);

    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "IPC socket {} is already owned by a live daemon",
                socket_path.display()
            ),
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            #[cfg(unix)]
            let quarantine_path =
                quarantine_stale_socket_after_replacement_fence(socket_path, original_identity)?;
            #[cfg(unix)]
            match std::fs::remove_file(&quarantine_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }?;
            Ok(bind_guard)
        }
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "Refusing to replace existing IPC socket {} after liveness probe failed: {error}",
                socket_path.display()
            ),
        )),
    }
}

fn socket_bind_lock_path(socket_path: &Path) -> PathBuf {
    let file_name = socket_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rub-ipc".to_string());
    socket_path.with_file_name(format!(".{file_name}.bind.lock"))
}

#[cfg(unix)]
fn acquire_socket_bind_guard(socket_path: &Path) -> io::Result<SocketBindGuard> {
    let lock_path = socket_bind_lock_path(socket_path);
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    let result = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SocketBindGuard {
        _lock_file: lock_file,
    })
}

#[cfg(not(unix))]
fn acquire_socket_bind_guard(_socket_path: &Path) -> io::Result<SocketBindGuard> {
    Ok(SocketBindGuard {})
}

#[cfg(unix)]
fn socket_identity(metadata: &std::fs::Metadata) -> SocketIdentity {
    SocketIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(unix)]
fn stale_socket_replacement_fence(
    socket_path: &Path,
    original_identity: SocketIdentity,
) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Refusing to replace IPC socket {} after ownership changed during stale-socket cleanup",
                socket_path.display()
            ),
        ));
    }
    if socket_identity(&metadata) != original_identity {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "Refusing to replace IPC socket {} because ownership changed after the stale-socket probe",
                socket_path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn quarantine_stale_socket_after_replacement_fence(
    socket_path: &Path,
    original_identity: SocketIdentity,
) -> io::Result<PathBuf> {
    stale_socket_replacement_fence(socket_path, original_identity)?;
    let quarantine_path = unique_stale_socket_quarantine_path(socket_path);
    std::fs::rename(socket_path, &quarantine_path)?;

    let metadata = std::fs::symlink_metadata(&quarantine_path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Refusing to delete quarantined IPC socket {} because the fenced socket changed shape during stale cleanup",
                quarantine_path.display()
            ),
        ));
    }
    if socket_identity(&metadata) != original_identity {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "Refusing to delete quarantined IPC socket {} because the fenced socket identity changed during stale cleanup",
                quarantine_path.display()
            ),
        ));
    }
    Ok(quarantine_path)
}

#[cfg(unix)]
fn unique_stale_socket_quarantine_path(socket_path: &Path) -> PathBuf {
    let file_name = socket_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rub-ipc.sock".to_string());
    socket_path.with_file_name(format!(
        ".{file_name}.stale.{}.{}",
        uuid::Uuid::now_v7(),
        STALE_SOCKET_QUARANTINE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A single IPC connection from a CLI client.
pub struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl IpcConnection {
    fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    /// Read a request from the client.
    /// Returns `None` on EOF (client disconnected).
    pub async fn read_request(
        &mut self,
    ) -> Result<Option<IpcRequest>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(frame) = NdJsonCodec::read_frame_bytes(&mut self.reader).await? else {
            return Ok(None);
        };
        let correlation = RequestCorrelation::from_request_frame(&frame);
        let value = serde_json::from_slice::<serde_json::Value>(&frame).map_err(|error| {
            let envelope = transport_read_failure_envelope(Box::new(error));
            Box::new(IpcProtocolDecodeError::with_request_correlation(
                envelope,
                correlation.command_id,
                correlation.daemon_session_id,
            )) as Box<dyn std::error::Error + Send + Sync>
        })?;
        let correlation = RequestCorrelation::from_request_value(&value);
        let request = IpcRequest::from_value_transport(value).map_err(|envelope| {
            Box::new(IpcProtocolDecodeError::with_request_correlation(
                envelope,
                correlation.command_id,
                correlation.daemon_session_id,
            )) as Box<dyn std::error::Error + Send + Sync>
        })?;
        Ok(Some(request))
    }

    /// Send a response after validating it against the request authority that
    /// opened this IPC transaction.
    pub async fn write_response_for_request(
        &mut self,
        request: &IpcRequest,
        response: &IpcResponse,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        response
            .validate_transport_contract(request)
            .map_err(|envelope| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, envelope.message)
            })?;
        response
            .validate_correlated_contract(request)
            .map_err(|envelope| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, envelope.message)
            })?;
        NdJsonCodec::write(&mut self.writer, response).await
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RequestCorrelation {
    command_id: Option<String>,
    daemon_session_id: Option<String>,
}

impl RequestCorrelation {
    fn from_request_value(value: &serde_json::Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            command_id: sanitize_optional_protocol_string(object.get("command_id")),
            daemon_session_id: sanitize_optional_protocol_string(object.get("daemon_session_id")),
        }
    }

    fn from_request_frame(frame: &[u8]) -> Self {
        Self {
            command_id: recover_top_level_string_field_from_frame(frame, "command_id"),
            daemon_session_id: recover_top_level_string_field_from_frame(
                frame,
                "daemon_session_id",
            ),
        }
    }
}

fn sanitize_optional_protocol_string(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value.and_then(serde_json::Value::as_str)?;
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn transport_read_failure_envelope(
    error: Box<dyn std::error::Error + Send + Sync>,
) -> rub_core::error::ErrorEnvelope {
    match error.downcast::<IpcProtocolDecodeError>() {
        Ok(protocol_error) => protocol_error.into_envelope(),
        Err(error) => match error.downcast::<std::io::Error>() {
            Ok(io_error) => {
                let reason = match io_error.kind() {
                    std::io::ErrorKind::UnexpectedEof => "partial_ndjson_frame",
                    std::io::ErrorKind::InvalidData
                        if crate::codec::is_oversized_frame_io_error(io_error.as_ref()) =>
                    {
                        "oversized_ndjson_frame"
                    }
                    std::io::ErrorKind::InvalidData => "invalid_ndjson_frame",
                    _ => "ipc_read_failure",
                };
                rub_core::error::ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("Invalid NDJSON request: {io_error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": reason,
                }))
            }
            Err(error) => match error.downcast::<serde_json::Error>() {
                Ok(json_error) => rub_core::error::ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("Invalid JSON request body: {json_error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": "invalid_json_request",
                })),
                Err(error) => rub_core::error::ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("Failed to read IPC request: {error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": "ipc_read_failure",
                })),
            },
        },
    }
}

fn recover_top_level_string_field_from_frame(frame: &[u8], field: &str) -> Option<String> {
    let bytes = frame;
    let mut cursor = 0usize;
    skip_json_whitespace(bytes, &mut cursor);
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor += 1;

    loop {
        skip_json_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return None;
        }
        let key = parse_json_string(bytes, &mut cursor)?;
        skip_json_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b':') {
            return None;
        }
        cursor += 1;
        skip_json_whitespace(bytes, &mut cursor);

        if key == field {
            if bytes.get(cursor) == Some(&b'"') {
                let value = parse_json_string(bytes, &mut cursor)?;
                return (!value.trim().is_empty()).then_some(value);
            }
            return None;
        }

        skip_json_value(bytes, &mut cursor)?;
        skip_json_whitespace(bytes, &mut cursor);
        match bytes.get(cursor) {
            Some(b',') => {
                cursor += 1;
            }
            Some(b'}') => return None,
            _ => return None,
        }
    }
}

fn skip_json_whitespace(bytes: &[u8], cursor: &mut usize) {
    while let Some(byte) = bytes.get(*cursor) {
        if !byte.is_ascii_whitespace() {
            break;
        }
        *cursor += 1;
    }
}

fn parse_json_string(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    if bytes.get(*cursor) != Some(&b'"') {
        return None;
    }
    let start = *cursor;
    *cursor += 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(*cursor) {
        *cursor += 1;
        if escaped {
            escaped = false;
            continue;
        }
        match *byte {
            b'\\' => escaped = true,
            b'"' => {
                let slice = std::str::from_utf8(&bytes[start..*cursor]).ok()?;
                return serde_json::from_str::<String>(slice).ok();
            }
            _ => {}
        }
    }
    None
}

fn skip_json_value(bytes: &[u8], cursor: &mut usize) -> Option<()> {
    match bytes.get(*cursor)? {
        b'"' => {
            parse_json_string(bytes, cursor)?;
            Some(())
        }
        b'{' => {
            *cursor += 1;
            loop {
                skip_json_whitespace(bytes, cursor);
                match bytes.get(*cursor)? {
                    b'}' => {
                        *cursor += 1;
                        return Some(());
                    }
                    b'"' => {
                        parse_json_string(bytes, cursor)?;
                        skip_json_whitespace(bytes, cursor);
                        if bytes.get(*cursor)? != &b':' {
                            return None;
                        }
                        *cursor += 1;
                        skip_json_whitespace(bytes, cursor);
                        skip_json_value(bytes, cursor)?;
                        skip_json_whitespace(bytes, cursor);
                        match bytes.get(*cursor)? {
                            b',' => *cursor += 1,
                            b'}' => {
                                *cursor += 1;
                                return Some(());
                            }
                            _ => return None,
                        }
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *cursor += 1;
            loop {
                skip_json_whitespace(bytes, cursor);
                match bytes.get(*cursor)? {
                    b']' => {
                        *cursor += 1;
                        return Some(());
                    }
                    _ => {
                        skip_json_value(bytes, cursor)?;
                        skip_json_whitespace(bytes, cursor);
                        match bytes.get(*cursor)? {
                            b',' => *cursor += 1,
                            b']' => {
                                *cursor += 1;
                                return Some(());
                            }
                            _ => return None,
                        }
                    }
                }
            }
        }
        _ => {
            while let Some(byte) = bytes.get(*cursor) {
                match *byte {
                    b',' | b']' | b'}' if !byte.is_ascii_whitespace() => break,
                    _ => *cursor += 1,
                }
            }
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_socket_bind_guard, prepare_socket_path_for_bind,
        quarantine_stale_socket_after_replacement_fence, socket_identity,
        stale_socket_replacement_fence,
    };
    use crate::protocol::{IpcProtocolDecodeError, IpcRequest, IpcResponse};
    use serial_test::serial;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    #[test]
    fn stale_socket_fence_rejects_identity_change_before_unlink() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");
        let other_path = root.join("other.sock");

        // Bind both sockets simultaneously. While both files exist at the same
        // time the OS cannot assign the same inode to both, so the identities
        // are guaranteed to differ. We capture other.sock's identity as the
        // "stale" reference, then close it — leaving d.sock as the "live"
        // replacement. The fence must reject cleaning up d.sock when given
        // other.sock's stale identity.
        let live_listener = UnixListener::bind(&socket_path).expect("bind live socket");
        let other_listener = UnixListener::bind(&other_path).expect("bind other socket");
        let stale_identity = {
            let metadata = std::fs::symlink_metadata(&other_path).expect("other metadata");
            socket_identity(&metadata)
        };
        drop(other_listener);
        let _ = std::fs::remove_file(&other_path);

        let error = stale_socket_replacement_fence(&socket_path, stale_identity)
            .expect_err("replacement socket with different identity must block stale unlink");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);

        drop(live_listener);
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[serial]
    fn stale_socket_fence_accepts_same_socket_identity() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");

        let stale_listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        let identity = {
            let metadata = std::fs::symlink_metadata(&socket_path).expect("stale metadata");
            socket_identity(&metadata)
        };
        drop(stale_listener);

        stale_socket_replacement_fence(&socket_path, identity)
            .expect("same socket identity should remain eligible for stale cleanup");

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[serial]
    fn quarantine_stale_socket_moves_fenced_identity_off_bind_path_before_delete() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");

        let stale_listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        let identity = {
            let metadata = std::fs::symlink_metadata(&socket_path).expect("stale metadata");
            socket_identity(&metadata)
        };
        drop(stale_listener);

        let quarantine_path =
            quarantine_stale_socket_after_replacement_fence(&socket_path, identity)
                .expect("stale socket should move into quarantine");
        assert!(
            !socket_path.exists(),
            "bind path must be cleared before stale cleanup deletes anything"
        );
        let quarantined = std::fs::symlink_metadata(&quarantine_path).expect("quarantine metadata");
        assert_eq!(socket_identity(&quarantined), identity);

        std::fs::remove_file(&quarantine_path).expect("remove quarantine path");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_socket_path_for_bind_serializes_stale_cleanup_with_bind_lock() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");

        let stale_listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        drop(stale_listener);

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let first_guard = runtime
            .block_on(prepare_socket_path_for_bind(&socket_path))
            .expect("first bind guard");

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let socket_path_clone = socket_path.clone();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("worker runtime");
            started_tx.send(()).expect("started");
            let result = runtime.block_on(prepare_socket_path_for_bind(&socket_path_clone));
            done_tx.send(result.is_ok()).expect("done");
        });

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should start");
        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "second prepare must remain blocked while the first bind guard still owns the socket-path fence"
        );

        drop(first_guard);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(true));

        worker.join().expect("worker join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(root.join(".d.sock.bind.lock"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_socket_path_for_bind_does_not_block_current_thread_runtime() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let socket_path_clone = socket_path.clone();
        let lock_holder = std::thread::spawn(move || {
            let guard = acquire_socket_bind_guard(&socket_path_clone).expect("acquire bind guard");
            locked_tx.send(()).expect("locked");
            release_rx.recv().expect("release");
            drop(guard);
        });

        locked_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bind lock should be held");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let heartbeat = tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(25)).await;
            });
            let socket_path_for_prepare = socket_path.clone();
            let prepare = tokio::spawn(async move {
                prepare_socket_path_for_bind(&socket_path_for_prepare).await
            });

            tokio::time::sleep(Duration::from_millis(75)).await;
            assert!(
                heartbeat.is_finished(),
                "bind preparation must not pin the current-thread runtime while waiting on the filesystem fence"
            );

            release_tx.send(()).expect("release bind guard");
            prepare
                .await
                .expect("prepare join")
                .expect("bind prep result");
        });

        lock_holder.join().expect("lock-holder join");
        let _ = std::fs::remove_file(root.join(".d.sock.bind.lock"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_request_preserves_correlation_on_transport_contract_failure() {
        let (mut client, server) = UnixStream::pair().expect("unix pair");
        let mut connection = super::IpcConnection::new(server);
        client
            .write_all(
                br#"{"ipc_protocol_version":"0.9","command":"doctor","command_id":"cmd-1","daemon_session_id":"sess-1","args":{},"timeout_ms":1000}
"#,
            )
            .await
            .expect("write request frame");
        drop(client);

        let error = connection
            .read_request()
            .await
            .expect_err("transport contract failure must fail closed");
        let protocol_error = error
            .downcast::<IpcProtocolDecodeError>()
            .expect("should expose protocol decode error");
        assert_eq!(protocol_error.command_id(), Some("cmd-1"));
        assert_eq!(protocol_error.daemon_session_id(), Some("sess-1"));
        assert_eq!(
            protocol_error
                .envelope()
                .context
                .as_ref()
                .and_then(|context| context.get("field"))
                .and_then(|value| value.as_str()),
            Some("ipc_protocol_version")
        );
    }

    #[tokio::test]
    async fn read_request_recovers_correlation_from_invalid_json_frame() {
        let (mut client, server) = UnixStream::pair().expect("unix pair");
        let mut connection = super::IpcConnection::new(server);
        client
            .write_all(
                br#"{"ipc_protocol_version":"1.1","command":"doctor","command_id":"cmd-json","daemon_session_id":"sess-json","args":{"broken": },"timeout_ms":1000}
"#,
            )
            .await
            .expect("write malformed frame");
        drop(client);

        let error = connection
            .read_request()
            .await
            .expect_err("invalid JSON must fail closed");
        let protocol_error = error
            .downcast::<IpcProtocolDecodeError>()
            .expect("should expose protocol decode error");
        assert_eq!(protocol_error.command_id(), Some("cmd-json"));
        assert_eq!(protocol_error.daemon_session_id(), Some("sess-json"));
        assert_eq!(
            protocol_error
                .envelope()
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("invalid_json_request")
        );
    }

    #[tokio::test]
    async fn write_response_for_request_rejects_uncorrelated_response() {
        let (client, server) = UnixStream::pair().expect("unix pair");
        drop(client);
        let mut connection = super::IpcConnection::new(server);
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_daemon_session_id("sess-1")
            .expect("daemon_session_id should be valid");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id("different-command")
            .expect("command_id should be valid")
            .with_daemon_session_id("sess-1")
            .expect("daemon_session_id should be valid");

        let error = connection
            .write_response_for_request(&request, &response)
            .await
            .expect_err("server write fence must reject uncorrelated response");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .expect("correlation failure should become io invalid data")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
