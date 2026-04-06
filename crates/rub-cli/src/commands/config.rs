use clap::Parser;
use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::{
    RubPaths, default_rub_home as default_rub_home_path, validate_session_name,
};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

use super::Commands;

/// rub — Rust Browser Automation CLI for AI agents
#[derive(Debug, Clone, Parser)]
#[command(
    name = "rub",
    version = env!("RUB_BUILD_VERSION"),
    about = "Browser automation for AI agents"
)]
pub struct Cli {
    /// Session name (default: "default")
    #[arg(
        long,
        default_value = "default",
        env = "RUB_SESSION",
        global = true,
        value_parser = parse_session_name
    )]
    pub session: String,

    /// Internal daemon authority id (hidden; set by parent bootstrap only)
    #[arg(long, hide = true, global = true)]
    pub session_id: Option<String>,

    /// RUB_HOME directory override
    #[arg(long, env = "RUB_HOME", global = true)]
    pub rub_home: Option<String>,

    /// Command timeout in milliseconds
    #[arg(long, global = true)]
    pub timeout: Option<u64>,

    /// Launch Chrome with a visible window instead of headless mode
    #[arg(long, global = true)]
    pub headed: bool,

    /// Ignore certificate errors (self-signed, expired, etc.)
    #[arg(long, env = "RUB_IGNORE_CERT_ERRORS", global = true)]
    pub ignore_cert_errors: bool,

    /// Use a specific Chrome user-data-dir / profile
    #[arg(long, env = "RUB_USER_DATA_DIR", global = true)]
    pub user_data_dir: Option<String>,

    /// Show the Chrome automation infobar instead of hiding it
    #[arg(long, env = "RUB_SHOW_INFOBARS", global = true)]
    pub show_infobars: bool,

    /// Pretty-print JSON output
    #[arg(long = "json-pretty", alias = "json", global = true)]
    pub json_pretty: bool,

    /// Include a lightweight interaction trace summary in CLI output
    #[arg(long, global = true, conflicts_with = "trace")]
    pub verbose: bool,

    /// Include the full interaction trace with observed effects in CLI output
    #[arg(long, global = true, conflicts_with = "verbose")]
    pub trace: bool,

    /// Connect to an external Chrome via CDP URL (ws:// or http://)
    #[arg(long, global = true)]
    pub cdp_url: Option<String>,

    /// Auto-discover and connect to a locally-running Chrome (ports 9222-9229)
    #[arg(long, global = true)]
    pub connect: bool,

    /// Connect using a named Chrome profile (e.g., "Default", "Work")
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// Disable the L1 stealth baseline and launch-arg minimization while
    /// keeping DOM hygiene enabled for snapshot correctness
    #[arg(long, global = true)]
    pub no_stealth: bool,

    /// Enable L2 humanized interaction (mouse movement, typing delay)
    #[arg(long, env = "RUB_HUMANIZE", global = true)]
    pub humanize: bool,

    /// Humanize speed preset: fast, normal, slow
    #[arg(
        long,
        env = "RUB_HUMANIZE_SPEED",
        value_parser = ["fast", "normal", "slow"],
        default_value = "normal",
        global = true
    )]
    pub humanize_speed: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub default_timeout_ms: Option<u64>,
    pub headed: Option<bool>,
    pub ignore_cert_errors: Option<bool>,
    pub user_data_dir: Option<String>,
    pub hide_infobars: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct EffectiveCli {
    pub session: String,
    pub session_id: Option<String>,
    pub rub_home: PathBuf,
    pub timeout: u64,
    pub headed: bool,
    pub ignore_cert_errors: bool,
    pub user_data_dir: Option<String>,
    pub hide_infobars: bool,
    pub json_pretty: bool,
    pub verbose: bool,
    pub trace: bool,
    pub command: Commands,
    pub cdp_url: Option<String>,
    pub connect: bool,
    pub profile: Option<String>,
    pub no_stealth: bool,
    pub humanize: bool,
    pub humanize_speed: String,
    pub requested_launch_policy: RequestedLaunchPolicy,
    pub effective_launch_policy: RequestedLaunchPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestedLaunchPolicy {
    pub headed: bool,
    pub ignore_cert_errors: bool,
    pub user_data_dir: Option<String>,
    pub show_infobars: bool,
    pub no_stealth: bool,
    pub humanize: bool,
    pub humanize_speed: Option<String>,
}

impl RequestedLaunchPolicy {
    pub fn has_any(&self) -> bool {
        self.headed
            || self.ignore_cert_errors
            || self.user_data_dir.is_some()
            || self.show_infobars
            || self.no_stealth
            || self.humanize
            || self.humanize_speed.is_some()
    }
}

impl Cli {
    pub fn effective(self) -> Result<EffectiveCli, RubError> {
        let requested_launch_policy = RequestedLaunchPolicy {
            headed: self.headed,
            ignore_cert_errors: self.ignore_cert_errors,
            user_data_dir: self.user_data_dir.clone(),
            show_infobars: self.show_infobars,
            no_stealth: self.no_stealth || std::env::var("RUB_STEALTH").as_deref() == Ok("0"),
            humanize: self.humanize,
            humanize_speed: (self.humanize_speed != "normal"
                || std::env::var_os("RUB_HUMANIZE_SPEED").is_some())
            .then(|| self.humanize_speed.clone()),
        };
        let rub_home = self
            .rub_home
            .as_deref()
            .map(normalize_rub_home_path)
            .unwrap_or_else(default_rub_home_path);
        let file_config = load_file_config(&rub_home)?;
        let effective_headed = self.headed || file_config.headed.unwrap_or(false);
        let effective_ignore_cert_errors =
            self.ignore_cert_errors || file_config.ignore_cert_errors.unwrap_or(false);
        let effective_user_data_dir = self.user_data_dir.clone().or(file_config.user_data_dir);
        let effective_hide_infobars = if self.show_infobars {
            false
        } else {
            file_config.hide_infobars.unwrap_or(true)
        };
        let effective_no_stealth =
            self.no_stealth || std::env::var("RUB_STEALTH").as_deref() == Ok("0");
        let effective_humanize = self.humanize;
        let effective_humanize_speed = self.humanize_speed.clone();
        let effective_launch_policy = RequestedLaunchPolicy {
            headed: effective_headed,
            ignore_cert_errors: effective_ignore_cert_errors,
            user_data_dir: effective_user_data_dir.clone(),
            show_infobars: !effective_hide_infobars,
            no_stealth: effective_no_stealth,
            humanize: effective_humanize,
            humanize_speed: (effective_humanize_speed != "normal"
                || std::env::var_os("RUB_HUMANIZE_SPEED").is_some())
            .then_some(effective_humanize_speed.clone()),
        };

        Ok(EffectiveCli {
            session: self.session,
            session_id: self.session_id,
            rub_home,
            timeout: self
                .timeout
                .or(file_config.default_timeout_ms)
                .unwrap_or(30_000),
            headed: effective_headed,
            ignore_cert_errors: effective_ignore_cert_errors,
            user_data_dir: effective_user_data_dir,
            hide_infobars: effective_hide_infobars,
            json_pretty: self.json_pretty,
            verbose: self.verbose,
            trace: self.trace,
            command: self.command,
            cdp_url: self.cdp_url,
            connect: self.connect,
            profile: self.profile,
            no_stealth: effective_no_stealth,
            humanize: effective_humanize,
            humanize_speed: effective_humanize_speed,
            requested_launch_policy,
            effective_launch_policy,
        })
    }
}

fn parse_session_name(value: &str) -> Result<String, String> {
    validate_session_name(value)?;
    Ok(value.to_string())
}

fn normalize_rub_home_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let normalized = collapse_path_components(&absolute);
    if let Ok(canonical) = normalized.canonicalize() {
        return canonical;
    }
    if let Some(canonicalized) = canonicalize_existing_ancestor(&normalized) {
        return canonicalized;
    }
    normalized
}

fn collapse_path_components(path: &Path) -> PathBuf {
    let mut normalized = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut probe = path;
    let mut suffix = Vec::new();

    while !probe.exists() {
        let component = probe.file_name()?.to_os_string();
        suffix.push(component);
        probe = probe.parent()?;
    }

    let mut canonical = probe.canonicalize().ok()?;
    for component in suffix.iter().rev() {
        canonical.push(component);
    }
    Some(collapse_path_components(&canonical))
}

pub fn load_file_config(rub_home: &Path) -> Result<FileConfig, RubError> {
    let path = RubPaths::new(rub_home).config_path();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileConfig::default());
        }
        Err(error) => {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Failed to read config.toml {}: {error}", path.display()),
                serde_json::json!({
                    "reason": "config_read_failed",
                    "path": path,
                }),
            ));
        }
    };

    toml::from_str(&contents).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Failed to parse config.toml: {error}"),
            serde_json::json!({
                "reason": "invalid_config_toml",
                "path": path,
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Cli, load_file_config, normalize_rub_home_path};
    use crate::commands::{
        Commands, OrchestrationSubcommand, RuntimeSubcommand, TriggerSubcommand,
    };
    use clap::Parser;

    #[test]
    fn json_alias_enables_pretty_output() {
        let cli = Cli::try_parse_from(["rub", "--json", "doctor"]).expect("cli should parse");
        assert!(cli.json_pretty);
    }

    #[test]
    fn screenshot_positional_path_parses() {
        let cli = Cli::try_parse_from(["rub", "screenshot", "shot.png"]).expect("cli should parse");
        match cli.command {
            Commands::Screenshot { path, .. } => {
                assert_eq!(path.as_deref(), Some("shot.png"));
            }
            other => panic!("expected screenshot command, got {other:?}"),
        }
    }

    #[test]
    fn scroll_y_shorthand_parses() {
        let cli = Cli::try_parse_from(["rub", "scroll", "--y", "-320"]).expect("cli should parse");
        match cli.command {
            Commands::Scroll { y, .. } => assert_eq!(y, Some(-320)),
            other => panic!("expected scroll command, got {other:?}"),
        }
    }

    #[test]
    fn orchestration_execute_id_flag_parses() {
        let cli = Cli::try_parse_from(["rub", "orchestration", "execute", "--id", "7"])
            .expect("cli should parse");
        match cli.command {
            Commands::Orchestration {
                subcommand: OrchestrationSubcommand::Execute { id, id_option },
            } => {
                assert_eq!(id, None);
                assert_eq!(id_option, Some(7));
            }
            other => panic!("expected orchestration execute command, got {other:?}"),
        }
    }

    #[test]
    fn paused_add_flags_parse() {
        let trigger =
            Cli::try_parse_from(["rub", "trigger", "add", "--file", "rule.json", "--paused"])
                .expect("trigger add should parse");
        match trigger.command {
            Commands::Trigger {
                subcommand: TriggerSubcommand::Add { paused, .. },
            } => assert!(paused),
            other => panic!("expected trigger add command, got {other:?}"),
        }

        let orchestration = Cli::try_parse_from([
            "rub",
            "orchestration",
            "add",
            "--file",
            "rule.json",
            "--paused",
        ])
        .expect("orchestration add should parse");
        match orchestration.command {
            Commands::Orchestration {
                subcommand: OrchestrationSubcommand::Add { paused, .. },
            } => assert!(paused),
            other => panic!("expected orchestration add command, got {other:?}"),
        }
    }

    #[test]
    fn runtime_subcommand_still_parses_after_surface_growth() {
        let cli =
            Cli::try_parse_from(["rub", "runtime", "orchestration"]).expect("cli should parse");
        match cli.command {
            Commands::Runtime {
                subcommand: Some(RuntimeSubcommand::Orchestration),
            } => {}
            other => panic!("expected runtime orchestration command, got {other:?}"),
        }
    }

    #[test]
    fn type_index_example_requires_explicit_index_flag() {
        let cli = Cli::try_parse_from(["rub", "type", "--index", "5", "hello"])
            .expect("cli should parse");
        match cli.command {
            Commands::Type { index, text, .. } => {
                assert_eq!(index, Some(5));
                assert_eq!(text, "hello");
            }
            other => panic!("expected type command, got {other:?}"),
        }

        let error = Cli::try_parse_from(["rub", "type", "5", "hello"]).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("unexpected argument"), "{rendered}");
    }

    #[test]
    fn invalid_humanize_speed_is_rejected_at_parse_time() {
        let error = Cli::try_parse_from(["rub", "--humanize-speed", "warp", "doctor"]).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("fast"), "{rendered}");
        assert!(rendered.contains("normal"), "{rendered}");
        assert!(rendered.contains("slow"), "{rendered}");
    }

    #[test]
    fn invalid_session_name_is_rejected_at_parse_time() {
        for value in ["../x", "a/b", "/tmp/x", "..", "."] {
            let error = Cli::try_parse_from(["rub", "--session", value, "doctor"]).unwrap_err();
            let rendered = error.to_string();
            assert!(rendered.contains("session name"), "{rendered}");
        }
    }

    #[test]
    fn rub_home_normalization_collapses_relative_segments() {
        let cwd = std::env::current_dir().expect("cwd");
        let normalized = normalize_rub_home_path("./tmp/../rub-home");
        assert_eq!(normalized, cwd.join("rub-home"));
        assert!(normalized.is_absolute());
    }

    #[test]
    fn rub_home_normalization_resolves_existing_ancestor_symlinks() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let root = std::env::temp_dir().join(format!(
                "rub-home-normalize-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
            let real_parent = root.join("real-parent");
            let alias_parent = root.join("alias-parent");
            std::fs::create_dir_all(&real_parent).expect("real parent");
            symlink(&real_parent, &alias_parent).expect("symlink parent");

            let normalized = normalize_rub_home_path(alias_parent.join("child-home"));
            let expected_parent = real_parent.canonicalize().expect("canonical parent");
            assert_eq!(normalized, expected_parent.join("child-home"));

            let _ = std::fs::remove_file(&alias_parent);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn invalid_toml_config_is_reported_instead_of_silently_ignored() {
        let home = std::env::temp_dir().join(format!(
            "rub-config-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("config home");
        std::fs::write(home.join("config.toml"), "headed = [").expect("invalid config");

        let error = load_file_config(&home)
            .expect_err("invalid toml should fail")
            .into_envelope();
        assert_eq!(error.code, rub_core::error::ErrorCode::InvalidInput);
        assert_eq!(
            error.context.expect("config error context")["reason"],
            "invalid_config_toml"
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn unreadable_config_path_is_reported_instead_of_defaulting() {
        let home = std::env::temp_dir().join(format!(
            "rub-config-read-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let config_dir = home.join("config.toml");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&config_dir).expect("config path directory");

        let error = load_file_config(&home)
            .expect_err("directory config path should fail")
            .into_envelope();
        assert_eq!(error.code, rub_core::error::ErrorCode::InvalidInput);
        assert_eq!(
            error.context.expect("config read error context")["reason"],
            "config_read_failed"
        );

        let _ = std::fs::remove_dir_all(home);
    }
}
