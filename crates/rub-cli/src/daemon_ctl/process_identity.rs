use std::path::Path;
use std::process::Command;

pub(crate) fn process_matches_registry_entry(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<bool> {
    process_matches_daemon_identity(
        rub_home,
        &entry.session_name,
        Some(entry.session_id.as_str()),
        entry.pid,
    )
}

pub(crate) fn process_matches_daemon_identity(
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
    pid: u32,
) -> std::io::Result<bool> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if command.is_empty() {
        return Ok(false);
    }
    Ok(command_matches_daemon_identity(
        &command,
        rub_home,
        session_name,
        session_id,
    ))
}

pub(crate) fn command_matches_daemon_identity(
    command: &str,
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
) -> bool {
    if !command.contains("__daemon")
        || extract_flag_value(command, "--session").as_deref() != Some(session_name)
        || extract_flag_value(command, "--rub-home").as_deref()
            != Some(rub_home.to_string_lossy().as_ref())
    {
        return false;
    }
    match session_id {
        Some(session_id) => {
            extract_flag_value(command, "--session-id").as_deref() == Some(session_id)
        }
        None => true,
    }
}

pub(crate) fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    let inline_prefix = format!("{flag}=");
    let mut parts = tokenize_command(command).into_iter();
    while let Some(part) = parts.next() {
        if part == flag {
            return parts.next();
        }
        if let Some(value) = part.strip_prefix(&inline_prefix) {
            return Some(value.to_string());
        }
    }
    None
}

fn tokenize_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single_quotes = false;
    let mut in_double_quotes = false;
    let mut escaping = false;

    for ch in command.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quotes => escaping = true,
            '\'' if !in_double_quotes => in_single_quotes = !in_single_quotes,
            '"' if !in_single_quotes => in_double_quotes = !in_double_quotes,
            ch if ch.is_whitespace() && !in_single_quotes && !in_double_quotes => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}
