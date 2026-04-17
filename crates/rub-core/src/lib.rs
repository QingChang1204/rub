pub mod automation_timeout;
pub mod command;
pub mod error;
pub mod fs;
pub mod json_spec;
pub mod locator;
pub mod managed_profile;
pub mod model;
pub mod observation;
pub mod port;
pub mod process;
pub mod secrets_env;
pub mod storage;

pub use model::InteractionOutcome;

pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_WAIT_AFTER_TIMEOUT_MS: u64 = 5_000;
