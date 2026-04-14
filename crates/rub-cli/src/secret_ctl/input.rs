use rub_core::error::{ErrorCode, RubError};
use serde_json::json;
use std::io::Read;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SecretInputMode {
    InlineValue,
    Environment,
    Stdin,
}

pub(super) fn secret_input_mode(
    inline_value: Option<&str>,
    from_env: Option<&str>,
    stdin: bool,
) -> Result<SecretInputMode, RubError> {
    let count =
        usize::from(inline_value.is_some()) + usize::from(from_env.is_some()) + usize::from(stdin);
    if count != 1 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Choose exactly one secret input mode: --value, --from-env, or --stdin".to_string(),
            json!({
                "reason": "secret_input_mode_required",
                "allowed_modes": ["value", "from_env", "stdin"],
            }),
        ));
    }
    if inline_value.is_some() {
        Ok(SecretInputMode::InlineValue)
    } else if from_env.is_some() {
        Ok(SecretInputMode::Environment)
    } else {
        Ok(SecretInputMode::Stdin)
    }
}

pub(super) fn resolve_secret_input_value(
    mode: SecretInputMode,
    inline_value: Option<&str>,
    from_env: Option<&str>,
) -> Result<String, RubError> {
    match mode {
        SecretInputMode::InlineValue => Ok(inline_value.expect("checked above").to_string()),
        SecretInputMode::Environment => {
            let key = from_env.expect("checked above");
            std::env::var(key).map_err(|_| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("Environment variable not found: {key}"),
                    json!({
                        "env_name": key,
                        "reason": "secret_input_env_not_found",
                    }),
                )
            })
        }
        SecretInputMode::Stdin => {
            let mut value = String::new();
            std::io::stdin()
                .read_to_string(&mut value)
                .map_err(|error| {
                    RubError::domain_with_context(
                        ErrorCode::InvalidInput,
                        format!("Failed to read secret value from stdin: {error}"),
                        json!({ "reason": "secret_input_stdin_read_failed" }),
                    )
                })?;
            if let Some(stripped) = value.strip_suffix("\r\n") {
                value = stripped.to_string();
            } else if let Some(stripped) = value.strip_suffix('\n') {
                value = stripped.to_string();
            }
            Ok(value)
        }
    }
}
