use std::collections::BTreeMap;

use rub_core::error::{ErrorCode, RubError};

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExtractValueType {
    String,
    Number,
    Boolean,
    Array,
    Object,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExtractTransform {
    Trim,
    Lowercase,
    Uppercase,
    ParseInt,
    ParseFloat,
    ParseBool,
}

pub(super) fn resolve_missing_field(
    field_name: &str,
    required: bool,
    default: Option<&serde_json::Value>,
) -> Result<serde_json::Value, RubError> {
    if let Some(default) = default {
        return Ok(default.clone());
    }
    if required {
        return Err(RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            format!("extract field '{field_name}' did not resolve to any element"),
            serde_json::json!({
                "field": field_name,
            }),
        ));
    }
    Ok(serde_json::Value::Null)
}

pub(super) fn apply_postprocess(
    field_name: &str,
    mut value: serde_json::Value,
    value_type: Option<ExtractValueType>,
    default: Option<&serde_json::Value>,
    map: &BTreeMap<String, serde_json::Value>,
    transform: Option<ExtractTransform>,
) -> Result<serde_json::Value, RubError> {
    if value.is_null() {
        if let Some(default) = default {
            value = default.clone();
        } else {
            return Ok(value);
        }
    }

    if !map.is_empty() {
        value = apply_map(value, map);
    }

    if let Some(transform) = transform {
        value = apply_transform(field_name, value, transform)?;
    }

    if let Some(value_type) = value_type {
        validate_type(field_name, &value, value_type)?;
    }

    Ok(value)
}

fn apply_map(
    value: serde_json::Value,
    map: &BTreeMap<String, serde_json::Value>,
) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(|item| apply_map(item, map)).collect())
        }
        serde_json::Value::String(text) => map
            .get(&text)
            .cloned()
            .unwrap_or(serde_json::Value::String(text)),
        other => other,
    }
}

fn apply_transform(
    field_name: &str,
    value: serde_json::Value,
    transform: ExtractTransform,
) -> Result<serde_json::Value, RubError> {
    match value {
        serde_json::Value::Array(items) => Ok(serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| apply_transform(field_name, item, transform))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        serde_json::Value::String(text) => apply_string_transform(field_name, &text, transform),
        serde_json::Value::Bool(flag) if matches!(transform, ExtractTransform::ParseBool) => {
            Ok(serde_json::Value::Bool(flag))
        }
        serde_json::Value::Number(number)
            if matches!(
                transform,
                ExtractTransform::ParseInt | ExtractTransform::ParseFloat
            ) =>
        {
            Ok(serde_json::Value::Number(number))
        }
        other => Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "extract field '{field_name}' transform '{}' requires string-like input",
                transform.as_str()
            ),
            serde_json::json!({
                "field": field_name,
                "transform": transform.as_str(),
                "value": other,
            }),
        )),
    }
}

fn apply_string_transform(
    field_name: &str,
    text: &str,
    transform: ExtractTransform,
) -> Result<serde_json::Value, RubError> {
    match transform {
        ExtractTransform::Trim => Ok(serde_json::Value::String(text.trim().to_string())),
        ExtractTransform::Lowercase => Ok(serde_json::Value::String(text.trim().to_lowercase())),
        ExtractTransform::Uppercase => Ok(serde_json::Value::String(text.trim().to_uppercase())),
        ExtractTransform::ParseInt => text
            .trim()
            .parse::<i64>()
            .map(serde_json::Value::from)
            .map_err(|error| transform_parse_error(field_name, transform, text, error)),
        ExtractTransform::ParseFloat => {
            let parsed = text
                .trim()
                .parse::<f64>()
                .map_err(|error| transform_parse_error(field_name, transform, text, error))?;
            let Some(number) = serde_json::Number::from_f64(parsed) else {
                return Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "extract field '{field_name}' transform '{}' produced a non-finite number",
                        transform.as_str()
                    ),
                    serde_json::json!({
                        "field": field_name,
                        "transform": transform.as_str(),
                        "value": text,
                    }),
                ));
            };
            Ok(serde_json::Value::Number(number))
        }
        ExtractTransform::ParseBool => {
            let normalized = text.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "yes" | "1" => Ok(serde_json::Value::Bool(true)),
                "false" | "no" | "0" => Ok(serde_json::Value::Bool(false)),
                _ => Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "extract field '{field_name}' transform '{}' could not parse '{text}' as boolean",
                        transform.as_str()
                    ),
                    serde_json::json!({
                        "field": field_name,
                        "transform": transform.as_str(),
                        "value": text,
                    }),
                )),
            }
        }
    }
}

fn transform_parse_error(
    field_name: &str,
    transform: ExtractTransform,
    text: &str,
    error: impl std::fmt::Display,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!(
            "extract field '{field_name}' transform '{}' failed for '{text}': {error}",
            transform.as_str()
        ),
        serde_json::json!({
            "field": field_name,
            "transform": transform.as_str(),
            "value": text,
        }),
    )
}

fn validate_type(
    field_name: &str,
    value: &serde_json::Value,
    value_type: ExtractValueType,
) -> Result<(), RubError> {
    let matches = match value_type {
        ExtractValueType::String => value.is_string(),
        ExtractValueType::Number => value.is_number(),
        ExtractValueType::Boolean => value.is_boolean(),
        ExtractValueType::Array => value.is_array(),
        ExtractValueType::Object => value.is_object(),
    };

    if matches {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!(
            "extract field '{field_name}' expected type '{}'",
            value_type.as_str()
        ),
        serde_json::json!({
            "field": field_name,
            "expected_type": value_type.as_str(),
            "actual_value": value,
        }),
    ))
}

impl ExtractValueType {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

impl ExtractTransform {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trim => "trim",
            Self::Lowercase => "lowercase",
            Self::Uppercase => "uppercase",
            Self::ParseInt => "parse_int",
            Self::ParseFloat => "parse_float",
            Self::ParseBool => "parse_bool",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtractTransform, ExtractValueType, apply_postprocess, resolve_missing_field};
    use std::collections::BTreeMap;

    #[test]
    fn postprocess_applies_map_transform_and_type() {
        let mut map = BTreeMap::new();
        map.insert("In Stock".to_string(), serde_json::Value::Bool(true));

        let value = apply_postprocess(
            "stock",
            serde_json::json!("In Stock"),
            Some(ExtractValueType::Boolean),
            None,
            &map,
            None,
        )
        .unwrap();
        assert_eq!(value, serde_json::Value::Bool(true));

        let price = apply_postprocess(
            "price",
            serde_json::json!(" 12.50 "),
            Some(ExtractValueType::Number),
            None,
            &BTreeMap::new(),
            Some(ExtractTransform::ParseFloat),
        )
        .unwrap();
        assert_eq!(price, serde_json::json!(12.5));
    }

    #[test]
    fn missing_field_uses_default_or_null_when_not_required() {
        assert_eq!(
            resolve_missing_field("optional", false, None).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(
            resolve_missing_field("optional", false, Some(&serde_json::json!([]))).unwrap(),
            serde_json::json!([])
        );
    }
}
