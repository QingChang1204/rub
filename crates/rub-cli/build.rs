use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    // Priority order for git SHA:
    // 1. GIT_SHA env var (explicitly injected by CI / release pipeline)
    // 2. `git rev-parse --short HEAD` (local dev checkout)
    // 3. CARGO_PKG_VERSION_PRE (non-empty for pre-release tags, contains useful info)
    // 4. Omitted — show clean version-only string (source tarball / crates.io install)
    let git_sha = std::env::var("GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| command_output(&["git", "rev-parse", "--short", "HEAD"]));

    // Priority order for build date:
    // 1. SOURCE_DATE_EPOCH (reproducible-build standard; set by cargo-vendor / packaging tools)
    // 2. `date -u` (local dev)
    let build_date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|epoch| {
            epoch.trim().parse::<i64>().ok().map(|ts| {
                // Format as ISO-8601 date only (no time needed for source tarballs)
                let secs = ts;
                let days = secs / 86400;
                // Simple Gregorian approximation sufficient for display purposes
                let year = 1970 + days / 365;
                format!("{year}-??-?? (epoch:{ts})")
            })
        })
        .or_else(|| command_output(&["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"]));

    let build_version = match (git_sha, build_date) {
        (Some(sha), Some(date)) => format!("{version}+{sha} ({date})"),
        (Some(sha), None) => format!("{version}+{sha}"),
        (None, Some(date)) => format!("{version} ({date})"),
        // No VCS info at all: emit clean version — still fully traceable via Cargo.toml
        (None, None) => version,
    };

    println!("cargo:rustc-env=RUB_BUILD_VERSION={build_version}");
    emit_git_rerun_hints();
}

fn command_output(command: &[&str]) -> Option<String> {
    let (program, args) = command.split_first()?;
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn emit_git_rerun_hints() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()));
    let git_hint = manifest_dir.join("../../.git");
    let Some(git_dir) = resolve_git_dir(&git_hint) else {
        println!("cargo:rerun-if-changed={}", git_hint.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_hint.join("refs").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_hint.join("packed-refs").display()
        );
        return;
    };

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    if let Some(head_ref_path) = resolve_head_ref_path(&git_dir) {
        println!("cargo:rerun-if-changed={}", head_ref_path.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
}

fn resolve_git_dir(git_hint: &Path) -> Option<PathBuf> {
    if git_hint.is_dir() {
        return Some(git_hint.to_path_buf());
    }
    let contents = std::fs::read_to_string(git_hint).ok()?;
    let gitdir = contents.strip_prefix("gitdir:")?.trim();
    let path = Path::new(gitdir);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        git_hint.parent()?.join(path)
    })
}

fn resolve_head_ref_path(git_dir: &Path) -> Option<PathBuf> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    Some(git_dir.join(reference))
}
