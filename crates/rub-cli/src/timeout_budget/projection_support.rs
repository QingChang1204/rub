use crate::commands::Commands;
use rub_core::error::{ErrorCode, RubError};

pub(super) fn local_only_command_projection_error(command: &Commands) -> RubError {
    let surface = command
        .local_projection_surface()
        .unwrap_or_else(|| command.canonical_name());
    RubError::domain(
        ErrorCode::InternalError,
        format!("{surface} must be handled locally before IPC request projection"),
    )
}

pub(super) fn resolve_type_text<'a>(
    text: Option<&'a str>,
    text_flag: Option<&'a str>,
) -> Result<&'a str, RubError> {
    match (text, text_flag) {
        (Some(_), Some(_)) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "type text is ambiguous; provide either positional TEXT or `--text`, not both",
        )),
        (Some(text), None) | (None, Some(text)) => Ok(text),
        (None, None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "type requires text; provide positional TEXT or `--text`",
        )),
    }
}
