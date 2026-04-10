use super::{RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot, RubPaths};
use rub_core::process::is_process_alive;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};
use serde::Deserialize;
use std::io::{BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrySocketProbe {
    Live,
    BusyOrUnknown,
    Dead,
}

#[derive(Debug, Deserialize)]
struct RegistryHandshakePayload {
    daemon_session_id: String,
}

pub fn registry_entry_is_live_for_home(home: &Path, entry: &RegistryEntry) -> bool {
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
    if !(committed && socket_live && pid_live && runtime_pid_live) {
        return RegistryEntryLiveness::Dead;
    }

    match registry_socket_probe(entry) {
        RegistrySocketProbe::Live => RegistryEntryLiveness::Live,
        RegistrySocketProbe::BusyOrUnknown => RegistryEntryLiveness::BusyOrUnknown,
        RegistrySocketProbe::Dead => RegistryEntryLiveness::Dead,
    }
}

#[cfg(unix)]
fn registry_socket_probe(entry: &RegistryEntry) -> RegistrySocketProbe {
    let Ok(mut stream) = UnixStream::connect(&entry.socket_path) else {
        return RegistrySocketProbe::Dead;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));

    let request = IpcRequest::new("_handshake", serde_json::json!({}), 750);
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
    let response = match NdJsonCodec::read_blocking::<IpcResponse, _>(&mut reader) {
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
    if response.ipc_protocol_version != IPC_PROTOCOL_VERSION {
        return RegistrySocketProbe::Dead;
    }
    if response.status != rub_ipc::protocol::ResponseStatus::Success {
        return RegistrySocketProbe::Dead;
    }
    let Ok(payload) =
        response.data.clone().ok_or(()).and_then(|data| {
            serde_json::from_value::<RegistryHandshakePayload>(data).map_err(|_| ())
        })
    else {
        return RegistrySocketProbe::Dead;
    };
    if payload.daemon_session_id != entry.session_id {
        return RegistrySocketProbe::Dead;
    }
    RegistrySocketProbe::Live
}

#[cfg(not(unix))]
fn registry_socket_probe(_entry: &RegistryEntry) -> RegistrySocketProbe {
    RegistrySocketProbe::Dead
}

fn socket_probe_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}
