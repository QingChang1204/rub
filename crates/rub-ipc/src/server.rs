use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tokio::io::BufReader;
use tokio::net::{
    UnixListener, UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::codec::NdJsonCodec;
use crate::protocol::{IpcRequest, IpcResponse};

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
            stale_socket_replacement_fence(socket_path, original_identity)?;
            match std::fs::remove_file(socket_path) {
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
        NdJsonCodec::read(&mut self.reader).await
    }

    /// Send a response to the client.
    pub async fn write_response(
        &mut self,
        response: &IpcResponse,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        NdJsonCodec::write(&mut self.writer, response).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_socket_bind_guard, prepare_socket_path_for_bind, socket_identity,
        stale_socket_replacement_fence,
    };
    use serial_test::serial;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    #[serial]
    fn stale_socket_fence_rejects_identity_change_before_unlink() {
        let root = std::env::temp_dir().join(format!("ri-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let socket_path = root.join("d.sock");

        let stale_listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        let identity = {
            let metadata = std::fs::symlink_metadata(&socket_path).expect("stale metadata");
            socket_identity(&metadata)
        };
        drop(stale_listener);
        std::fs::remove_file(&socket_path).expect("remove stale socket path");

        let live_listener = UnixListener::bind(&socket_path).expect("bind replacement socket");
        let error = stale_socket_replacement_fence(&socket_path, identity)
            .expect_err("replacement socket must block stale unlink");
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
}
