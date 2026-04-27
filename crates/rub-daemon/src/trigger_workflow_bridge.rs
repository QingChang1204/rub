use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::port::BrowserPort;
use serde_json::{Map, Value};

use crate::router::request_args::{LocatorParseOptions, parse_canonical_locator_from_value};
use crate::workflow_params::{
    parse_workflow_json_parameter_bindings, resolve_workflow_binding_map,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TriggerWorkflowSourceVarKind {
    Text,
    Html,
    Value,
    Attribute,
}

impl TriggerWorkflowSourceVarKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Html => "html",
            Self::Value => "value",
            Self::Attribute => "attribute",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TriggerWorkflowSourceVarSpec {
    pub(crate) kind: TriggerWorkflowSourceVarKind,
    pub(crate) locator: LiveLocator,
    pub(crate) attribute: Option<String>,
}

pub(crate) fn validate_trigger_workflow_bindings(
    payload: &Map<String, Value>,
) -> Result<(), RubError> {
    let explicit = payload
        .get("vars")
        .and_then(|value| value.as_object())
        .map(parse_workflow_json_parameter_bindings)
        .transpose()?;
    let source = parse_trigger_workflow_source_vars(payload)?;

    let explicit_keys = explicit
        .as_ref()
        .map(|map| map.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let source_keys = source.keys().cloned().collect::<BTreeSet<_>>();
    let duplicates = explicit_keys
        .intersection(&source_keys)
        .cloned()
        .collect::<Vec<_>>();
    if !duplicates.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "trigger workflow payload duplicates variable bindings across payload.vars and payload.source_vars: {}",
                duplicates.join(", ")
            ),
            serde_json::json!({
                "duplicates": duplicates,
            }),
        ));
    }

    Ok(())
}

pub(crate) async fn resolve_trigger_workflow_parameterization(
    browser: &Arc<dyn BrowserPort>,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    payload: &Map<String, Value>,
    raw_spec: &str,
) -> Result<crate::workflow_params::WorkflowParameterization, RubError> {
    let mut bindings = payload
        .get("vars")
        .and_then(|value| value.as_object())
        .map(parse_workflow_json_parameter_bindings)
        .transpose()?
        .unwrap_or_default();

    for (name, value) in
        resolve_source_var_bindings(browser, source_target_id, source_frame_id, payload).await?
    {
        if bindings.insert(name.clone(), value).is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "trigger workflow parameter '{name}' is defined by both payload.vars and payload.source_vars"
                ),
            ));
        }
    }

    resolve_workflow_binding_map(raw_spec, &bindings)
}

pub(crate) fn trigger_workflow_source_var_keys(
    payload: &Map<String, Value>,
) -> Result<Vec<String>, RubError> {
    let mut keys = parse_trigger_workflow_source_vars(payload)?
        .into_keys()
        .collect::<Vec<_>>();
    keys.sort();
    Ok(keys)
}

fn parse_trigger_workflow_source_vars(
    payload: &Map<String, Value>,
) -> Result<BTreeMap<String, TriggerWorkflowSourceVarSpec>, RubError> {
    let Some(source_vars) = payload.get("source_vars") else {
        return Ok(BTreeMap::new());
    };
    let object = source_vars.as_object().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload.source_vars must be a JSON object of source-bound variable specs",
        )
    })?;

    let mut parsed = BTreeMap::new();
    for (name, raw_spec) in object {
        let spec_object = raw_spec.as_object().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("trigger workflow payload.source_vars.{name} must be a JSON object"),
            )
        })?;
        let kind = parse_source_var_kind(name, spec_object)?;
        let locator = parse_canonical_locator_from_value(raw_spec, LocatorParseOptions::LIVE_ONLY)?
            .ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("trigger workflow payload.source_vars.{name} requires a live locator"),
                )
            })?;
        let locator = LiveLocator::try_from(locator).map_err(|invalid| {
            RubError::domain_with_context_and_suggestion(
                ErrorCode::InvalidInput,
                format!("trigger workflow payload.source_vars.{name} requires a live locator"),
                serde_json::json!({
                    "var": name,
                    "locator": invalid,
                }),
                "Use selector, target_text, role, label, or testid addressing for source-bound live reads",
            )
        })?;
        let attribute = spec_object
            .get("attribute")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        if matches!(kind, TriggerWorkflowSourceVarKind::Attribute) && attribute.is_none() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "trigger workflow payload.source_vars.{name} kind='attribute' requires a non-empty attribute field"
                ),
            ));
        }
        if !matches!(kind, TriggerWorkflowSourceVarKind::Attribute) && attribute.is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "trigger workflow payload.source_vars.{name} only supports the attribute field when kind='attribute'"
                ),
            ));
        }
        parsed.insert(
            name.to_string(),
            TriggerWorkflowSourceVarSpec {
                kind,
                locator,
                attribute,
            },
        );
    }

    Ok(parsed)
}

async fn resolve_source_var_bindings(
    browser: &Arc<dyn BrowserPort>,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    payload: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, RubError> {
    let mut resolved = BTreeMap::new();
    for (name, spec) in parse_trigger_workflow_source_vars(payload)? {
        let value = match spec.kind {
            TriggerWorkflowSourceVarKind::Text => {
                browser
                    .query_text_in_tab(source_target_id, source_frame_id, &spec.locator)
                    .await
            }
            TriggerWorkflowSourceVarKind::Html => {
                browser
                    .query_html_in_tab(source_target_id, source_frame_id, &spec.locator)
                    .await
            }
            TriggerWorkflowSourceVarKind::Value => {
                browser
                    .query_value_in_tab(source_target_id, source_frame_id, &spec.locator)
                    .await
            }
            TriggerWorkflowSourceVarKind::Attribute => {
                let attribute_name = spec.attribute.as_deref().ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::InvalidInput,
                        format!(
                            "trigger workflow payload.source_vars.{name} kind='attribute' requires a non-empty attribute field"
                        ),
                    )
                })?;
                let attributes = browser
                    .query_attributes_in_tab(source_target_id, source_frame_id, &spec.locator)
                    .await
                    .map_err(|error| {
                        normalize_source_var_frame_continuity_error(
                            error,
                            source_target_id,
                            source_frame_id,
                            &name,
                            spec.kind.as_str(),
                        )
                    })?;
                Ok(attributes.get(attribute_name).cloned().ok_or_else(|| {
                    RubError::domain_with_context_and_suggestion(
                        ErrorCode::ElementNotFound,
                        format!(
                            "trigger workflow source_vars.{name} resolved an element without attribute '{attribute_name}'"
                        ),
                        serde_json::json!({
                            "var": name,
                            "attribute": attribute_name,
                            "locator": serde_json::json!({
                                "kind": spec.locator.kind_name(),
                                "probe": spec.locator.probe_value(),
                            }),
                        }),
                        "Use a locator that resolves to an element carrying the requested attribute, or change payload.source_vars to read text, html, or value instead",
                    )
                })?)
            }
        }
        .map_err(|error| {
            normalize_source_var_frame_continuity_error(
                error,
                source_target_id,
                source_frame_id,
                &name,
                spec.kind.as_str(),
            )
        })?;
        resolved.insert(name, value);
    }
    Ok(resolved)
}

fn normalize_source_var_frame_continuity_error(
    error: RubError,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    var_name: &str,
    source_var_kind: &str,
) -> RubError {
    let RubError::Domain(envelope) = error else {
        return error;
    };
    let authority_reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str());
    let frame_continuity_failed = source_frame_id.is_some()
        && envelope.code == ErrorCode::InvalidInput
        && matches!(
            authority_reason,
            Some(
                "frame_inventory_missing"
                    | "frame_not_same_origin_accessible"
                    | "frame_execution_context_missing"
            )
        );
    let document_continuity_failed = envelope.code == ErrorCode::StaleSnapshot
        && matches!(
            authority_reason,
            Some("document_changed_during_live_read" | "document_fence_unavailable")
        );
    if !frame_continuity_failed && !document_continuity_failed {
        return RubError::Domain(envelope);
    }
    let continuity_reason = if document_continuity_failed {
        "continuity_document_unavailable"
    } else {
        "continuity_frame_unavailable"
    };

    RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "trigger workflow source_vars continuity fence failed before authoritative source materialization could complete",
        serde_json::json!({
            "reason": continuity_reason,
            "source_tab_target_id": source_target_id,
            "source_frame_id": source_frame_id,
            "source_var": var_name,
            "source_var_kind": source_var_kind,
            "source_authority_reason": authority_reason,
            "upstream_error_code": envelope.code,
            "upstream_error_message": envelope.message,
            "upstream_error_context": envelope.context,
        }),
    )
}

pub(crate) async fn resolve_trigger_workflow_source_bindings(
    browser: &Arc<dyn BrowserPort>,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    payload: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, RubError> {
    resolve_source_var_bindings(browser, source_target_id, source_frame_id, payload).await
}

fn parse_source_var_kind(
    name: &str,
    spec: &Map<String, Value>,
) -> Result<TriggerWorkflowSourceVarKind, RubError> {
    match spec
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("text") => Ok(TriggerWorkflowSourceVarKind::Text),
        Some("html") => Ok(TriggerWorkflowSourceVarKind::Html),
        Some("value") => Ok(TriggerWorkflowSourceVarKind::Value),
        Some("attribute") => Ok(TriggerWorkflowSourceVarKind::Attribute),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Unsupported trigger workflow payload.source_vars.{name}.kind '{other}'; use 'text', 'html', 'value', or 'attribute'"
            ),
        )),
        None => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("trigger workflow payload.source_vars.{name} requires a non-empty kind field"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TriggerWorkflowSourceVarKind, normalize_source_var_frame_continuity_error,
        parse_trigger_workflow_source_vars, trigger_workflow_source_var_keys,
        validate_trigger_workflow_bindings,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::locator::{CanonicalLocator, LiveLocator, LocatorSelection};
    use serde_json::json;

    #[test]
    fn parse_trigger_workflow_source_vars_accepts_text_locator_specs() {
        let payload = json!({
            "source_vars": {
                "reply_name": {
                    "kind": "text",
                    "selector": "#question",
                    "first": true
                }
            }
        });
        let parsed = parse_trigger_workflow_source_vars(payload.as_object().unwrap())
            .expect("source vars should parse");
        let spec = parsed.get("reply_name").expect("var should exist");
        assert_eq!(spec.kind, TriggerWorkflowSourceVarKind::Text);
        assert_eq!(
            spec.locator,
            LiveLocator::try_from(CanonicalLocator::Selector {
                css: "#question".to_string(),
                selection: Some(LocatorSelection::First),
            })
            .expect("selector should be a valid live locator")
        );
    }

    #[test]
    fn validate_trigger_workflow_bindings_rejects_duplicate_explicit_and_source_vars() {
        let payload = json!({
            "vars": {
                "reply_name": "Grace"
            },
            "source_vars": {
                "reply_name": {
                    "kind": "text",
                    "selector": "#question"
                }
            }
        });
        let error = validate_trigger_workflow_bindings(payload.as_object().unwrap())
            .expect_err("duplicate bindings should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn parse_trigger_workflow_source_vars_rejects_attribute_without_name() {
        let payload = json!({
            "source_vars": {
                "reply_href": {
                    "kind": "attribute",
                    "selector": "a.reply"
                }
            }
        });
        let error = parse_trigger_workflow_source_vars(payload.as_object().unwrap())
            .expect_err("missing attribute should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn source_var_frame_continuity_error_is_retryable_degraded_authority() {
        let error = normalize_source_var_frame_continuity_error(
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "Frame is unavailable",
                json!({
                    "reason": "frame_execution_context_missing",
                }),
            ),
            "tab-source",
            Some("frame-child"),
            "prompt_text",
            "text",
        )
        .into_envelope();

        assert_eq!(error.code, ErrorCode::SessionBusy);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_unavailable")
        );
    }

    #[test]
    fn trigger_workflow_source_var_keys_sort_keys() {
        let payload = json!({
            "source_vars": {
                "zeta": { "kind": "text", "selector": "#z" },
                "alpha": { "kind": "value", "selector": "#a" }
            }
        });
        let keys = trigger_workflow_source_var_keys(payload.as_object().unwrap())
            .expect("keys should parse");
        assert_eq!(keys, vec!["alpha".to_string(), "zeta".to_string()]);
    }
}
