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

#[cfg(test)]
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

pub(crate) fn parse_json_spec_value<T>(raw: serde_json::Value, command: &str) -> Result<T, RubError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(raw).map_err(|error| {
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

pub(crate) fn copy_semantic_raw_field(
    args: &serde_json::Value,
    key: &str,
    projected: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(value) = args.get(key) {
        projected.insert(key.to_string(), value.clone());
    }
}

pub(crate) fn lookup_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

pub(crate) fn canonical_json_batch_root(value: &serde_json::Value) -> Option<&serde_json::Value> {
    let items = value.get("items")?;
    array_or_string_json_root(items)
}

pub(crate) fn array_or_string_json_root(value: &serde_json::Value) -> Option<&serde_json::Value> {
    match value {
        serde_json::Value::Array(_) | serde_json::Value::String(_) => Some(value),
        _ => None,
    }
}

pub(crate) fn first_canonical_json_root<'a>(
    root: &'a serde_json::Value,
    candidates: &[&str],
) -> Option<&'a serde_json::Value> {
    candidates.iter().find_map(|candidate| {
        lookup_json_path(root, candidate)
            .and_then(|value| canonical_json_batch_root(value).or(array_or_string_json_root(value)))
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
