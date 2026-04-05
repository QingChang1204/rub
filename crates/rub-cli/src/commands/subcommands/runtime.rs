use clap::Subcommand;

use crate::commands::{DownloadWaitStateArg, InterferenceModeArg, StorageAreaArg};

/// Subcommands for `rub cookies`.
#[derive(Debug, Clone, Subcommand)]
pub enum CookiesSubcommand {
    /// Get all cookies (optionally filtered by URL)
    Get {
        /// Only return cookies that would be sent to this URL
        #[arg(long)]
        url: Option<String>,
    },
    /// Set a cookie
    Set {
        /// Cookie name
        name: String,
        /// Cookie value
        value: String,
        /// Cookie domain
        #[arg(long)]
        domain: Option<String>,
        /// Cookie path
        #[arg(long, default_value = "/")]
        path: String,
        /// Secure flag
        #[arg(long)]
        secure: bool,
        /// HttpOnly flag
        #[arg(long)]
        http_only: bool,
        /// SameSite policy (Strict, Lax, None)
        #[arg(long)]
        same_site: Option<String>,
        /// Expiration time as Unix timestamp in seconds
        #[arg(long)]
        expires: Option<f64>,
    },
    /// Clear all cookies (or for a specific URL)
    Clear {
        /// URL to scope clearing
        #[arg(long)]
        url: Option<String>,
    },
    /// Export cookies to a JSON file
    Export {
        /// File path
        path: String,
    },
    /// Import cookies from a JSON file
    Import {
        /// File path
        path: String,
    },
}

/// Subcommands for `rub handoff`.
#[derive(Debug, Clone, Subcommand)]
pub enum HandoffSubcommand {
    /// Show current handoff status
    Status,
    /// Pause automation and hand control to a user
    Start,
    /// Mark human verification as completed and resume automation
    Complete,
}

/// Subcommands for `rub takeover`.
#[derive(Debug, Clone, Subcommand)]
pub enum TakeoverSubcommand {
    /// Show current session takeover/accessibility status
    Status,
    /// Pause automation and hand control to a user when the session is accessible
    Start,
    /// Relaunch a managed headless session into a visible browser when supported
    Elevate,
    /// Resume automation after manual takeover work is complete
    Resume,
}

/// Subcommands for `rub dialog`.
#[derive(Debug, Clone, Subcommand)]
pub enum DialogSubcommand {
    /// Show current JavaScript dialog runtime state
    Status,
    /// Accept the pending dialog
    Accept {
        /// Prompt text to supply before accepting a prompt dialog
        #[arg(long = "prompt-text")]
        prompt_text: Option<String>,
    },
    /// Dismiss the pending dialog
    Dismiss,
}

/// Subcommands for `rub intercept`.
#[derive(Debug, Clone, Subcommand)]
pub enum InterceptSubcommand {
    /// Rewrite matching requests to a different base URL
    Rewrite {
        /// Source URL pattern (exact match or trailing-* prefix pattern)
        source_pattern: String,
        /// Target base URL
        target_base: String,
    },
    /// Block matching requests
    Block {
        /// URL pattern to block
        url_pattern: String,
    },
    /// Explicitly allow matching requests to pass through unchanged
    Allow {
        /// URL pattern to allow
        url_pattern: String,
    },
    /// Override request headers for matching requests
    Header {
        /// URL pattern to match
        url_pattern: String,
        /// Single header name for the intuitive `header <pattern> <NAME> <VALUE>` form
        #[arg(value_name = "NAME", requires = "value", conflicts_with = "headers")]
        name: Option<String>,
        /// Single header value for the intuitive `header <pattern> <NAME> <VALUE>` form
        #[arg(value_name = "VALUE", requires = "name", conflicts_with = "headers")]
        value: Option<String>,
        /// Header override in NAME=VALUE form (repeatable)
        #[arg(long = "header", value_name = "NAME=VALUE", conflicts_with_all = ["name", "value"])]
        headers: Vec<String>,
    },
    /// List active session-scoped request rules
    List,
    /// Remove a rule by stable id
    Remove {
        /// Rule id returned by `rub intercept list`
        id: u32,
    },
    /// Clear all request rules
    Clear,
}

/// Subcommands for `rub interference`.
#[derive(Debug, Clone, Subcommand)]
pub enum InterferenceSubcommand {
    /// Set the session-scoped public-web interference mode
    Mode {
        /// Policy mode to apply to this session
        mode: InterferenceModeArg,
    },
    /// Attempt safe recovery for the current classified interference
    Recover,
}

/// Subcommands for `rub download`.
#[derive(Debug, Clone, Subcommand)]
pub enum DownloadSubcommand {
    /// Wait for a download to reach the requested state
    Wait {
        /// Specific download GUID to wait for
        #[arg(long)]
        id: Option<String>,
        /// Desired terminal or lifecycle state (default: completed)
        #[arg(long, value_enum, default_value = "completed")]
        state: DownloadWaitStateArg,
    },
    /// Cancel an in-progress download by GUID
    Cancel {
        /// Download GUID returned by `rub downloads`
        id: String,
    },
    /// Save a batch of explicit asset URLs to disk
    Save {
        /// Source file containing URLs or JSON rows
        #[arg(long, value_name = "PATH")]
        file: String,
        /// Output directory for saved assets
        #[arg(long, value_name = "DIR")]
        output_dir: String,
        /// Dot-path to the array inside a JSON source document (for example `fields.items`)
        #[arg(long, value_name = "PATH")]
        input_field: Option<String>,
        /// Dot-path to the URL field inside each JSON row
        #[arg(long, value_name = "FIELD")]
        url_field: Option<String>,
        /// Dot-path to an optional source name field inside each JSON row
        #[arg(long, value_name = "FIELD")]
        name_field: Option<String>,
        /// Base URL used to resolve relative asset URLs
        #[arg(long, value_name = "URL")]
        base_url: Option<String>,
        /// Use this URL as page context / Referer while fetching the assets
        #[arg(long, value_name = "URL")]
        cookie_url: Option<String>,
        /// Only save the first N parsed asset sources
        #[arg(long, value_name = "COUNT")]
        limit: Option<u32>,
        /// Number of concurrent fetches (default: 6)
        #[arg(long, value_name = "COUNT", default_value_t = 6)]
        concurrency: u32,
        /// Overwrite existing files instead of skipping them
        #[arg(long)]
        overwrite: bool,
    },
}

/// Subcommands for `rub runtime`.
#[derive(Debug, Clone, Subcommand)]
pub enum RuntimeSubcommand {
    /// Show all canonical runtime surfaces together
    Summary,
    /// Show current JavaScript dialog runtime state
    Dialog,
    /// Show current frame runtime context
    Frame,
    /// Show integration runtime status and request-rule state
    Integration,
    /// Show public-web interference runtime state
    Interference,
    /// Show recent runtime observatory events and summaries
    Observatory,
    /// Show auth/session storage visibility
    #[command(name = "state-inspector")]
    StateInspector,
    /// Show readiness/stabilization heuristics
    Readiness,
    /// Show human verification handoff state
    Handoff,
    /// Show download runtime state
    Downloads,
    /// Show storage runtime state
    Storage,
    /// Show takeover/accessibility runtime state
    Takeover,
    /// Show cross-session orchestration foundation state
    Orchestration,
    /// Show trigger registry runtime state
    Trigger,
}

/// Subcommands for `rub trigger`.
#[derive(Debug, Clone, Subcommand)]
pub enum TriggerSubcommand {
    /// Register a trigger spec from a JSON file
    Add {
        /// Trigger JSON specification file
        #[arg(long, value_name = "PATH")]
        file: String,
        /// Register the trigger but keep it paused until explicitly resumed
        #[arg(long)]
        paused: bool,
    },
    /// List configured triggers and current registry health
    List,
    /// Show recent trigger lifecycle/outcome events from the dedicated trace surface
    Trace {
        /// Max number of recent events to return
        #[arg(long, default_value_t = 20)]
        last: u32,
    },
    /// Remove a trigger by stable id
    Remove {
        /// Trigger id returned by `rub trigger list`
        id: u32,
    },
    /// Pause an armed trigger without deleting it
    Pause {
        /// Trigger id returned by `rub trigger list`
        id: u32,
    },
    /// Resume a paused trigger
    Resume {
        /// Trigger id returned by `rub trigger list`
        id: u32,
    },
}

/// Subcommands for `rub orchestration`.
#[derive(Debug, Clone, Subcommand)]
pub enum OrchestrationSubcommand {
    /// Register an orchestration rule spec from a JSON file or named asset
    Add {
        /// Orchestration JSON specification file
        #[arg(long, value_name = "PATH", conflicts_with = "asset")]
        file: Option<String>,
        /// Load a named orchestration asset from RUB_HOME/orchestrations/<name>.json
        #[arg(long, value_name = "NAME", conflicts_with = "file")]
        asset: Option<String>,
        /// Register the rule but keep it paused until explicitly resumed
        #[arg(long)]
        paused: bool,
    },
    /// List configured orchestration rules and current registry health
    List,
    /// List saved orchestration assets under RUB_HOME/orchestrations
    ListAssets,
    /// Show recent orchestration lifecycle events from the dedicated trace surface
    Trace {
        /// Max number of recent events to return
        #[arg(long, default_value_t = 20)]
        last: u32,
    },
    /// Remove an orchestration rule by stable id
    Remove {
        /// Orchestration rule id returned by `rub orchestration list`
        id: u32,
    },
    /// Pause an armed orchestration rule without deleting it
    Pause {
        /// Orchestration rule id returned by `rub orchestration list`
        id: u32,
    },
    /// Resume a paused orchestration rule
    Resume {
        /// Orchestration rule id returned by `rub orchestration list`
        id: u32,
    },
    /// Execute a registered orchestration rule once through the canonical target-session fence
    Execute {
        /// Orchestration rule id returned by `rub orchestration list`
        id: Option<u32>,
        /// Explicit id alias for discoverability
        #[arg(long = "id", conflicts_with = "id")]
        id_option: Option<u32>,
    },
    /// Export a registered orchestration rule as a reusable asset spec
    Export {
        /// Orchestration rule id returned by `rub orchestration list`
        id: u32,
        /// Save under RUB_HOME/orchestrations/<name>.json
        #[arg(long, value_name = "NAME")]
        save_as: Option<String>,
        /// Also write the exported spec to an explicit path
        #[arg(long, value_name = "PATH")]
        output: Option<String>,
    },
}

/// Subcommands for `rub storage`.
#[derive(Debug, Clone, Subcommand)]
pub enum StorageSubcommand {
    /// Read a key from current-origin storage (searches both areas unless --area is given)
    Get {
        key: String,
        #[arg(long, value_enum)]
        area: Option<StorageAreaArg>,
    },
    /// Set one current-origin storage item
    Set {
        key: String,
        value: String,
        #[arg(long, value_enum)]
        area: Option<StorageAreaArg>,
    },
    /// Remove one current-origin storage item (both areas when --area is omitted)
    Remove {
        key: String,
        #[arg(long, value_enum)]
        area: Option<StorageAreaArg>,
    },
    /// Clear one storage area or both areas for the current origin
    Clear {
        #[arg(long, value_enum)]
        area: Option<StorageAreaArg>,
    },
    /// Export current-origin storage to JSON (and optionally write it to a file)
    Export {
        #[arg(long)]
        path: Option<String>,
    },
    /// Import a storage snapshot JSON file into the current origin
    Import { path: String },
}
