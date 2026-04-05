use clap::Args;

#[derive(Debug, Clone, Default, Args)]
pub struct WaitAfterArgs {
    /// Wait for this selector after the action completes
    #[arg(long = "wait-after-selector", id = "wait_after_selector")]
    pub selector: Option<String>,
    /// Wait for an element with matching visible/accessibility text after the action completes
    #[arg(long = "wait-after-target-text", id = "wait_after_target_text")]
    pub target_text: Option<String>,
    /// Wait for an element with this semantic role after the action completes
    #[arg(long = "wait-after-role", id = "wait_after_role")]
    pub role: Option<String>,
    /// Wait for an element with this accessible/visible label after the action completes
    #[arg(long = "wait-after-label", id = "wait_after_label")]
    pub label: Option<String>,
    /// Wait for an element with this testing id after the action completes
    #[arg(long = "wait-after-testid", id = "wait_after_testid")]
    pub testid: Option<String>,
    /// Wait for this text after the action completes
    #[arg(long = "wait-after-text", id = "wait_after_text")]
    pub text: Option<String>,
    /// Select the first match from a semantic wait locator
    #[arg(long = "wait-after-first", id = "wait_after_first")]
    pub first: bool,
    /// Select the last match from a semantic wait locator
    #[arg(long = "wait-after-last", id = "wait_after_last")]
    pub last: bool,
    /// Select the nth match from a semantic wait locator (0-based)
    #[arg(long = "wait-after-nth", id = "wait_after_nth")]
    pub nth: Option<u32>,
    /// Timeout in milliseconds for the post-action wait
    #[arg(long = "wait-after-timeout", id = "wait_after_timeout_ms")]
    pub timeout_ms: Option<u64>,
    /// Selector wait state: visible, hidden, attached, detached
    #[arg(long = "wait-after-state", id = "wait_after_state")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ElementAddressArgs {
    /// Snapshot ID (strict mode; omitted = use an implicit live snapshot)
    #[arg(long)]
    pub snapshot: Option<String>,
    /// Resolve the target through a stable frame-bound element ref from `state`/`observe`
    #[arg(long = "ref")]
    pub element_ref: Option<String>,
    /// Resolve the target through a CSS selector instead of an index
    #[arg(long)]
    pub selector: Option<String>,
    /// Resolve the target through visible/accessibility text instead of an index
    #[arg(long = "target-text")]
    pub target_text: Option<String>,
    /// Resolve the target through semantic role instead of an index
    #[arg(long)]
    pub role: Option<String>,
    /// Resolve the target through accessible/visible label instead of an index
    #[arg(long)]
    pub label: Option<String>,
    /// Resolve the target through a testing id instead of an index
    #[arg(long)]
    pub testid: Option<String>,
    /// Select the first result from a multi-match locator
    #[arg(long)]
    pub first: bool,
    /// Select the last result from a multi-match locator
    #[arg(long)]
    pub last: bool,
    /// Select the nth result from a multi-match locator (0-based)
    #[arg(long)]
    pub nth: Option<u32>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ObservationScopeArgs {
    /// Scope the projection to a CSS content root (preferred long form: --scope-selector)
    #[arg(long = "scope-selector")]
    pub selector: Option<String>,
    /// Scope the projection to content roots matched by semantic role
    #[arg(long = "scope-role")]
    pub role: Option<String>,
    /// Scope the projection to content roots matched by accessible label
    #[arg(long = "scope-label")]
    pub label: Option<String>,
    /// Scope the projection to content roots matched by testing id
    #[arg(long = "scope-testid")]
    pub testid: Option<String>,
    /// Select the first matching content root
    #[arg(long = "scope-first")]
    pub first: bool,
    /// Select the last matching content root
    #[arg(long = "scope-last")]
    pub last: bool,
    /// Select the nth matching content root (0-based)
    #[arg(long = "scope-nth")]
    pub nth: Option<u32>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ObservationProjectionArgs {
    /// Publish a token-friendlier compact observation projection
    #[arg(long)]
    pub compact: bool,
    /// Limit observation output to interactive descendants at or above this relative DOM depth
    #[arg(long)]
    pub depth: Option<u32>,
}
