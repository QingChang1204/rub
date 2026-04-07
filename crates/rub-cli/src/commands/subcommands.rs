use clap::Subcommand;
use rub_core::DEFAULT_WAIT_TIMEOUT_MS;
use rub_core::command::CommandName;

use super::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, StateFormatArg,
    WaitAfterArgs,
};

mod query;
mod runtime;

pub use query::{GetSubcommand, InspectSubcommand};
pub use runtime::{
    CookiesSubcommand, DialogSubcommand, DownloadSubcommand, HandoffSubcommand,
    InterceptSubcommand, InterferenceSubcommand, OrchestrationSubcommand, RuntimeSubcommand,
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
    ///
    /// Produces an element index map and screenshot in one round trip.
    /// The `element_map` in the result lists all visible interactive elements
    /// with their numeric index — use that index with `click`, `type`, `hover`,
    /// and `fill` to interact with the page.
    ///
    /// After `observe`, interact using the index numbers in element_map:
    ///   rub observe --path /tmp/page.png
    ///   rub click 3                    # click element #3 from element_map
    ///   rub type --index 5 "hello"     # type into element #5
    ///   rub fill '[{"index":3,"value":"hello"},{"index":5,"value":"world"}]'
    ///
    /// Screenshot is base64 in JSON by default; use --path to save to disk.
    /// Use --compact for a token-efficient text summary instead of full a11y tree.
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
        /// Text to type
        text: String,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Wait for a condition (CSS selector or text) to be met
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
        /// Element state to wait for: visible, hidden, attached, detached (default: visible)
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

    /// Fill multiple form fields through the canonical interaction runtime.
    ///
    /// SPEC is a JSON array where each entry targets a field and sets its value.
    /// Fields can be targeted by snapshot index (from `observe`/`state`), by
    /// label, by CSS selector, or by semantic locator.
    ///
    /// Spec formats:
    ///
    ///   By index:    '[{"index":3,"value":"alice@example.com"},{"index":5,"value":"secret"}]'
    ///   By label:    '[{"label":"Email","value":"alice@example.com"},{"label":"Password","value":"secret"}]'
    ///   By selector: '[{"selector":"#email","value":"alice@example.com"}]'
    ///   By text:     '[{"target_text":"Email address","value":"alice@example.com"}]'
    ///
    /// Mixed locators are allowed in the same spec array.
    /// Add --submit-label "Sign in" (or --submit-index N) to click submit after filling.
    ///
    /// Examples:
    ///   rub fill '[{"label":"Email","value":"user@example.com"},{"label":"Password","value":"pass"}]' --submit-label "Log in"
    ///   rub fill --file form.json --submit-label "Submit"
    Fill {
        /// Inline JSON fill specification (array of field descriptors)
        #[arg(conflicts_with = "file")]
        spec: Option<String>,
        /// Load the fill specification from a JSON file
        #[arg(long, value_name = "PATH", conflicts_with = "spec")]
        file: Option<String>,
        /// Optional submit button index
        #[arg(long = "submit-index")]
        submit_index: Option<u32>,
        /// Optional submit selector
        #[arg(long = "submit-selector")]
        submit_selector: Option<String>,
        /// Optional submit target text
        #[arg(long = "submit-target-text")]
        submit_target_text: Option<String>,
        /// Optional submit ref
        #[arg(long = "submit-ref")]
        submit_ref: Option<String>,
        /// Optional submit semantic role
        #[arg(long = "submit-role")]
        submit_role: Option<String>,
        /// Optional submit accessible/visible label
        #[arg(long = "submit-label")]
        submit_label: Option<String>,
        /// Optional submit testing id
        #[arg(long = "submit-testid")]
        submit_testid: Option<String>,
        /// Select the first submit match
        #[arg(long = "submit-first")]
        submit_first: bool,
        /// Select the last submit match
        #[arg(long = "submit-last")]
        submit_last: bool,
        /// Select the nth submit match (0-based)
        #[arg(long = "submit-nth")]
        submit_nth: Option<u32>,
        #[command(flatten)]
        wait_after: WaitAfterArgs,
    },

    /// Extract structured data through the canonical query surface.
    ///
    /// SPEC is a JSON object mapping output field names to extraction descriptors.
    /// Each key becomes a field in the returned JSON.
    ///
    /// Shorthand — field name to CSS selector (defaults to text content):
    ///   rub extract '{"title":"h1","price":".price","desc":".desc"}'
    ///
    /// Explicit kind (text, value, html, bbox, attributes, attribute):
    ///   rub extract '{"title":{"selector":"h1","kind":"text"}}'
    ///
    /// Attribute extraction (use "attr" or "attribute" + "kind":"attribute"):
    ///   rub extract '{"link":{"selector":"a.main","attr":"href"}}'
    ///
    /// Collection extraction (returns an array of row objects):
    ///   rub extract '{"items":{"collection":"li.item","fields":{"name":{"kind":"text"},"price":{"selector":".price"}}}}'
    ///
    /// Output shape: {"result": {"fields": {"title": "...", ...}, "field_count": N}}
    Extract {
        /// Inline JSON extract specification
        #[arg(conflicts_with = "file")]
        spec: Option<String>,
        /// Load the extract specification from a JSON file
        #[arg(long, value_name = "PATH", conflicts_with = "spec")]
        file: Option<String>,
        /// Snapshot ID (strict mode; omitted = use an implicit live snapshot)
        #[arg(long)]
        snapshot: Option<String>,
    },

    /// Execute a workflow pipeline over existing canonical commands.
    ///
    /// SPEC is a JSON array of step objects, each with a `command` key and optional
    /// `args` object. Steps run sequentially; each step result is included in the
    /// final response under `steps[n].result`.
    ///
    /// Minimal example (open and take screenshot):
    ///   rub pipe '[{"command":"open","args":{"url":"https://example.com"}},{"command":"screenshot"}]'
    ///
    /// Form automation:
    ///   rub pipe '[{"command":"open","args":{"url":"https://login.example.com"}},{"command":"fill","args":{"spec":[{"label":"Email","value":"user@example.com"},{"label":"Password","value":"pass"}],"submit_label":"Log in"}}]'
    ///
    /// Named workflow (saved under RUB_HOME/workflows/<name>.json):
    ///   rub pipe --workflow login --var email=user@example.com --var password=secret
    ///
    /// Step result references: Use {{prev.result.PATH}} to inject the previous step's
    /// result, or {{steps[N].result.PATH}} / {{steps[LABEL].result.PATH}} to reference
    /// any completed prior step by index or label:
    ///   rub pipe '[{"command":"extract","args":{"spec":"{\"title\":\"h1\"}"},"label":"get_title"},{"command":"exec","args":{"code":"document.title = \"{{prev.result.fields.title}}\""}}]'
    ///
    /// Allowed commands in pipe: open, state, click, type, exec, scroll, back,
    ///   keys, wait, tabs, switch, close-tab, get, hover, upload, select, fill, extract
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

impl Commands {
    pub(crate) fn canonical_name(&self) -> &'static str {
        match self {
            Self::Open { .. } => CommandName::Open.as_str(),
            Self::State { .. } => CommandName::State.as_str(),
            Self::Observe { .. } => CommandName::Observe.as_str(),
            Self::Find { .. } => CommandName::Find.as_str(),
            Self::Click { .. } => CommandName::Click.as_str(),
            Self::Exec { .. } => CommandName::Exec.as_str(),
            Self::Scroll { .. } => CommandName::Scroll.as_str(),
            Self::Back { .. } => CommandName::Back.as_str(),
            Self::Forward { .. } => CommandName::Forward.as_str(),
            Self::Reload { .. } => CommandName::Reload.as_str(),
            Self::Screenshot { .. } => CommandName::Screenshot.as_str(),
            Self::Close { .. } => CommandName::Close.as_str(),
            Self::Sessions => "sessions",
            Self::Doctor => CommandName::Doctor.as_str(),
            Self::Runtime { .. } => CommandName::Runtime.as_str(),
            Self::Trigger { .. } => CommandName::Trigger.as_str(),
            Self::Orchestration { .. } => CommandName::Orchestration.as_str(),
            Self::Frames => CommandName::Frames.as_str(),
            Self::Frame { .. } => CommandName::Frame.as_str(),
            Self::Cleanup => "cleanup",
            Self::History { .. } => CommandName::History.as_str(),
            Self::Downloads => CommandName::Downloads.as_str(),
            Self::Download { .. } => CommandName::Download.as_str(),
            Self::Storage(_) => CommandName::Storage.as_str(),
            Self::Handoff { .. } => CommandName::Handoff.as_str(),
            Self::Takeover { .. } => CommandName::Takeover.as_str(),
            Self::Dialog { .. } => CommandName::Dialog.as_str(),
            Self::Intercept { .. } => CommandName::Intercept.as_str(),
            Self::Interference { .. } => CommandName::Interference.as_str(),
            Self::Keys { .. } => CommandName::Keys.as_str(),
            Self::Type { .. } => CommandName::Type.as_str(),
            Self::Wait { .. } => CommandName::Wait.as_str(),
            Self::Tabs => CommandName::Tabs.as_str(),
            Self::Switch { .. } => CommandName::Switch.as_str(),
            Self::CloseTab { .. } => CommandName::CloseTab.as_str(),
            Self::Get(_) => CommandName::Get.as_str(),
            Self::Inspect(_) => CommandName::Inspect.as_str(),
            Self::Hover { .. } => CommandName::Hover.as_str(),
            Self::Cookies(_) => CommandName::Cookies.as_str(),
            Self::Upload { .. } => CommandName::Upload.as_str(),
            Self::Select { .. } => CommandName::Select.as_str(),
            Self::Fill { .. } => CommandName::Fill.as_str(),
            Self::Extract { .. } => CommandName::Extract.as_str(),
            Self::Pipe { .. } => CommandName::Pipe.as_str(),
            Self::InternalDaemon => "__daemon",
        }
    }

    pub(crate) fn wait_after_args(&self) -> Option<&WaitAfterArgs> {
        match self {
            Self::Open { wait_after, .. }
            | Self::Back { wait_after }
            | Self::Forward { wait_after }
            | Self::Reload { wait_after, .. }
            | Self::Keys { wait_after, .. }
            | Self::Type { wait_after, .. }
            | Self::Switch { wait_after, .. }
            | Self::Hover { wait_after, .. }
            | Self::Upload { wait_after, .. }
            | Self::Select { wait_after, .. }
            | Self::Fill { wait_after, .. }
            | Self::Pipe { wait_after, .. }
            | Self::Click { wait_after, .. } => Some(wait_after),
            _ => None,
        }
    }

    pub(crate) fn local_projection_surface(&self) -> Option<&'static str> {
        match self {
            Self::Close { all: true } => Some("close --all"),
            Self::Cleanup => Some("cleanup"),
            Self::Sessions => Some("sessions"),
            Self::InternalDaemon => Some("internal daemon"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Commands;
    use crate::commands::Cli;
    use clap::CommandFactory;

    fn render_subcommand_long_help(name: &str) -> String {
        let mut root = Cli::command();
        let mut subcommand = root
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("missing subcommand {name}"))
            .clone();
        let mut buffer = Vec::new();
        subcommand
            .write_long_help(&mut buffer)
            .expect("help should render");
        String::from_utf8(buffer).expect("help should be valid utf-8")
    }

    #[test]
    fn canonical_name_single_sources_public_command_strings() {
        assert_eq!(
            Commands::Downloads.canonical_name(),
            rub_core::command::CommandName::Downloads.as_str()
        );
        assert_eq!(
            Commands::Doctor.canonical_name(),
            rub_core::command::CommandName::Doctor.as_str()
        );
        assert_eq!(Commands::Sessions.canonical_name(), "sessions");
        assert_eq!(Commands::InternalDaemon.canonical_name(), "__daemon");
    }

    #[test]
    fn local_projection_surface_marks_local_only_variants() {
        assert_eq!(
            Commands::Close { all: true }.local_projection_surface(),
            Some("close --all")
        );
        assert_eq!(
            Commands::Cleanup.local_projection_surface(),
            Some("cleanup")
        );
        assert_eq!(
            Commands::InternalDaemon.local_projection_surface(),
            Some("internal daemon")
        );
        assert_eq!(Commands::Downloads.local_projection_surface(), None);
    }

    #[test]
    fn observe_help_uses_index_flag_in_type_example() {
        let help = render_subcommand_long_help("observe");
        assert!(help.contains("rub type --index 5 \"hello\""), "{help}");
        assert!(!help.contains("rub type 5 \"hello\""), "{help}");
    }

    #[test]
    fn pipe_help_examples_use_command_step_key() {
        let help = render_subcommand_long_help("pipe");
        assert!(help.contains("\"command\":\"open\""), "{help}");
        assert!(help.contains("{command, args} step objects"), "{help}");
        assert!(!help.contains("\"cmd\":\"open\""), "{help}");
        assert!(!help.contains("`cmd` key"), "{help}");
    }
}
