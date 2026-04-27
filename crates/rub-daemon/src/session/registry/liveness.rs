use super::{
    RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot, RubPaths,
    hard_cut_release_pending_blocks_entry,
};
use rub_core::process::is_process_alive;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::handshake::{
    HANDSHAKE_PROBE_COMMAND_ID, SocketSessionIdentityConfirmation,
    classify_handshake_probe_response,
};
use rub_ipc::protocol::IpcRequest;
use std::io::{BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(unix)]
use tokio::io::BufReader as AsyncBufReader;

#[cfg(test)]
static FORCE_SOCKET_PROBE_ONCE: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::BTreeMap<PathBuf, std::collections::VecDeque<RegistrySocketProbe>>,
    >,
> = std::sync::OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrySocketProbe {
    Live,
    BusyOrUnknown,
    ProbeContractFailure,
    ProtocolIncompatible,
    Dead,
}

pub fn registry_entry_is_live_for_home(home: &Path, entry: &RegistryEntry) -> bool {
    registry_entry_snapshot_for_home(home, entry).is_live_authority()
}

pub(crate) fn registry_entry_has_runtime_authority_for_home(
    home: &Path,
    entry: &RegistryEntry,
) -> bool {
    registry_entry_snapshot_for_home(home, entry).is_live_authority()
}

pub fn registry_entry_is_pending_startup_for_home(home: &Path, entry: &RegistryEntry) -> bool {
    registry_entry_snapshot_for_home(home, entry).is_pending_startup()
}

pub(super) fn registry_entry_snapshot_for_home(
    home: &Path,
    entry: &RegistryEntry,
) -> RegistryEntrySnapshot {
    let pid_live = is_process_alive(entry.pid);
    RegistryEntrySnapshot {
        entry: entry.clone(),
        liveness: registry_entry_liveness_for_home(home, entry, pid_live),
        pid_live,
    }
}

pub(crate) async fn registry_entry_snapshot_async_for_home(
    home: PathBuf,
    entry: RegistryEntry,
    allow_socket_probe: bool,
) -> RegistryEntrySnapshot {
    let pid_live = is_process_alive(entry.pid);
    RegistryEntrySnapshot {
        entry: entry.clone(),
        liveness: registry_entry_liveness_async_for_home(
            &home,
            &entry,
            pid_live,
            allow_socket_probe,
        )
        .await,
        pid_live,
    }
}

fn registry_entry_liveness_for_home(
    home: &Path,
    entry: &RegistryEntry,
    pid_live: bool,
) -> RegistryEntryLiveness {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let socket_live = Path::new(&entry.socket_path).exists();
    let committed = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|session_id| session_id == entry.session_id);
    let runtime_pid_live = runtime.pid_path().exists();
    if !committed && socket_live && pid_live && runtime_pid_live {
        return RegistryEntryLiveness::PendingStartup;
    }
    if hard_cut_release_pending_blocks_entry(home, entry) {
        return RegistryEntryLiveness::HardCutReleasePending;
    }
    if !(committed && socket_live && pid_live && runtime_pid_live) {
        return RegistryEntryLiveness::Dead;
    }

    match registry_socket_probe(entry) {
        RegistrySocketProbe::Live => RegistryEntryLiveness::Live,
        RegistrySocketProbe::BusyOrUnknown => RegistryEntryLiveness::BusyOrUnknown,
        RegistrySocketProbe::ProbeContractFailure => RegistryEntryLiveness::ProbeContractFailure,
        RegistrySocketProbe::ProtocolIncompatible => RegistryEntryLiveness::ProtocolIncompatible,
        RegistrySocketProbe::Dead => RegistryEntryLiveness::Dead,
    }
}

async fn registry_entry_liveness_async_for_home(
    home: &Path,
    entry: &RegistryEntry,
    pid_live: bool,
    allow_socket_probe: bool,
) -> RegistryEntryLiveness {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let socket_live = Path::new(&entry.socket_path).exists();
    let committed = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|session_id| session_id == entry.session_id);
    let runtime_pid_live = runtime.pid_path().exists();
    if !committed && socket_live && pid_live && runtime_pid_live {
        return RegistryEntryLiveness::PendingStartup;
    }
    if hard_cut_release_pending_blocks_entry(home, entry) {
        return RegistryEntryLiveness::HardCutReleasePending;
    }
    if !(committed && socket_live && pid_live && runtime_pid_live) {
        return RegistryEntryLiveness::Dead;
    }
    if !allow_socket_probe {
        return RegistryEntryLiveness::BusyOrUnknown;
    }

    match registry_socket_probe_async(entry).await {
        RegistrySocketProbe::Live => RegistryEntryLiveness::Live,
        RegistrySocketProbe::BusyOrUnknown => RegistryEntryLiveness::BusyOrUnknown,
        RegistrySocketProbe::ProbeContractFailure => RegistryEntryLiveness::ProbeContractFailure,
        RegistrySocketProbe::ProtocolIncompatible => RegistryEntryLiveness::ProtocolIncompatible,
        RegistrySocketProbe::Dead => RegistryEntryLiveness::Dead,
    }
}

#[cfg(unix)]
fn registry_socket_probe(entry: &RegistryEntry) -> RegistrySocketProbe {
    #[cfg(test)]
    if let Some(probe) = consume_socket_probe_once(Path::new(&entry.socket_path)) {
        return probe;
    }

    let Ok(mut stream) = UnixStream::connect(&entry.socket_path) else {
        return RegistrySocketProbe::Dead;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));

    let request = match IpcRequest::new("_handshake", serde_json::json!({}), 750)
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
    {
        Ok(request) => request,
        Err(_) => return RegistrySocketProbe::Dead,
    };
    let Ok(encoded) = NdJsonCodec::encode(&request) else {
        return RegistrySocketProbe::Dead;
    };
    if let Err(error) = stream.write_all(&encoded) {
        return if socket_probe_timeout(&error) {
            RegistrySocketProbe::BusyOrUnknown
        } else {
            RegistrySocketProbe::Dead
        };
    }

    let mut reader = BufReader::new(stream);
    let response_value = match NdJsonCodec::read_blocking::<serde_json::Value, _>(&mut reader) {
        Ok(Some(response)) => response,
        Ok(None) => return RegistrySocketProbe::Dead,
        Err(error) => {
            let timeout = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(socket_probe_timeout);
            return if timeout {
                RegistrySocketProbe::BusyOrUnknown
            } else {
                RegistrySocketProbe::Dead
            };
        }
    };
    match classify_handshake_probe_response(response_value, &entry.session_id) {
        SocketSessionIdentityConfirmation::ConfirmedMatch => RegistrySocketProbe::Live,
        SocketSessionIdentityConfirmation::ConfirmedMismatch => RegistrySocketProbe::Dead,
        SocketSessionIdentityConfirmation::ProtocolVersionMismatch => {
            RegistrySocketProbe::ProtocolIncompatible
        }
        SocketSessionIdentityConfirmation::ProbeContractFailure => {
            RegistrySocketProbe::ProbeContractFailure
        }
        SocketSessionIdentityConfirmation::Inconclusive => RegistrySocketProbe::BusyOrUnknown,
    }
}

#[cfg(not(unix))]
fn registry_socket_probe(_entry: &RegistryEntry) -> RegistrySocketProbe {
    RegistrySocketProbe::Dead
}

#[cfg(unix)]
async fn registry_socket_probe_async(entry: &RegistryEntry) -> RegistrySocketProbe {
    #[cfg(test)]
    if let Some(probe) = consume_socket_probe_once(Path::new(&entry.socket_path)) {
        return probe;
    }

    let connect = tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::UnixStream::connect(&entry.socket_path),
    )
    .await;
    let mut stream = match connect {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return RegistrySocketProbe::Dead,
        Err(_) => return RegistrySocketProbe::BusyOrUnknown,
    };

    let request = match IpcRequest::new("_handshake", serde_json::json!({}), 750)
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
    {
        Ok(request) => request,
        Err(_) => return RegistrySocketProbe::Dead,
    };
    match tokio::time::timeout(
        Duration::from_millis(750),
        NdJsonCodec::write(&mut stream, &request),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return if boxed_socket_probe_timeout(&*error) {
                RegistrySocketProbe::BusyOrUnknown
            } else {
                RegistrySocketProbe::Dead
            };
        }
        Err(_) => return RegistrySocketProbe::BusyOrUnknown,
    }

    let mut reader = AsyncBufReader::new(stream);
    let response_value = match tokio::time::timeout(
        Duration::from_millis(750),
        NdJsonCodec::read::<serde_json::Value, _>(&mut reader),
    )
    .await
    {
        Ok(Ok(Some(response))) => response,
        Ok(Ok(None)) => return RegistrySocketProbe::Dead,
        Ok(Err(error)) => {
            return if boxed_socket_probe_timeout(&*error) {
                RegistrySocketProbe::BusyOrUnknown
            } else {
                RegistrySocketProbe::Dead
            };
        }
        Err(_) => return RegistrySocketProbe::BusyOrUnknown,
    };

    match classify_handshake_probe_response(response_value, &entry.session_id) {
        SocketSessionIdentityConfirmation::ConfirmedMatch => RegistrySocketProbe::Live,
        SocketSessionIdentityConfirmation::ConfirmedMismatch => RegistrySocketProbe::Dead,
        SocketSessionIdentityConfirmation::ProtocolVersionMismatch => {
            RegistrySocketProbe::ProtocolIncompatible
        }
        SocketSessionIdentityConfirmation::ProbeContractFailure => {
            RegistrySocketProbe::ProbeContractFailure
        }
        SocketSessionIdentityConfirmation::Inconclusive => RegistrySocketProbe::BusyOrUnknown,
    }
}

#[cfg(not(unix))]
async fn registry_socket_probe_async(_entry: &RegistryEntry) -> RegistrySocketProbe {
    RegistrySocketProbe::Dead
}

fn socket_probe_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn boxed_socket_probe_timeout(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(socket_probe_timeout)
}

#[cfg(test)]
fn consume_socket_probe_once(path: &Path) -> Option<RegistrySocketProbe> {
    let mut probes = FORCE_SOCKET_PROBE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
        .lock()
        .expect("registry socket probe override");
    let queue = probes.get_mut(path)?;
    let probe = queue.pop_front();
    if queue.is_empty() {
        probes.remove(path);
    }
    probe
}

#[cfg(test)]
fn force_registry_socket_probe_once_for_test(path: &Path, probe: RegistrySocketProbe) {
    let mut probes = FORCE_SOCKET_PROBE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
        .lock()
        .expect("registry socket probe override");
    probes
        .entry(path.to_path_buf())
        .or_default()
        .push_back(probe);
}

#[cfg(test)]
pub(crate) fn force_live_registry_socket_probe_once_for_test(path: &Path) {
    force_registry_socket_probe_once_for_test(path, RegistrySocketProbe::Live);
}

#[cfg(test)]
pub(crate) fn force_busy_registry_socket_probe_once_for_test(path: &Path) {
    force_registry_socket_probe_once_for_test(path, RegistrySocketProbe::BusyOrUnknown);
}

#[cfg(test)]
pub(crate) fn force_probe_contract_failure_registry_socket_probe_once_for_test(path: &Path) {
    force_registry_socket_probe_once_for_test(path, RegistrySocketProbe::ProbeContractFailure);
}

#[cfg(test)]
pub(crate) fn force_protocol_incompatible_registry_socket_probe_once_for_test(path: &Path) {
    force_registry_socket_probe_once_for_test(path, RegistrySocketProbe::ProtocolIncompatible);
}

#[cfg(test)]
pub(crate) fn force_dead_registry_socket_probe_once_for_test(path: &Path) {
    force_registry_socket_probe_once_for_test(path, RegistrySocketProbe::Dead);
}
