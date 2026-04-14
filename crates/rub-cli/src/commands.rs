//! CLI command definitions using clap derive API.

mod args;
mod config;
mod enums;
mod subcommands;

pub use args::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, WaitAfterArgs,
};
pub use config::RequestedLaunchPolicy;
pub use config::{Cli, EffectiveCli};
pub use enums::{DownloadWaitStateArg, InterferenceModeArg, StateFormatArg, StorageAreaArg};
pub use subcommands::{
    BindingCaptureAuthInputArg, BindingSubcommand, Commands, CookiesSubcommand, DialogSubcommand,
    DownloadSubcommand, ExplainSubcommand, GetSubcommand, HandoffSubcommand, InspectSubcommand,
    InterceptSubcommand, InterferenceSubcommand, OrchestrationSubcommand,
    RememberedBindingAliasKindArg, RuntimeSubcommand, SecretSubcommand, StorageSubcommand,
    TakeoverSubcommand, TriggerSubcommand,
};
