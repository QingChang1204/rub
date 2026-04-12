use clap::Subcommand;

use crate::commands::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, StateFormatArg,
    StorageAreaArg,
};

/// Subcommands for `rub get`.
#[derive(Debug, Clone, Subcommand)]
pub enum GetSubcommand {
    /// Get page title
    Title,
    /// Get page HTML (full page or selector)
    Html {
        /// CSS selector (default: full page)
        #[arg(long)]
        selector: Option<String>,
    },
    /// Get element text content
    Text {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
    },
    /// Get element value (for inputs)
    Value {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
    },
    /// Get element attributes
    Attributes {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
    },
    /// Get element bounding box
    Bbox {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
    },
}

/// Subcommands for `rub inspect`.
#[derive(Debug, Clone, Subcommand)]
pub enum InspectSubcommand {
    /// Inspect the page or a scoped content region through the inspection runtime
    Page {
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
        /// Detect JS event listeners on elements
        #[arg(long)]
        listeners: bool,
        #[command(flatten)]
        scope: ObservationScopeArgs,
        #[command(flatten)]
        projection: ObservationProjectionArgs,
    },
    /// Inspect element text through the canonical inspection runtime
    Text {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Return every live DOM match instead of requiring a single selected element
        #[arg(long)]
        many: bool,
    },
    /// Inspect page HTML or locator-matched element HTML
    Html {
        /// Element index from the current/implicit snapshot fence
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Return every live DOM match instead of requiring a single selected element
        #[arg(long)]
        many: bool,
    },
    /// Inspect an input/textarea value through the canonical inspection runtime
    Value {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Return every live DOM match instead of requiring a single selected element
        #[arg(long)]
        many: bool,
    },
    /// Inspect element attributes through the canonical inspection runtime
    Attributes {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Return every live DOM match instead of requiring a single selected element
        #[arg(long)]
        many: bool,
    },
    /// Inspect an element bounding box through the canonical inspection runtime
    Bbox {
        /// Element index
        index: Option<u32>,
        #[command(flatten)]
        target: ElementAddressArgs,
        /// Return every live DOM match instead of requiring a single selected element
        #[arg(long)]
        many: bool,
    },
    /// Inspect a structured list through the extract runtime
    List {
        /// Print built-in help for the `--collection` / `--field` builder surface
        #[arg(
            long,
            help_heading = "Built-in help",
            conflicts_with_all = [
                "spec",
                "file",
                "collection",
                "row_scope",
                "field",
                "snapshot",
                "scan_until",
                "scan_key",
                "max_scrolls",
                "scroll_amount",
                "settle_ms",
                "stall_limit",
                "wait_field",
                "wait_contains",
                "wait_timeout"
            ]
        )]
        builder_help: bool,
        /// Inline JSON inspection/extract specification
        #[arg(conflicts_with_all = ["file", "collection", "builder_help"])]
        spec: Option<String>,
        /// Load the inspection/extract specification from a JSON file
        #[arg(long, value_name = "PATH", conflicts_with_all = ["spec", "collection", "field", "builder_help"])]
        file: Option<String>,
        /// Simple list-builder collection selector for common cases
        #[arg(long, value_name = "SELECTOR", conflicts_with_all = ["spec", "file", "builder_help"])]
        collection: Option<String>,
        /// Optional nearest-ancestor scope selector used as the projection root for child fields
        #[arg(long, value_name = "SELECTOR", conflicts_with_all = ["spec", "file", "builder_help"])]
        row_scope: Option<String>,
        /// Builder field shorthand:
        /// `name` (root text), `name=.selector`, `name=role:heading`,
        /// `name=text:.selector`, `name=text:role:heading`,
        /// `name=text:target_text:Read more`,
        /// `name=html:.selector`, `name=value:.selector`,
        /// `name=attributes:.selector`, `name=bbox:.selector`,
        /// `name=attribute:href:.selector`,
        /// `name=attribute:src:testid:hero-image`
        /// Append `@first`, `@last`, `@many`, or `@nth(0)` to disambiguate repeated matches
        #[arg(long = "field", value_name = "FIELD", conflicts_with_all = ["spec", "file", "builder_help"])]
        field: Vec<String>,
        /// Optional snapshot to reuse as the authoritative inspection fence
        #[arg(long)]
        snapshot: Option<String>,
        /// Keep scrolling/extracting until this many unique rows have been collected
        #[arg(long, value_name = "COUNT")]
        scan_until: Option<u32>,
        /// Dot-path field used as the stable dedupe key while scanning
        #[arg(long, value_name = "FIELD", requires = "scan_until")]
        scan_key: Option<String>,
        /// Maximum number of downward scrolls to attempt during a bounded scan
        #[arg(long, value_name = "COUNT", requires = "scan_until")]
        max_scrolls: Option<u32>,
        /// Scroll amount in pixels for each scan step
        #[arg(long, value_name = "PX", requires = "scan_until")]
        scroll_amount: Option<u32>,
        /// Delay after each scroll before re-running the list extraction
        #[arg(long, value_name = "MS", requires = "scan_until")]
        settle_ms: Option<u64>,
        /// Stop after this many consecutive no-growth passes
        #[arg(long, value_name = "COUNT", requires = "scan_until")]
        stall_limit: Option<u32>,
        /// Wait until a projected row field contains the given substring
        #[arg(
            long,
            value_name = "FIELD",
            requires = "wait_contains",
            conflicts_with = "scan_until"
        )]
        wait_field: Option<String>,
        /// Substring to match inside the projected wait field
        #[arg(
            long,
            value_name = "TEXT",
            requires = "wait_field",
            conflicts_with = "scan_until"
        )]
        wait_contains: Option<String>,
        /// Timeout in milliseconds for `inspect list` wait mode
        #[arg(
            long,
            value_name = "MS",
            requires = "wait_field",
            conflicts_with = "scan_until"
        )]
        wait_timeout: Option<u64>,
    },
    /// Follow a bounded set of list/detail URLs and extract structured fields from each detail page
    Harvest {
        /// Source file containing harvested/list rows (JSON array or plain text URL list)
        #[arg(long, value_name = "PATH")]
        file: String,
        /// Optional dot-path into the JSON source, e.g. `data.fields.items`
        #[arg(long, value_name = "FIELD")]
        input_field: Option<String>,
        /// Row field containing the detail URL (defaults to `url`, then `href`)
        #[arg(long, value_name = "FIELD")]
        url_field: Option<String>,
        /// Optional row field to project as a stable source label
        #[arg(long, value_name = "FIELD")]
        name_field: Option<String>,
        /// Base URL used to resolve relative detail URLs
        #[arg(long, value_name = "URL")]
        base_url: Option<String>,
        /// Inline extract JSON to run on each detail page
        #[arg(long, conflicts_with_all = ["extract_file", "field"])]
        extract: Option<String>,
        /// Extract JSON file to run on each detail page
        #[arg(long, value_name = "PATH", conflicts_with_all = ["extract", "field"])]
        extract_file: Option<String>,
        /// Builder field shorthand for common detail-page extraction cases:
        /// `name` (root text), `name=.selector`, `name=role:heading`,
        /// `name=text:.selector`, `name=text:role:heading`,
        /// `name=text:target_text:Read more`,
        /// `name=html:.selector`, `name=value:.selector`,
        /// `name=attributes:.selector`, `name=bbox:.selector`,
        /// `name=attribute:href:.selector`,
        /// `name=attribute:src:testid:hero-image`
        /// Append `@first`, `@last`, `@many`, or `@nth(0)` to disambiguate repeated matches
        #[arg(long = "field", value_name = "FIELD", conflicts_with_all = ["extract", "extract_file"])]
        field: Vec<String>,
        /// Stop after this many detail pages
        #[arg(long, value_name = "COUNT")]
        limit: Option<u32>,
    },
    /// Inspect current-origin web storage through the unified inspection runtime
    Storage {
        /// Filter to one storage area
        #[arg(long, value_enum)]
        area: Option<StorageAreaArg>,
        /// Return a specific key instead of the whole snapshot/area
        #[arg(long)]
        key: Option<String>,
    },
    /// Inspect recent network requests through the unified inspection runtime
    Network {
        /// Inspect a single recorded request by id
        #[arg(long)]
        id: Option<String>,
        /// Wait for a matching request to reach the requested lifecycle instead of listing immediately
        #[arg(long)]
        wait: bool,
        /// Limit the result to the most recent N matching requests
        #[arg(long)]
        last: Option<u32>,
        /// Match request URL by substring
        #[arg(long = "match")]
        url_match: Option<String>,
        /// Filter requests by HTTP method
        #[arg(long)]
        method: Option<String>,
        /// Filter requests by HTTP response status code
        #[arg(long)]
        status: Option<u16>,
        /// Filter or wait for a request lifecycle: pending, responded, completed, failed, terminal
        #[arg(long)]
        lifecycle: Option<String>,
        /// Timeout in milliseconds when using `--wait`
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Export a recorded request as a reproducible curl command
    Curl {
        /// Recorded request id from `rub inspect network`
        id: String,
    },
}
