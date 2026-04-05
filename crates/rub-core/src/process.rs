use std::collections::{HashMap, HashSet};
use std::io;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
}

pub fn process_snapshot() -> io::Result<Vec<ProcessInfo>> {
    let output = Command::new("ps")
        .args(["-Ao", "pid=,ppid=,command="])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Process snapshot command failed with status {}",
            output.status
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(parse_process_snapshot_line)
        .collect())
}

pub fn parse_process_snapshot_line(line: &str) -> Option<ProcessInfo> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (pid_part, remainder) = next_snapshot_field(trimmed)?;
    let (ppid_part, command_part) = next_snapshot_field(remainder)?;
    let pid = pid_part.parse::<u32>().ok()?;
    let ppid = ppid_part.parse::<u32>().ok()?;
    let command = command_part.trim_start().to_string();
    if command.is_empty() {
        return None;
    }

    Some(ProcessInfo { pid, ppid, command })
}

pub fn process_tree(snapshot: &[ProcessInfo], root_pid: u32) -> HashSet<u32> {
    let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in snapshot {
        children_by_parent
            .entry(process.ppid)
            .or_default()
            .push(process.pid);
    }

    let mut tree = HashSet::new();
    let mut stack = vec![root_pid];
    while let Some(pid) = stack.pop() {
        if !tree.insert(pid) {
            continue;
        }
        if let Some(children) = children_by_parent.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
    tree
}

pub fn process_has_ancestor(snapshot: &[ProcessInfo], pid: u32, ancestors: &HashSet<u32>) -> bool {
    let parent_by_pid: HashMap<u32, u32> = snapshot.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut current = Some(pid);
    let mut seen = HashSet::new();
    while let Some(pid) = current {
        if !seen.insert(pid) {
            break;
        }
        if ancestors.contains(&pid) {
            return true;
        }
        current = parent_by_pid.get(&pid).copied();
    }
    false
}

pub fn is_process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    let errno = io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or_default();
    errno == libc::EPERM
}

pub fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
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

pub fn is_chromium_browser_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.contains("helper") {
        return false;
    }
    let prefix = lower.split(" --").next().unwrap_or(&lower).trim();
    prefix.ends_with("google chrome")
        || prefix.ends_with("chrome")
        || prefix.ends_with("google-chrome")
        || prefix.ends_with("chromium")
        || prefix.ends_with("chromium-browser")
        || prefix.ends_with("msedge")
        || prefix.ends_with("microsoft edge")
        || prefix.ends_with("/google chrome")
        || prefix.ends_with("/chrome")
        || prefix.ends_with("/google-chrome")
        || prefix.ends_with("/chromium")
        || prefix.ends_with("/chromium-browser")
        || prefix.ends_with("/msedge")
}

pub fn is_browser_root_process(command: &str) -> bool {
    !command.contains("--type=") && is_chromium_browser_command(command)
}

pub fn tokenize_command(command: &str) -> Vec<String> {
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
            '\\' if !in_single_quotes => {
                escaping = true;
            }
            '\'' if !in_double_quotes => {
                in_single_quotes = !in_single_quotes;
            }
            '"' if !in_single_quotes => {
                in_double_quotes = !in_double_quotes;
            }
            c if c.is_whitespace() && !in_single_quotes && !in_double_quotes => {
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

fn next_snapshot_field(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    let split_at = trimmed.find(char::is_whitespace)?;
    let (field, remainder) = trimmed.split_at(split_at);
    Some((field, remainder))
}

#[cfg(test)]
mod tests {
    use super::{
        extract_flag_value, is_browser_root_process, is_chromium_browser_command,
        parse_process_snapshot_line, process_has_ancestor, process_tree, tokenize_command,
    };
    use std::collections::HashSet;

    #[test]
    fn parse_process_snapshot_line_preserves_command_with_embedded_spaces() {
        let parsed = parse_process_snapshot_line(
            r#"  123  1 Google Chrome --user-data-dir="/tmp/rub chrome 300" --remote-debugging-port=0"#,
        )
        .expect("snapshot line should parse");
        assert_eq!(parsed.pid, 123);
        assert_eq!(parsed.ppid, 1);
        assert_eq!(
            extract_flag_value(&parsed.command, "--user-data-dir"),
            Some("/tmp/rub chrome 300".to_string())
        );
    }

    #[test]
    fn extract_flag_value_handles_quoted_paths_with_spaces() {
        let inline =
            r#"Google Chrome --user-data-dir="/tmp/rub chrome 100" --remote-debugging-port=0"#;
        assert_eq!(
            extract_flag_value(inline, "--user-data-dir"),
            Some("/tmp/rub chrome 100".to_string())
        );

        let separated =
            r#"Google Chrome --user-data-dir "/tmp/rub chrome 200" --remote-debugging-port=0"#;
        assert_eq!(
            extract_flag_value(separated, "--user-data-dir"),
            Some("/tmp/rub chrome 200".to_string())
        );
    }

    #[test]
    fn process_tree_collects_descendants() {
        let snapshot = vec![
            super::ProcessInfo {
                pid: 1,
                ppid: 0,
                command: "root".to_string(),
            },
            super::ProcessInfo {
                pid: 2,
                ppid: 1,
                command: "child".to_string(),
            },
            super::ProcessInfo {
                pid: 3,
                ppid: 2,
                command: "grandchild".to_string(),
            },
        ];
        let tree = process_tree(&snapshot, 1);
        assert_eq!(tree, HashSet::from([1, 2, 3]));
    }

    #[test]
    fn process_has_ancestor_uses_pid_chain() {
        let snapshot = vec![
            super::ProcessInfo {
                pid: 10,
                ppid: 1,
                command: "root".to_string(),
            },
            super::ProcessInfo {
                pid: 11,
                ppid: 10,
                command: "child".to_string(),
            },
        ];
        assert!(process_has_ancestor(&snapshot, 11, &HashSet::from([10])));
    }

    #[test]
    fn chromium_command_filters_helpers() {
        assert!(is_chromium_browser_command(
            "Google Chrome --user-data-dir=/tmp/profile"
        ));
        assert!(!is_chromium_browser_command(
            "Google Chrome Helper --type=renderer --user-data-dir=/tmp/profile"
        ));
        assert!(is_browser_root_process(
            "Google Chrome --user-data-dir=/tmp/profile --remote-debugging-port=0"
        ));
        assert!(!is_browser_root_process(
            "Google Chrome --type=renderer --user-data-dir=/tmp/profile"
        ));
    }

    #[test]
    fn tokenize_command_preserves_quoted_segments() {
        let parts =
            tokenize_command(r#"rub __daemon --rub-home "/tmp/rub home" --session default"#);
        assert_eq!(
            parts,
            vec![
                "rub",
                "__daemon",
                "--rub-home",
                "/tmp/rub home",
                "--session",
                "default",
            ]
        );
    }
}
