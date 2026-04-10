use rub_core::error::{ErrorCode, RubError};

pub(crate) fn reject_unknown_fields(
    args: &serde_json::Value,
    allowed_fields: &[&str],
    surface: &str,
) -> Result<(), RubError> {
    let Some(object) = args.as_object() else {
        return Ok(());
    };
    let unknown = object
        .keys()
        .filter(|key| !allowed_fields.iter().any(|allowed| key == allowed))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(RubError::domain(
        ErrorCode::InvalidInput,
        format!("Unknown field(s) for {surface}: {}", unknown.join(", ")),
    ))
}

pub(crate) fn parse_json_spec<T>(raw: &str, command: &str) -> Result<T, RubError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(raw).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid JSON spec for '{command}': {error}"),
        )
    })
}

pub(crate) fn parse_json_args<T>(args: &serde_json::Value, command: &str) -> Result<T, RubError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid {command} payload: {error}"),
        )
    })
}

pub(crate) fn required_string_arg(
    args: &serde_json::Value,
    name: &str,
) -> Result<String, RubError> {
    args.get(name)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Missing required argument: '{name}'"),
            )
        })
}

pub(crate) fn subcommand_arg<'a>(args: &'a serde_json::Value, default: &'a str) -> &'a str {
    optional_string_arg(args, "sub").unwrap_or(default)
}

pub(crate) fn optional_string_arg<'a>(args: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(|value| value.as_str())
}

pub(crate) fn parse_optional_u32_arg(
    args: &serde_json::Value,
    name: &str,
) -> Result<Option<u32>, RubError> {
    let Some(value) = args.get(name).and_then(|value| value.as_u64()) else {
        return Ok(None);
    };
    let parsed = u32::try_from(value).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Argument '{name}' is too large; expected a 32-bit unsigned integer"),
        )
    })?;
    Ok(Some(parsed))
}
