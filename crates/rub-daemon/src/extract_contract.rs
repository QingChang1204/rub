use std::path::Path;

use rub_core::error::RubError;

/// Explain how the canonical extract runtime will normalize and interpret a spec.
///
/// This is a pure local helper: it reuses the daemon's extract parser and
/// normalization rules without opening a browser session or requiring a live
/// daemon runtime.
pub fn explain_extract_spec(raw: &str, rub_home: &Path) -> Result<serde_json::Value, RubError> {
    crate::router::explain_extract_spec_contract(raw, rub_home)
}
