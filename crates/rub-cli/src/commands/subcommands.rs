use clap::Subcommand;
use rub_core::DEFAULT_WAIT_TIMEOUT_MS;

use super::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, StateFormatArg,
    WaitAfterArgs,
};

mod explain;
mod help;
mod meta;
mod query;
mod runtime;

#[cfg(test)]
pub(crate) use self::meta::render_nested_subcommand_long_help;
pub use explain::ExplainSubcommand;
pub use query::{GetSubcommand, InspectSubcommand};
pub use runtime::{
    BindingCaptureAuthInputArg, BindingSubcommand, CookiesSubcommand, DialogSubcommand,
    DownloadSubcommand, HandoffSubcommand, InterceptSubcommand, InterferenceSubcommand,
    OrchestrationSubcommand, RememberedBindingAliasKindArg, RuntimeSubcommand, SecretSubcommand,
    StorageSubcommand, TakeoverSubcommand, TriggerSubcommand,
};

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Navigate to a URL
    Open {
        /// Target URL
        url: String,
        /// Load strategy: load, domcontentloaded, networkidle
        #[arg(long, default_value = "load")]
        load_strategy: String,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Get current page state (DOM snapshot)
    State {
        /// Max number of elements to return
        #[arg(long)]
        limit: Option<u32>,
        /// Output projection format (default: snapshot)
        #[arg(long, value_enum)]
        format: Option<StateFormatArg>,
        /// Hidden positional alias for common `rub state compact`-style input.
        #[arg(value_enum, hide = true, conflicts_with = "format")]
        format_alias: Option<StateFormatArg>,
        /// Include accessibility information (ARIA roles, names)
        #[arg(long)]
        a11y: bool,
        /// Only include elements visible in the current viewport
        #[arg(long)]
        viewport: bool,
        /// Compare against a previous snapshot (returns diff only)
        #[arg(long)]
        diff: Option<String>,
        /// Detect JS event listeners on elements
        #[arg(long)]
        listeners: bool,
        #[command(flatten)]
        scope: ObservationScopeArgs,
        #[command(flatten)]
        projection: ObservationProjectionArgs,
    },

    /// Atomically capture a token-friendly page summary plus screenshot.
    #[command(long_about = help::OBSERVE_LONG_ABOUT)]
    Observe {
        /// Save the screenshot to a file path (otherwise base64 in JSON)
        #[arg(long)]
        path: Option<String>,
        /// Capture a full-page screenshot
        #[arg(long)]
        full: bool,
        /// Max number of elements to summarize in the shared snapshot
        #[arg(long)]
        limit: Option<u32>,
        #[command(flatten)]
        scope: ObservationScopeArgs,
        #[command(flatten)]
        projection: ObservationProjectionArgs,
    },

    /// Find matching elements through canonical locator semantics
    Find {
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Search live DOM content anchors instead of only interactive snapshot elements
        #[arg(long)]
        content: bool,
        /// Project the candidate set through the read-only locator explain surface
        #[arg(long, conflicts_with_all = ["content", "limit"])]
        explain: bool,
        /// Max number of matches to return
        #[arg(long)]
        limit: Option<u32>,
    },

    /// Click an element by index (or at coordinates with --xy)
    Click {
        /// Element index from last state (not needed with --xy)
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Click at raw coordinates x,y (e.g. --xy 450 300)
        #[arg(long, num_args = 2, value_names = ["X", "Y"])]
        xy: Option<Vec<f64>>,
        /// Dispatch a double-click instead of a single click
        #[arg(long, conflicts_with = "right")]
        double: bool,
        /// Dispatch a right-click instead of a left click
        #[arg(long, conflicts_with = "double")]
        right: bool,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Execute JavaScript
    Exec {
        /// JavaScript code
        code: String,
        /// Print the result directly instead of the standard JSON envelope
        #[arg(long)]
        raw: bool,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Explain how canonical CLI surfaces will interpret a command shape
    Explain {
        #[command(subcommand)]
        subcommand: ExplainSubcommand,
    },

    /// Scroll the viewport.
    ///
    /// Examples:
    ///   rub scroll                   # scroll down 600px (default)
    ///   rub scroll up --amount 300   # scroll up 300px
    ///   rub scroll --y -300          # scroll up 300px (signed delta syntax)
    ///   rub scroll --y 500           # scroll down 500px
    Scroll {
        /// Direction: up or down (default: down). Ignored when --y is set.
        #[arg(default_value = "down")]
        direction: String,
        /// Pixels to scroll as a positive integer (default: 600).
        /// Mutually exclusive with --y.
        #[arg(long)]
        amount: Option<u32>,
        /// Signed pixel delta: negative = scroll up, positive = scroll down.
        /// (e.g. --y -300 scrolls up 300px). Mutually exclusive with --amount.
        #[arg(long, conflicts_with = "amount", allow_hyphen_values = true)]
        y: Option<i32>,
    },

    /// Navigate back in history
    Back {
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Navigate forward in history
    Forward {
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Reload the current page
    Reload {
        /// Load strategy: load, domcontentloaded, networkidle
        #[arg(long, default_value = "load")]
        load_strategy: String,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Take a screenshot
    Screenshot {
        /// Save to file path (otherwise base64 in JSON)
        path: Option<String>,
        /// Explicit output path alias for `screenshot <path>`
        #[arg(long = "path", conflicts_with = "path")]
        output_path: Option<String>,
        /// Capture full page
        #[arg(long)]
        full: bool,
        /// Include visual index overlays on interactive elements
        #[arg(long)]
        highlight: bool,
    },

    /// Close the session browser; the daemon exits later on idle timeout or shutdown
    Close {
        /// Close all active sessions (not just the current one)
        #[arg(long)]
        all: bool,
    },

    /// List active sessions
    Sessions,

    /// Inspect and manage named authenticated-runtime bindings stored under RUB_HOME
    Binding {
        #[command(subcommand)]
        subcommand: BindingSubcommand,
    },

    /// Inspect and manage explicit secret references stored under RUB_HOME/secrets.env
    Secret {
        #[command(subcommand)]
        subcommand: SecretSubcommand,
    },

    /// System health check
    Doctor,

    /// Query canonical runtime integration surfaces
    Runtime {
        #[command(subcommand)]
        subcommand: Option<RuntimeSubcommand>,
    },

    /// Session-scoped cross-tab trigger registry controls
    Trigger {
        #[command(subcommand)]
        subcommand: TriggerSubcommand,
    },

    /// Cross-session orchestration rule registry controls
    Orchestration {
        #[command(subcommand)]
        subcommand: OrchestrationSubcommand,
    },

    /// List the live frame inventory for the current page context
    Frames,

    /// Select the current frame context for subsequent snapshot/query/interaction commands
    Frame {
        /// Frame inventory index from `rub frames`
        index: Option<u32>,
        /// Select by live frame name
        #[arg(long)]
        name: Option<String>,
        /// Reset to the top/primary frame
        #[arg(long)]
        top: bool,
    },

    /// Clean stale session state and orphaned temporary rub/browser artifacts
    Cleanup,

    /// Canonical lifecycle exit for one RUB_HOME.
    #[command(long_about = help::TEARDOWN_LONG_ABOUT)]
    Teardown,

    /// Show recent session-scoped command history
    History {
        /// Number of recent entries to return
        #[arg(long, default_value = "10")]
        last: u32,
        /// Export commands starting at this workflow-capture sequence (inclusive)
        #[arg(long)]
        from: Option<u64>,
        /// Export commands ending at this workflow-capture sequence (inclusive)
        #[arg(long)]
        to: Option<u64>,
        /// Export recent successful commands as canonical pipe JSON
        #[arg(long, conflicts_with = "export_script")]
        export_pipe: bool,
        /// Export recent successful commands as a replayable shell script wrapper
        #[arg(long, conflicts_with = "export_pipe")]
        export_script: bool,
        /// Include observation-class commands when exporting a pipe workflow
        #[arg(long)]
        include_observation: bool,
        /// Save exported pipe JSON as a named workflow in RUB_HOME/workflows/<name>.json
        #[arg(long, value_name = "NAME", conflicts_with = "output")]
        save_as: Option<String>,
        /// Write the exported workflow/script asset directly to a file
        #[arg(long, value_name = "PATH", conflicts_with = "save_as")]
        output: Option<String>,
    },

    /// Show session-scoped browser download runtime state
    Downloads,

    /// Wait for, cancel, or save downloads/assets
    Download {
        #[command(subcommand)]
        subcommand: DownloadSubcommand,
    },

    /// Web storage inspection and mutation for the current frame/current origin
    #[command(subcommand)]
    Storage(StorageSubcommand),

    /// Human verification handoff controls
    Handoff {
        #[command(subcommand)]
        subcommand: Option<HandoffSubcommand>,
    },

    /// Session accessibility / human takeover controls
    Takeover {
        #[command(subcommand)]
        subcommand: Option<TakeoverSubcommand>,
    },

    /// JavaScript dialog controls
    Dialog {
        #[command(subcommand)]
        subcommand: Option<DialogSubcommand>,
    },

    /// Session-scoped network rule controls for developer integration
    Intercept {
        #[command(subcommand)]
        subcommand: InterceptSubcommand,
    },

    /// Public-web interference controls
    Interference {
        #[command(subcommand)]
        subcommand: InterferenceSubcommand,
    },

    /// Send a key press or key combination (e.g., Enter, Control+a)
    Keys {
        /// Key name or combination (e.g., "Enter", "Control+a", "Shift+Tab")
        keys: String,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Type text into the focused text target, or into a targeted element when a locator is provided
    Type {
        /// Element index from last state (optional; omitted = use active element)
        #[arg(long)]
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Clear existing content before typing when targeting a specific element
        #[arg(long)]
        clear: bool,
        /// Text to type (flag form)
        #[arg(long = "text", value_name = "TEXT", conflicts_with = "text")]
        text_flag: Option<String>,
        /// Text to type
        #[arg(value_name = "TEXT", required_unless_present = "text_flag")]
        text: Option<String>,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Wait for an element, page text, or page context condition to be met
    Wait {
        /// Wait for a CSS selector to match
        #[arg(long)]
        selector: Option<String>,
        /// Wait for an element with matching visible/accessibility text
        #[arg(long = "target-text")]
        target_text: Option<String>,
        /// Wait for an element with this semantic role
        #[arg(long)]
        role: Option<String>,
        /// Wait for an element with this accessible/visible label
        #[arg(long)]
        label: Option<String>,
        /// Wait for an element with this testing id
        #[arg(long)]
        testid: Option<String>,
        /// Wait for text to appear on page
        #[arg(long)]
        text: Option<String>,
        /// Wait for the target's accessible description to contain this substring
        #[arg(long = "description-contains")]
        description_contains: Option<String>,
        /// Wait for the current frame/page URL to contain this substring
        #[arg(long = "url-contains")]
        url_contains: Option<String>,
        /// Wait for the current frame/page title to contain this substring
        #[arg(long = "title-contains")]
        title_contains: Option<String>,
        /// Select the first match from a semantic wait locator
        #[arg(long)]
        first: bool,
        /// Select the last match from a semantic wait locator
        #[arg(long)]
        last: bool,
        /// Select the nth match from a semantic wait locator (0-based)
        #[arg(long)]
        nth: Option<u32>,
        /// Timeout in milliseconds (default: 30000)
        #[arg(long, default_value_t = DEFAULT_WAIT_TIMEOUT_MS)]
        timeout: u64,
        /// Element state to wait for: visible, hidden, attached, detached, interactable (default: visible)
        #[arg(long, default_value = "visible")]
        state: String,
    },

    /// List all browser tabs
    Tabs,

    /// Switch to a tab by index
    Switch {
        /// Tab index (0-based)
        index: u32,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Close a tab (current tab if no index specified)
    #[command(name = "close-tab")]
    CloseTab {
        /// Tab index to close (default: current tab)
        index: Option<u32>,
    },

    /// Get DOM information
    #[command(subcommand)]
    Get(GetSubcommand),

    /// Unified inspection surface for read-only queries and structured extraction
    #[command(subcommand)]
    Inspect(InspectSubcommand),

    /// Hover over an element
    Hover {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Cookie management
    #[command(subcommand)]
    Cookies(CookiesSubcommand),

    /// Upload a file to an input element
    Upload {
        /// Positional operands: `<path>` or `<index> <path>`
        #[arg(num_args = 1..=2, value_names = ["INDEX", "PATH"])]
        operands: Vec<String>,
        #[command(flatten)]
        target: ElementAddressArgs,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Select an option from a dropdown
    Select {
        /// Positional operands: `<value>` or `<index> <value>`
        #[arg(num_args = 1..=2, value_names = ["INDEX", "VALUE"])]
        operands: Vec<String>,
        /// Explicit option value/text to select when you do not want positional operands
        #[arg(long, conflicts_with = "operands")]
        value: Option<String>,
        #[command(flatten)]
        target: ElementAddressArgs,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Fill multiple form fields through the canonical interaction runtime
    #[command(
        long_about = help::FILL_LONG_ABOUT,
        after_long_help = help::FILL_AFTER_LONG_HELP
    )]
    Fill {
        /// Inline JSON fill specification (array of field descriptors)
        #[arg(conflicts_with = "file", help_heading = "Fill spec input")]
        spec: Option<String>,
        /// Load the fill specification from a JSON file
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "spec",
            help_heading = "Fill spec input"
        )]
        file: Option<String>,
        /// Validate and explain the fill plan without mutating the page
        #[arg(long, help_heading = "Planning")]
        validate: bool,
        /// Execute a strict opt-in atomic subset with explicit rollback on failure
        #[arg(long, help_heading = "Planning")]
        atomic: bool,
        /// Snapshot ID for strict preflight continuity (target resolution only)
        #[arg(long, value_name = "SNAPSHOT_ID", help_heading = "Planning")]
        snapshot: Option<String>,
        /// Optional submit button index
        #[arg(long = "submit-index", help_heading = "Submit action")]
        submit_index: Option<u32>,
        /// Optional submit selector
        #[arg(long = "submit-selector", help_heading = "Submit action")]
        submit_selector: Option<String>,
        /// Optional submit target text
        #[arg(long = "submit-target-text", help_heading = "Submit action")]
        submit_target_text: Option<String>,
        /// Optional submit ref
        #[arg(long = "submit-ref", help_heading = "Submit action")]
        submit_ref: Option<String>,
        /// Optional submit semantic role
        #[arg(long = "submit-role", help_heading = "Submit action")]
        submit_role: Option<String>,
        /// Optional submit accessible/visible label
        #[arg(long = "submit-label", help_heading = "Submit action")]
        submit_label: Option<String>,
        /// Optional submit testing id
        #[arg(long = "submit-testid", help_heading = "Submit action")]
        submit_testid: Option<String>,
        /// Select the first submit match
        #[arg(long = "submit-first", help_heading = "Submit action")]
        submit_first: bool,
        /// Select the last submit match
        #[arg(long = "submit-last", help_heading = "Submit action")]
        submit_last: bool,
        /// Select the nth submit match (0-based)
        #[arg(long = "submit-nth", help_heading = "Submit action")]
        submit_nth: Option<u32>,
        #[command(flatten, next_help_heading = "Post-action wait")]
        wait_after: WaitAfterArgs,
    },

    /// Extract structured data through the canonical query surface
    #[command(
        long_about = help::EXTRACT_LONG_ABOUT,
        after_long_help = help::EXTRACT_AFTER_LONG_HELP
    )]
    Extract {
        /// Inline JSON extract specification
        #[arg(
            conflicts_with_all = ["file", "examples", "schema"],
            help_heading = "Extract spec input"
        )]
        spec: Option<String>,
        /// Load the extract specification from a JSON file
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with_all = ["spec", "examples", "schema"],
            help_heading = "Extract spec input"
        )]
        file: Option<String>,
        /// Snapshot ID (strict mode; omitted = use an implicit live snapshot)
        #[arg(
            long,
            conflicts_with_all = ["examples", "schema"],
            help_heading = "Snapshot continuity"
        )]
        snapshot: Option<String>,
        /// Print built-in extract examples (optionally scoped to a topic)
        #[arg(
            long,
            value_name = "TOPIC",
            num_args = 0..=1,
            default_missing_value = "all",
            help_heading = "Built-in help",
            conflicts_with_all = ["spec", "file", "snapshot", "schema"]
        )]
        examples: Option<String>,
        /// Print the canonical extract field and collection schema
        #[arg(
            long,
            help_heading = "Built-in help",
            conflicts_with_all = ["spec", "file", "snapshot", "examples"]
        )]
        schema: bool,
    },

    /// Execute a workflow pipeline over existing canonical commands.
    #[command(long_about = help::PIPE_LONG_ABOUT)]
    Pipe {
        /// JSON pipeline specification (array of {command, args} step objects)
        #[arg(conflicts_with_all = ["file", "workflow", "list_workflows"])]
        spec: Option<String>,
        /// Load the pipeline specification from a JSON file
        #[arg(long, value_name = "PATH", conflicts_with_all = ["spec", "workflow", "list_workflows"])]
        file: Option<String>,
        /// Load a named workflow asset from RUB_HOME/workflows/<name>.json
        #[arg(long, value_name = "NAME", conflicts_with_all = ["spec", "file", "list_workflows"])]
        workflow: Option<String>,
        /// List saved workflow assets under RUB_HOME/workflows
        #[arg(long = "list-workflows", conflicts_with_all = ["spec", "file", "workflow"])]
        list_workflows: bool,
        /// Bind a workflow parameter placeholder like `{{target_url}}` to a concrete value
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Internal: start daemon (not user-facing)
    #[command(hide = true)]
    #[command(name = "__daemon")]
    InternalDaemon,
}

#[cfg(test)]
mod tests;
