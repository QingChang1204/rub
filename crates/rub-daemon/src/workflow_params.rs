use std::collections::{BTreeMap, BTreeSet};

use rub_core::error::{ErrorCode, RubError};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowParameterization {
    pub resolved_spec: String,
    pub parameter_keys: Vec<String>,
}

pub fn resolve_workflow_parameters(
    raw_spec: &str,
    vars: &[String],
) -> Result<WorkflowParameterization, RubError> {
    let parameters = parse_workflow_vars(vars)?;
    resolve_workflow_binding_map(raw_spec, &parameters)
}

pub fn resolve_workflow_json_parameters(
    raw_spec: &str,
    vars: &Map<String, Value>,
) -> Result<WorkflowParameterization, RubError> {
    let parameters = parse_workflow_json_parameter_bindings(vars)?;
    resolve_workflow_binding_map(raw_spec, &parameters)
}

pub(crate) fn resolve_workflow_binding_map(
    raw_spec: &str,
    parameters: &BTreeMap<String, String>,
) -> Result<WorkflowParameterization, RubError> {
    let mut spec = serde_json::from_str::<Value>(raw_spec).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid workflow JSON for parameterization: {error}"),
        )
    })?;
    let mut used = BTreeSet::new();
    let mut missing = BTreeSet::new();
    resolve_placeholders(&mut spec, parameters, &mut used, &mut missing);

    if !missing.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Unresolved workflow parameter(s): {}",
                missing.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            serde_json::json!({
                "missing": missing,
                "provided": parameters.keys().cloned().collect::<Vec<_>>(),
            }),
        ));
    }

    let resolved_spec = serde_json::to_string(&spec).map_err(RubError::from)?;
    Ok(WorkflowParameterization {
        resolved_spec,
        parameter_keys: used.into_iter().collect(),
    })
}

fn parse_workflow_vars(vars: &[String]) -> Result<BTreeMap<String, String>, RubError> {
    let mut parsed = BTreeMap::new();
    for assignment in vars {
        let Some((raw_key, value)) = assignment.split_once('=') else {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid workflow parameter '{assignment}'; expected KEY=VALUE"),
            ));
        };
        let key = raw_key.trim();
        if !is_valid_parameter_name(key) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Invalid workflow parameter name '{key}'; use letters, digits, and underscores"
                ),
            ));
        }
        if parsed.insert(key.to_string(), value.to_string()).is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Duplicate workflow parameter '{key}'"),
            ));
        }
    }
    Ok(parsed)
}

pub(crate) fn parse_workflow_json_parameter_bindings(
    vars: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, RubError> {
    let mut parsed = BTreeMap::new();
    for (key, value) in vars {
        if !is_valid_parameter_name(key) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Invalid workflow parameter name '{key}'; use letters, digits, and underscores"
                ),
            ));
        }
        let string = value.as_str().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Workflow parameter '{key}' must resolve to a string value"),
            )
        })?;
        if parsed.insert(key.to_string(), string.to_string()).is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Duplicate workflow parameter '{key}'"),
            ));
        }
    }
    Ok(parsed)
}

fn is_valid_parameter_name(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn resolve_placeholders(
    value: &mut Value,
    parameters: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
    missing: &mut BTreeSet<String>,
) {
    match value {
        Value::String(text) => {
            let Some(name) = parse_placeholder(text).map(str::to_string) else {
                return;
            };
            match parameters.get(&name) {
                Some(resolved) => {
                    *text = resolved.clone();
                    used.insert(name);
                }
                None => {
                    missing.insert(name);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                resolve_placeholders(value, parameters, used, missing);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                resolve_placeholders(value, parameters, used, missing);
            }
        }
        _ => {}
    }
}

fn parse_placeholder(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("{{")?.strip_suffix("}}")?.trim();
    if is_valid_parameter_name(inner) {
        Some(inner)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_workflow_json_parameters, resolve_workflow_parameters};
    use rub_core::error::ErrorCode;
    use serde_json::json;

    #[test]
    fn resolve_workflow_parameters_replaces_exact_leaf_placeholders() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}","label":"prefix {{target_url}}"}}]}"#;
        let resolved =
            resolve_workflow_parameters(raw, &[String::from("target_url=https://example.com")])
                .expect("parameters should resolve");

        let value: serde_json::Value =
            serde_json::from_str(&resolved.resolved_spec).expect("resolved json");
        assert_eq!(value["steps"][0]["args"]["url"], "https://example.com");
        assert_eq!(value["steps"][0]["args"]["label"], "prefix {{target_url}}");
        assert_eq!(resolved.parameter_keys, vec!["target_url"]);
    }

    #[test]
    fn resolve_workflow_parameters_preserves_secret_references_inside_values() {
        let raw = r#"{"steps":[{"command":"type","args":{"text":"{{password_ref}}"}}]}"#;
        let resolved =
            resolve_workflow_parameters(raw, &[String::from("password_ref=$RUB_PASSWORD")])
                .expect("parameters should resolve");
        let value: serde_json::Value =
            serde_json::from_str(&resolved.resolved_spec).expect("resolved json");
        assert_eq!(value["steps"][0]["args"]["text"], "$RUB_PASSWORD");
    }

    #[test]
    fn resolve_workflow_parameters_reports_missing_placeholders() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#;
        let error = resolve_workflow_parameters(raw, &[]).expect_err("missing vars should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_workflow_parameters_rejects_duplicate_variables() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#;
        let error = resolve_workflow_parameters(
            raw,
            &[
                String::from("target_url=https://a.example"),
                String::from("target_url=https://b.example"),
            ],
        )
        .expect_err("duplicate vars should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_workflow_json_parameters_accepts_string_object_bindings() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#;
        let resolved = resolve_workflow_json_parameters(
            raw,
            json!({"target_url": "https://example.com"})
                .as_object()
                .unwrap(),
        )
        .expect("json vars should resolve");
        let value: serde_json::Value =
            serde_json::from_str(&resolved.resolved_spec).expect("resolved json");
        assert_eq!(value["steps"][0]["args"]["url"], "https://example.com");
        assert_eq!(resolved.parameter_keys, vec!["target_url"]);
    }

    #[test]
    fn resolve_workflow_json_parameters_rejects_non_string_values() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#;
        let error =
            resolve_workflow_json_parameters(raw, json!({"target_url": 42}).as_object().unwrap())
                .expect_err("non-string vars should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }
}
