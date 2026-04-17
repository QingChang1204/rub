use super::{
    CleanupOps, CleanupPath, HomeCleanupObservation, HomeDaemonAuthority,
    SocketIdentityConfirmation, cleanup_impl_with, cleanup_verification_for_path,
    daemon_command_matches_home, daemon_command_matches_home_authority,
    daemon_pid_matches_home_in_snapshot, require_product_teardown_verification,
    runtime_socket_path_for_session_id, socket_identity_confirmation,
};
use crate::browser_session::CleanupVerification;
use rub_core::managed_profile::{
    projected_managed_profile_path_for_session, sync_temp_owned_managed_profile_marker,
};
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::IpcResponse;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

static GRACEFUL_KILL_CALLS: AtomicUsize = AtomicUsize::new(0);
static FALLBACK_KILL_CALLS: AtomicUsize = AtomicUsize::new(0);
static FALLBACK_WAIT_CALLS: AtomicUsize = AtomicUsize::new(0);
static MANAGED_BROWSER_REAP_CALLS: AtomicUsize = AtomicUsize::new(0);
static REMOVE_DIR_CALLS: AtomicUsize = AtomicUsize::new(0);
static PRODUCT_CLEANUP_HOME: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static CLEANUP_TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn product_cleanup_home() -> &'static Mutex<Option<String>> {
    PRODUCT_CLEANUP_HOME.get_or_init(|| Mutex::new(None))
}

fn cleanup_test_serial() -> &'static Mutex<()> {
    CLEANUP_TEST_SERIAL.get_or_init(|| Mutex::new(()))
}

fn set_product_cleanup_home(home: Option<String>) {
    *product_cleanup_home()
        .lock()
        .expect("product cleanup home lock") = home;
}

fn unique_cleanup_home() -> String {
    let home = std::env::temp_dir().join(format!(
        "rub-harness-cleanup-{}",
        uuid::Uuid::now_v7().simple()
    ));
    std::fs::create_dir_all(&home).expect("create cleanup test home");
    home.to_string_lossy().to_string()
}

fn test_request_success(_home: &str, _timeout: std::time::Duration) -> bool {
    true
}

fn test_request_failure(_home: &str, _timeout: std::time::Duration) -> bool {
    false
}

fn test_product_teardown_removes_configured_home(
    _home: &str,
    _timeout: std::time::Duration,
) -> bool {
    if let Some(home) = product_cleanup_home()
        .lock()
        .expect("product cleanup home lock")
        .clone()
    {
        let _ = std::fs::remove_dir_all(home);
    }
    true
}

fn test_wait_immediately_succeeds(_home: &str, _timeout: std::time::Duration) -> bool {
    true
}

fn test_wait_succeeds_after_explicit_fallback_kill(
    _home: &str,
    _timeout: std::time::Duration,
) -> bool {
    FALLBACK_WAIT_CALLS.fetch_add(1, Ordering::SeqCst);
    FALLBACK_KILL_CALLS.load(Ordering::SeqCst) > 0
}

fn test_graceful_kill_counter(_home: &str) {
    GRACEFUL_KILL_CALLS.fetch_add(1, Ordering::SeqCst);
}

fn test_fallback_kill_counter(_home: &str) {
    FALLBACK_KILL_CALLS.fetch_add(1, Ordering::SeqCst);
}

fn test_wait_managed_browser_release_succeeds(
    _observed: &HomeCleanupObservation,
    _timeout: std::time::Duration,
) -> bool {
    true
}

fn test_wait_managed_browser_release_fails(
    _observed: &HomeCleanupObservation,
    _timeout: std::time::Duration,
) -> bool {
    false
}

fn test_reap_managed_browser_authority_residue(
    _observed: &HomeCleanupObservation,
    _timeout: std::time::Duration,
) {
    MANAGED_BROWSER_REAP_CALLS.fetch_add(1, Ordering::SeqCst);
}

fn test_remove_dir_counter(home: &str) {
    REMOVE_DIR_CALLS.fetch_add(1, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(home);
}

fn test_noop_remove_dir(_home: &str) {}

#[test]
fn cleanup_verification_distinguishes_harness_fallback_from_product_teardown() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let product_verified = cleanup_verification_for_path(CleanupPath::ProductTeardownVerified);
    let fallback_verified = cleanup_verification_for_path(CleanupPath::HarnessFallbackVerified);

    assert_eq!(product_verified, CleanupVerification::Verified);
    assert!(product_verified.product_teardown_verified());
    assert!(!product_verified.used_harness_fallback());

    assert_eq!(
        fallback_verified,
        CleanupVerification::VerifiedWithHarnessFallback
    );
    assert!(!fallback_verified.product_teardown_verified());
    assert!(fallback_verified.used_harness_fallback());
}

#[test]
fn default_cleanup_acceptance_rejects_harness_fallback_verification() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let accepted =
        require_product_teardown_verification("/tmp/rub-home", CleanupVerification::Verified)
            .expect("product teardown verification should pass by default");
    assert_eq!(accepted, CleanupVerification::Verified);

    let err = require_product_teardown_verification(
        "/tmp/rub-home",
        CleanupVerification::VerifiedWithHarnessFallback,
    )
    .expect_err("harness fallback must fail default browser-backed cleanup");
    assert!(
        err.contains("required harness fallback"),
        "strict cleanup error should explain the fallback regression: {err}"
    );
}

#[test]
fn graceful_cleanup_path_does_not_invoke_harness_kill() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    GRACEFUL_KILL_CALLS.store(0, Ordering::SeqCst);
    MANAGED_BROWSER_REAP_CALLS.store(0, Ordering::SeqCst);
    REMOVE_DIR_CALLS.store(0, Ordering::SeqCst);
    let observed = HomeCleanupObservation {
        daemon_root_pids: Vec::new(),
        managed_profile_dirs: Vec::new(),
    };
    let home = unique_cleanup_home();
    set_product_cleanup_home(Some(home.clone()));
    let path = cleanup_impl_with(
        &home,
        &observed,
        CleanupOps {
            request_product_teardown: test_product_teardown_removes_configured_home,
            request_cleanup_runtime: test_request_success,
            wait_for_exit: test_wait_immediately_succeeds,
            wait_for_managed_browser_authority_release: test_wait_managed_browser_release_succeeds,
            kill_home_process_tree: test_graceful_kill_counter,
            reap_managed_browser_authority_residue: test_reap_managed_browser_authority_residue,
            remove_dir_all: test_noop_remove_dir,
        },
    );
    set_product_cleanup_home(None);

    assert_eq!(path, CleanupPath::ProductTeardownVerified);
    assert_eq!(GRACEFUL_KILL_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(MANAGED_BROWSER_REAP_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(REMOVE_DIR_CALLS.load(Ordering::SeqCst), 0);
}

#[test]
fn graceful_cleanup_path_marks_harness_fallback_when_product_lane_leaves_home_removal() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    MANAGED_BROWSER_REAP_CALLS.store(0, Ordering::SeqCst);
    REMOVE_DIR_CALLS.store(0, Ordering::SeqCst);
    let observed = HomeCleanupObservation {
        daemon_root_pids: Vec::new(),
        managed_profile_dirs: Vec::new(),
    };
    let home = unique_cleanup_home();
    let path = cleanup_impl_with(
        &home,
        &observed,
        CleanupOps {
            request_product_teardown: test_request_success,
            request_cleanup_runtime: test_request_success,
            wait_for_exit: test_wait_immediately_succeeds,
            wait_for_managed_browser_authority_release: test_wait_managed_browser_release_succeeds,
            kill_home_process_tree: test_graceful_kill_counter,
            reap_managed_browser_authority_residue: test_reap_managed_browser_authority_residue,
            remove_dir_all: test_remove_dir_counter,
        },
    );

    assert_eq!(path, CleanupPath::HarnessFallbackVerified);
    assert_eq!(MANAGED_BROWSER_REAP_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(REMOVE_DIR_CALLS.load(Ordering::SeqCst), 1);
}

#[test]
fn graceful_cleanup_path_marks_harness_fallback_when_browser_authority_needs_reap() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    MANAGED_BROWSER_REAP_CALLS.store(0, Ordering::SeqCst);
    REMOVE_DIR_CALLS.store(0, Ordering::SeqCst);
    let observed = HomeCleanupObservation {
        daemon_root_pids: Vec::new(),
        managed_profile_dirs: Vec::new(),
    };
    let home = unique_cleanup_home();
    set_product_cleanup_home(Some(home.clone()));
    let path = cleanup_impl_with(
        &home,
        &observed,
        CleanupOps {
            request_product_teardown: test_product_teardown_removes_configured_home,
            request_cleanup_runtime: test_request_success,
            wait_for_exit: test_wait_immediately_succeeds,
            wait_for_managed_browser_authority_release: test_wait_managed_browser_release_fails,
            kill_home_process_tree: test_graceful_kill_counter,
            reap_managed_browser_authority_residue: test_reap_managed_browser_authority_residue,
            remove_dir_all: test_remove_dir_counter,
        },
    );
    set_product_cleanup_home(None);

    assert_eq!(path, CleanupPath::HarnessFallbackVerified);
    assert_eq!(MANAGED_BROWSER_REAP_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(REMOVE_DIR_CALLS.load(Ordering::SeqCst), 1);
}

#[test]
fn fallback_cleanup_path_marks_harness_fallback_after_explicit_kill_lane() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    FALLBACK_KILL_CALLS.store(0, Ordering::SeqCst);
    FALLBACK_WAIT_CALLS.store(0, Ordering::SeqCst);
    MANAGED_BROWSER_REAP_CALLS.store(0, Ordering::SeqCst);
    REMOVE_DIR_CALLS.store(0, Ordering::SeqCst);
    let observed = HomeCleanupObservation {
        daemon_root_pids: Vec::new(),
        managed_profile_dirs: Vec::new(),
    };
    let home = unique_cleanup_home();
    let path = cleanup_impl_with(
        &home,
        &observed,
        CleanupOps {
            request_product_teardown: test_request_failure,
            request_cleanup_runtime: test_request_success,
            wait_for_exit: test_wait_succeeds_after_explicit_fallback_kill,
            wait_for_managed_browser_authority_release: test_wait_managed_browser_release_succeeds,
            kill_home_process_tree: test_fallback_kill_counter,
            reap_managed_browser_authority_residue: test_reap_managed_browser_authority_residue,
            remove_dir_all: test_remove_dir_counter,
        },
    );

    assert_eq!(path, CleanupPath::HarnessFallbackVerified);
    assert_eq!(FALLBACK_KILL_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(FALLBACK_WAIT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(MANAGED_BROWSER_REAP_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(REMOVE_DIR_CALLS.load(Ordering::SeqCst), 1);
}

#[test]
fn daemon_pid_matches_home_in_snapshot_requires_exact_rub_home_flag_match() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let snapshot = format!(
        "101 /tmp/target/debug/rub __daemon --session default --rub-home {}\n102 /tmp/target/debug/rub __daemon --session default --rub-home {}-other\n",
        "/tmp/rub-home", "/tmp/rub-home",
    );
    assert!(daemon_pid_matches_home_in_snapshot(
        &snapshot,
        101,
        "/tmp/rub-home"
    ));
    assert!(!daemon_pid_matches_home_in_snapshot(
        &snapshot,
        102,
        "/tmp/rub-home"
    ));
    assert!(daemon_command_matches_home(
        "/tmp/target/debug/rub __daemon --session default --rub-home /tmp/rub-home",
        "/tmp/rub-home"
    ));
    assert!(!daemon_command_matches_home(
        "/tmp/target/debug/rub __daemon --session default --rub-home /tmp/rub-home-other",
        "/tmp/rub-home"
    ));
}

#[test]
fn daemon_command_matches_home_authority_requires_exact_session_metadata() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let authority = HomeDaemonAuthority {
        pid: 101,
        session_name: Some("default".to_string()),
        session_id: Some("sess-live".to_string()),
        socket_path: Some(runtime_socket_path_for_session_id(
            "/tmp/rub-home",
            "sess-live",
        )),
        user_data_dir: None,
    };
    assert!(daemon_command_matches_home_authority(
        "/tmp/target/debug/rub __daemon --session default --session-id sess-live --rub-home /tmp/rub-home",
        "/tmp/rub-home",
        &authority,
    ));
    assert!(!daemon_command_matches_home_authority(
        "/tmp/target/debug/rub __daemon --session other --session-id sess-live --rub-home /tmp/rub-home",
        "/tmp/rub-home",
        &authority,
    ));
    assert!(!daemon_command_matches_home_authority(
        "/tmp/target/debug/rub __daemon --session default --session-id sess-other --rub-home /tmp/rub-home",
        "/tmp/rub-home",
        &authority,
    ));
}

#[test]
fn observe_home_cleanup_captures_exact_session_scoped_managed_profile_dirs() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let home = std::env::temp_dir().join(format!(
        "rub-harness-observed-managed-profile-{}",
        uuid::Uuid::now_v7().simple()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create cleanup observation home");
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-observed");
    std::fs::create_dir_all(runtime.session_dir()).expect("create session runtime dir");
    std::fs::write(runtime.pid_path(), "424242\n").expect("write daemon pid");
    let managed_profile_dir = projected_managed_profile_path_for_session("sess-observed");
    let _ = std::fs::remove_dir_all(&managed_profile_dir);
    sync_temp_owned_managed_profile_marker(&managed_profile_dir, true)
        .expect("mark observed profile temp-owned");

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-observed".to_string(),
                session_name: "default".to_string(),
                pid: 424242,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-17T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: Some(managed_profile_dir.display().to_string()),
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .expect("write registry");

    let observed = super::observe_home_cleanup(home.to_string_lossy().as_ref());

    assert!(
        observed.managed_profile_dirs.contains(&managed_profile_dir),
        "cleanup observation must retain exact session-scoped managed profile authority"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&managed_profile_dir);
}

#[test]
fn observe_home_cleanup_ignores_explicit_durable_tmp_profile_shape_without_marker() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let home = std::env::temp_dir().join(format!(
        "rub-harness-observed-explicit-profile-{}",
        uuid::Uuid::now_v7().simple()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create cleanup observation home");
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-explicit");
    std::fs::create_dir_all(runtime.session_dir()).expect("create session runtime dir");
    std::fs::write(runtime.pid_path(), "525252\n").expect("write daemon pid");
    let managed_profile_dir = std::env::temp_dir().join("rub-chrome-explicit-workspace");
    let _ = std::fs::remove_dir_all(&managed_profile_dir);
    std::fs::create_dir_all(&managed_profile_dir).expect("create explicit tmp profile");

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-explicit".to_string(),
                session_name: "default".to_string(),
                pid: 525252,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-17T00:00:01Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: Some(managed_profile_dir.display().to_string()),
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .expect("write registry");

    let observed = super::observe_home_cleanup(home.to_string_lossy().as_ref());

    assert!(
        !observed.managed_profile_dirs.contains(&managed_profile_dir),
        "strict cleanup observation must not promote durable tmp-shaped profiles into temp-owned authority"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&managed_profile_dir);
}

#[cfg(unix)]
fn spawn_handshake_server(
    socket_path: &std::path::Path,
    daemon_session_id: &str,
) -> std::thread::JoinHandle<()> {
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path).expect("bind handshake socket");
    let daemon_session_id = daemon_session_id.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept handshake connection");
        let mut reader = std::io::BufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for reading"),
        );
        let _request = NdJsonCodec::read_blocking::<rub_ipc::protocol::IpcRequest, _>(&mut reader)
            .expect("read handshake request")
            .expect("handshake request");
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": daemon_session_id,
            }),
        );
        let encoded = NdJsonCodec::encode(&response).expect("encode handshake response");
        stream
            .write_all(&encoded)
            .expect("write handshake response");
    })
}

#[cfg(unix)]
fn unique_socket_path() -> (std::path::PathBuf, std::path::PathBuf) {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let socket_dir = std::env::temp_dir().join(format!("rth-{}", &suffix[..12]));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    (socket_dir.join("ipc.sock"), socket_dir)
}

#[test]
#[cfg(unix)]
fn socket_identity_confirmation_rejects_mismatched_session() {
    let _serial = cleanup_test_serial().lock().expect("cleanup test serial");
    let (socket_path, socket_dir) = unique_socket_path();
    let server = spawn_handshake_server(&socket_path, "sess-other");
    let confirmation = socket_identity_confirmation(&socket_path, "sess-live");
    server.join().expect("handshake server should join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(socket_dir);
    assert_eq!(confirmation, SocketIdentityConfirmation::ConfirmedMismatch);
}
