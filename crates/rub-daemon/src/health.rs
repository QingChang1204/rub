//! Health check diagnostics for `rub doctor`.

use crate::rub_paths::RubPaths;

/// System health report.
#[derive(Debug, serde::Serialize)]
pub struct HealthReport {
    pub browser_found: bool,
    pub browser_path: Option<String>,
    pub browser_version: Option<String>,
    pub daemon_running: bool,
    pub session_id: String,
    pub session_name: String,
    pub rub_home: String,
    pub ipc_protocol_version: String,
    pub rub_version: String,
    pub daemon_log_size_mb: f64,
}

/// Detect Chrome/Chromium binary on the system.
pub fn detect_browser() -> (bool, Option<String>) {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/microsoft-edge",
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return (true, Some(path.to_string()));
        }
    }

    // Try PATH lookup without depending on an external `which` binary.
    for name in &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
    ] {
        if let Some(path) = path_lookup(name) {
            return (true, Some(path));
        }
    }

    (false, None)
}

fn path_lookup(name: &str) -> Option<String> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

/// Build a health report.
pub fn build_report(
    session_id: &str,
    session_name: &str,
    rub_home: &std::path::Path,
    daemon_running: bool,
) -> HealthReport {
    let (browser_found, browser_path) = detect_browser();
    let browser_version = browser_path.as_deref().and_then(detect_browser_version);
    HealthReport {
        browser_found,
        browser_path,
        browser_version,
        daemon_running,
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        rub_home: rub_home.display().to_string(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        rub_version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_log_size_mb: daemon_log_size_mb(rub_home),
    }
}

fn detect_browser_version(path: &str) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

fn daemon_log_size_mb(rub_home: &std::path::Path) -> f64 {
    let rub_paths = RubPaths::new(rub_home);
    std::fs::metadata(rub_paths.daemon_log_path())
        .map(|metadata| metadata.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::{daemon_log_size_mb, path_lookup};
    use crate::rub_paths::RubPaths;

    #[test]
    fn daemon_log_size_reads_only_canonical_logs_dir() {
        let home = std::env::temp_dir().join(format!("rub-health-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let paths = RubPaths::new(&home);

        std::fs::write(home.join("daemon.log"), vec![0u8; 1024]).unwrap();
        assert_eq!(daemon_log_size_mb(&home), 0.0);

        std::fs::create_dir_all(paths.logs_dir()).unwrap();
        std::fs::write(paths.daemon_log_path(), vec![0u8; 2048]).unwrap();
        assert!(daemon_log_size_mb(&home) > 0.0);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn path_lookup_finds_supported_edge_binary_without_which() {
        let root =
            std::env::temp_dir().join(format!("rub-health-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let edge = root.join("microsoft-edge");
        std::fs::write(&edge, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&edge).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&edge, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", &root);
        }
        let found = path_lookup("microsoft-edge");
        if let Some(path) = old_path {
            unsafe {
                std::env::set_var("PATH", path);
            }
        } else {
            unsafe {
                std::env::remove_var("PATH");
            }
        }

        assert_eq!(found, Some(edge.display().to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
