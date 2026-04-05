pub mod automation_timeout;
pub mod command;
pub mod error;
pub mod fs;
pub mod locator;
pub mod model;
pub mod observation;
pub mod port;
pub mod process;
pub mod storage;

pub use model::InteractionOutcome;

pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_WAIT_AFTER_TIMEOUT_MS: u64 = 5_000;
