use super::*;
use crate::router::request_args::parse_optional_u32_arg;
use rub_core::error::ErrorCode;
use rub_core::observation::{ObservationScope, ObservationSelection};

#[derive(Debug, Clone)]
pub(super) struct ScopedSnapshot {
    pub snapshot: rub_core::model::Snapshot,
    pub scope: ObservationScope,
    pub scope_total_count: u32,
    pub scope_match_count: u32,
}

pub(super) async fn apply_observation_scope(
    router: &DaemonRouter,
    snapshot: rub_core::model::Snapshot,
    scope: &ObservationScope,
) -> Result<ScopedSnapshot, RubError> {
    let (scoped_elements, scope_match_count) = router
        .browser
        .find_snapshot_elements_in_observation_scope(&snapshot, scope)
        .await?;
    let scope_total_count = scoped_elements.len() as u32;

    let mut scoped = snapshot;
    scoped.elements = scoped_elements;
    scoped.total_count = scope_total_count;
    scoped.truncated = false;

    Ok(ScopedSnapshot {
        snapshot: scoped,
        scope: scope.clone(),
        scope_total_count,
        scope_match_count,
    })
}

pub(super) fn parse_observation_scope(
    args: &serde_json::Value,
) -> Result<Option<ObservationScope>, RubError> {
    if let Some(scope) = args.get("scope") {
        if scope.is_null() {
            return Ok(None);
        }
        let scope: ObservationScope = serde_json::from_value(scope.clone()).map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid observation scope: {error}"),
            )
        })?;
        validate_scope(&scope)?;
        return Ok(Some(scope));
    }

    let selector = scope_string_arg(args, &["scope_selector"]);
    let role = scope_string_arg(args, &["scope_role"]);
    let label = scope_string_arg(args, &["scope_label"]);
    let testid = scope_string_arg(args, &["scope_testid"]);
    let selection = parse_scope_selection(args)?;

    let configured = selector.is_some() as u8
        + role.is_some() as u8
        + label.is_some() as u8
        + testid.is_some() as u8;
    if configured == 0 {
        return Ok(None);
    }
    if configured > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Observation scope is ambiguous: provide only one of --scope-selector, --scope-role, --scope-label, or --scope-testid",
        ));
    }

    let scope = if let Some(selector) = selector {
        ObservationScope::Selector {
            css: selector,
            selection,
        }
    } else if let Some(role) = role {
        ObservationScope::Role { role, selection }
    } else if let Some(label) = label {
        ObservationScope::Label { label, selection }
    } else if let Some(testid) = testid {
        ObservationScope::TestId { testid, selection }
    } else {
        return Ok(None);
    };
    validate_scope(&scope)?;
    Ok(Some(scope))
}

pub(super) fn apply_projection_limit(snapshot: &mut rub_core::model::Snapshot, limit: Option<u32>) {
    let Some(limit) = limit else {
        return;
    };
    if limit == 0 {
        return;
    }

    let total_count = snapshot.elements.len() as u32;
    if total_count > limit {
        snapshot.elements.truncate(limit as usize);
        snapshot.total_count = total_count;
        snapshot.truncated = true;
    } else {
        snapshot.total_count = total_count;
        snapshot.truncated = false;
    }
}

pub(super) fn attach_scope_metadata(
    value: &mut serde_json::Value,
    scope: &ObservationScope,
    scope_total_count: u32,
    scope_match_count: u32,
) {
    if let Some(object) = value.as_object_mut() {
        object.insert("scope".to_string(), serde_json::json!(scope));
        object.insert("scope_filtered".to_string(), serde_json::json!(true));
        object.insert(
            "scope_count".to_string(),
            serde_json::json!(scope_total_count),
        );
        object.insert(
            "scope_match_count".to_string(),
            serde_json::json!(scope_match_count),
        );
    }
}

fn scope_string_arg(args: &serde_json::Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        args.get(*name)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn parse_scope_selection(
    args: &serde_json::Value,
) -> Result<Option<ObservationSelection>, RubError> {
    let first = args
        .get("scope_first")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let last = args
        .get("scope_last")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let nth = parse_optional_u32_arg(args, "scope_nth")?;
    let selection_count = first as u8 + last as u8 + nth.is_some() as u8;
    if selection_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Observation scope selection is ambiguous: provide at most one of --scope-first, --scope-last, or --scope-nth",
        ));
    }

    Ok(if first {
        Some(ObservationSelection::First)
    } else if last {
        Some(ObservationSelection::Last)
    } else {
        nth.map(ObservationSelection::Nth)
    })
}

fn validate_scope(scope: &ObservationScope) -> Result<(), RubError> {
    let value = scope.probe_value();
    if value.trim().is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Observation scope {} cannot be empty", scope.kind_name()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{attach_scope_metadata, parse_observation_scope};
    use rub_core::error::ErrorCode;
    use rub_core::observation::{ObservationScope, ObservationSelection};

    #[test]
    fn parse_scope_accepts_role_scope_with_selection() {
        let scope = parse_observation_scope(&serde_json::json!({
            "scope_role": "main",
            "scope_nth": 1,
        }))
        .expect("scope should parse")
        .expect("scope should be present");

        assert_eq!(
            scope,
            ObservationScope::Role {
                role: "main".to_string(),
                selection: Some(ObservationSelection::Nth(1)),
            }
        );
    }

    #[test]
    fn parse_scope_requires_canonical_typed_scope_object() {
        let error = parse_observation_scope(&serde_json::json!({
            "scope": {
                "selector": "#content",
                "selection": { "nth": 1 }
            }
        }))
        .expect_err("shorthand scope object should be rejected");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn attach_scope_metadata_publishes_canonical_scope_fields() {
        let mut value = serde_json::json!({ "snapshot_id": "snap-1" });
        let scope = ObservationScope::Selector {
            css: "#content".to_string(),
            selection: None,
        };
        attach_scope_metadata(&mut value, &scope, 3, 1);
        assert_eq!(value["scope"]["kind"], "selector");
        assert_eq!(value["scope_filtered"], true);
        assert_eq!(value["scope_count"], 3);
        assert_eq!(value["scope_match_count"], 1);
    }
}
