use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::commands::{ElementAddressArgs, ExplainSubcommand};
use rub_core::error::{ErrorCode, RubError};
use serde_json::{Map, Value, json};

pub(crate) fn project_explain(
    subcommand: &ExplainSubcommand,
    rub_home: &std::path::Path,
) -> Result<serde_json::Value, RubError> {
    match subcommand {
        ExplainSubcommand::Extract { spec, file } => {
            let raw = resolve_extract_source(spec.as_deref(), file.as_deref())?;
            rub_daemon::extract_contract::explain_extract_spec(&raw, rub_home)
        }
        ExplainSubcommand::Locator { .. }
        | ExplainSubcommand::Interactability { .. }
        | ExplainSubcommand::Blockers => Err(RubError::domain(
            ErrorCode::InternalError,
            "this explain surface is not a local-only explain surface",
        )),
    }
}

pub(crate) fn project_locator_explain_response(
    target: &ElementAddressArgs,
    data: Option<Value>,
) -> Result<Value, RubError> {
    let Some(data) = data else {
        return Err(RubError::domain(
            ErrorCode::InternalError,
            "locator explain expected a successful find payload",
        ));
    };
    let Some(subject) = data.get("subject").cloned() else {
        return Err(RubError::domain(
            ErrorCode::InternalError,
            "locator explain payload is missing subject",
        ));
    };
    let result = data
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InternalError,
                "locator explain payload is missing result object",
            )
        })?;
    let matches = result
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InternalError,
                "locator explain payload is missing candidate matches",
            )
        })?;

    let mut candidates = matches.clone();
    candidates.sort_by_key(|candidate| {
        candidate
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    });

    let selection = describe_locator_selection(target);
    let selected_rank = selection
        .as_ref()
        .and_then(|selection| select_candidate(&candidates, selection));
    let selection_outcome =
        locator_selection_outcome(candidates.len(), selection.as_ref(), selected_rank);

    let ordering_policy = ordering_policy_from_target(target);
    let disambiguation_hints = locator_disambiguation_hints(&candidates);
    let frequencies = locator_candidate_frequencies(&candidates);
    let projected_candidates = candidates
        .iter()
        .enumerate()
        .map(|(rank, candidate)| {
            let mut object = candidate.as_object().cloned().unwrap_or_else(Map::new);
            object.insert("rank".to_string(), json!(rank));
            object.insert(
                "selected".to_string(),
                json!(selected_rank.is_some_and(|selected| selected == rank)),
            );
            object.insert(
                "ranking_hints".to_string(),
                locator_candidate_ranking_hints(candidate, &frequencies, ordering_policy),
            );
            Value::Object(object)
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "subject": {
            "kind": "locator_explain",
            "surface": subject.get("surface").cloned().unwrap_or(Value::Null),
            "find_subject": subject,
        },
        "result": {
            "candidate_count": result.get("match_count").cloned().unwrap_or(json!(projected_candidates.len())),
            "returned_count": result.get("returned_count").cloned().unwrap_or(json!(projected_candidates.len())),
            "snapshot_id": result.get("snapshot_id").cloned().unwrap_or(Value::Null),
            "ordering_policy": ordering_policy,
            "ranking_policy": {
                "visible": target.visible,
                "prefer_enabled": target.prefer_enabled,
                "topmost": target.topmost,
            },
            "selection": selection.as_ref().map(render_selection).unwrap_or(Value::Null),
            "selection_outcome": render_selection_outcome(selection_outcome, selection.as_ref(), candidates.len()),
            "selected_candidate": selected_rank.map(|rank| projected_candidates[rank].clone()).unwrap_or(Value::Null),
            "candidates": projected_candidates,
            "disambiguation_hints": disambiguation_hints,
            "guidance": locator_guidance(target, &candidates, selection.as_ref(), selection_outcome),
        }
    }))
}

fn resolve_extract_source(spec: Option<&str>, file: Option<&str>) -> Result<String, RubError> {
    match (spec, file) {
        (Some(raw), None) => Ok(raw.to_string()),
        (None, Some(path)) => {
            let resolved = resolve_cli_path(path);
            fs::read_to_string(&resolved).map_err(|error| {
                let code = if error.kind() == std::io::ErrorKind::NotFound {
                    ErrorCode::FileNotFound
                } else {
                    ErrorCode::InvalidInput
                };
                RubError::domain_with_context(
                    code,
                    format!(
                        "Failed to read explain extract spec file {}: {error}",
                        resolved.display()
                    ),
                    serde_json::json!({
                        "path": resolved.display().to_string(),
                        "surface": "explain.extract.file",
                    }),
                )
            })
        }
        (Some(_), Some(_)) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "explain extract accepts either inline spec or --file, not both",
        )),
        (None, None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "explain extract requires an inline spec or --file",
        )),
    }
}

fn resolve_cli_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(candidate)
    }
}

#[derive(Debug, Clone, Copy)]
enum LocatorSelectionExplain {
    First,
    Last,
    Nth(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocatorSelectionOutcome {
    NoCandidates,
    ImplicitSingleCandidate,
    AmbiguousWithoutSelection,
    ExplicitSelectionResolved,
    ExplicitSelectionOutOfRange,
}

fn describe_locator_selection(target: &ElementAddressArgs) -> Option<LocatorSelectionExplain> {
    if target.first {
        Some(LocatorSelectionExplain::First)
    } else if target.last {
        Some(LocatorSelectionExplain::Last)
    } else {
        target.nth.map(LocatorSelectionExplain::Nth)
    }
}

fn select_candidate(candidates: &[Value], selection: &LocatorSelectionExplain) -> Option<usize> {
    match selection {
        LocatorSelectionExplain::First => (!candidates.is_empty()).then_some(0),
        LocatorSelectionExplain::Last => candidates.len().checked_sub(1),
        LocatorSelectionExplain::Nth(nth) => {
            let nth = *nth as usize;
            (nth < candidates.len()).then_some(nth)
        }
    }
}

fn render_selection(selection: &LocatorSelectionExplain) -> Value {
    match selection {
        LocatorSelectionExplain::First => json!({ "kind": "first" }),
        LocatorSelectionExplain::Last => json!({ "kind": "last" }),
        LocatorSelectionExplain::Nth(nth) => json!({ "kind": "nth", "nth": nth }),
    }
}

fn locator_selection_outcome(
    candidate_count: usize,
    selection: Option<&LocatorSelectionExplain>,
    selected_rank: Option<usize>,
) -> LocatorSelectionOutcome {
    if candidate_count == 0 {
        return LocatorSelectionOutcome::NoCandidates;
    }
    match selection {
        Some(_) if selected_rank.is_some() => LocatorSelectionOutcome::ExplicitSelectionResolved,
        Some(_) => LocatorSelectionOutcome::ExplicitSelectionOutOfRange,
        None if candidate_count == 1 => LocatorSelectionOutcome::ImplicitSingleCandidate,
        None => LocatorSelectionOutcome::AmbiguousWithoutSelection,
    }
}

fn render_selection_outcome(
    outcome: LocatorSelectionOutcome,
    selection: Option<&LocatorSelectionExplain>,
    candidate_count: usize,
) -> Value {
    match outcome {
        LocatorSelectionOutcome::NoCandidates => json!({
            "kind": "no_candidates",
        }),
        LocatorSelectionOutcome::ImplicitSingleCandidate => json!({
            "kind": "implicit_single_candidate",
        }),
        LocatorSelectionOutcome::AmbiguousWithoutSelection => json!({
            "kind": "ambiguous_without_selection",
            "candidate_count": candidate_count,
        }),
        LocatorSelectionOutcome::ExplicitSelectionResolved => json!({
            "kind": "explicit_selection_resolved",
            "requested_selection": selection.map(render_selection).unwrap_or(Value::Null),
        }),
        LocatorSelectionOutcome::ExplicitSelectionOutOfRange => json!({
            "kind": "explicit_selection_out_of_range",
            "requested_selection": selection.map(render_selection).unwrap_or(Value::Null),
            "candidate_count": candidate_count,
        }),
    }
}

fn locator_disambiguation_hints(candidates: &[Value]) -> Value {
    json!({
        "labels": unique_candidate_field_values(candidates, "label"),
        "roles": unique_candidate_field_values(candidates, "role"),
        "testids": unique_candidate_field_values(candidates, "testid"),
        "texts": unique_candidate_field_values(candidates, "text"),
    })
}

fn ordering_policy_from_target(target: &ElementAddressArgs) -> &'static str {
    match (target.visible, target.prefer_enabled, target.topmost) {
        (true, true, true) => {
            "visible_filter_then_topmost_hit_test_then_prefer_enabled_then_snapshot_index"
        }
        (true, true, false) => "visible_filter_then_prefer_enabled_then_snapshot_index",
        (true, false, true) => "visible_filter_then_topmost_hit_test_then_snapshot_index",
        (true, false, false) => "visible_filter_then_snapshot_index",
        (false, true, true) => "topmost_hit_test_then_prefer_enabled_then_snapshot_index",
        (false, true, false) => "prefer_enabled_then_snapshot_index",
        (false, false, true) => "topmost_hit_test_then_snapshot_index",
        (false, false, false) => "snapshot_index",
    }
}

struct LocatorCandidateFrequencies {
    labels: HashMap<String, usize>,
    roles: HashMap<String, usize>,
    testids: HashMap<String, usize>,
    texts: HashMap<String, usize>,
}

fn locator_candidate_frequencies(candidates: &[Value]) -> LocatorCandidateFrequencies {
    let mut frequencies = LocatorCandidateFrequencies {
        labels: HashMap::new(),
        roles: HashMap::new(),
        testids: HashMap::new(),
        texts: HashMap::new(),
    };
    for candidate in candidates {
        if let Some(label) = candidate_field(candidate, "label") {
            *frequencies.labels.entry(label.to_string()).or_insert(0) += 1;
        }
        if let Some(role) = candidate_field(candidate, "role") {
            *frequencies.roles.entry(role.to_string()).or_insert(0) += 1;
        }
        if let Some(testid) = candidate_field(candidate, "testid") {
            *frequencies.testids.entry(testid.to_string()).or_insert(0) += 1;
        }
        if let Some(text) = candidate_field(candidate, "text") {
            *frequencies.texts.entry(text.to_string()).or_insert(0) += 1;
        }
    }
    frequencies
}

fn locator_candidate_ranking_hints(
    candidate: &Value,
    frequencies: &LocatorCandidateFrequencies,
    ordering_policy: &str,
) -> Value {
    let label = candidate_field(candidate, "label");
    let role = candidate_field(candidate, "role");
    let testid = candidate_field(candidate, "testid");
    let text = candidate_field(candidate, "text");

    json!({
        "ordered_by": ordering_policy,
        "unique_anchors": {
            "label": label.filter(|value| frequencies.labels.get(*value) == Some(&1)),
            "role": role.filter(|value| frequencies.roles.get(*value) == Some(&1)),
            "testid": testid.filter(|value| frequencies.testids.get(*value) == Some(&1)),
            "text": text.filter(|value| frequencies.texts.get(*value) == Some(&1)),
        }
    })
}

fn unique_candidate_field_values(candidates: &[Value], key: &str) -> Vec<String> {
    let mut values = candidates
        .iter()
        .filter_map(|candidate| candidate_field(candidate, key).map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    if values.len() > 5 {
        values.truncate(5);
    }
    values
}

fn candidate_field<'a>(candidate: &'a Value, key: &str) -> Option<&'a str> {
    candidate
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn locator_primary_kind(target: &ElementAddressArgs) -> &'static str {
    if target.element_ref.is_some() {
        "ref"
    } else if target.selector.is_some() {
        "selector"
    } else if target.target_text.is_some() {
        "target_text"
    } else if target.role.is_some() {
        "role"
    } else if target.label.is_some() {
        "label"
    } else if target.testid.is_some() {
        "testid"
    } else {
        "unknown"
    }
}

fn locator_guidance(
    target: &ElementAddressArgs,
    candidates: &[Value],
    selection: Option<&LocatorSelectionExplain>,
    outcome: LocatorSelectionOutcome,
) -> Value {
    let primary_kind = locator_primary_kind(target);
    let mut next_safe_actions = Vec::new();
    let summary = match outcome {
        LocatorSelectionOutcome::NoCandidates => {
            next_safe_actions.push(
                "Run `rub state --format compact` or `rub observe` to inspect the current page baseline."
                    .to_string(),
            );
            next_safe_actions.push(match primary_kind {
                "label" => {
                    "Try a stricter label, or switch to `--role` / `--selector` if the control is not labeled."
                        .to_string()
                }
                "target_text" => {
                    "Try a tighter text fragment, or switch to `--label` / `--selector` if the target is not text-anchored."
                        .to_string()
                }
                "selector" => {
                    "Verify the selector against the current frame, or fall back to `--label` / `--role` for a more stable surface."
                        .to_string()
                }
                _ => "Adjust the locator or switch to a different canonical locator surface.".to_string(),
            });
            "No candidates were returned from the authoritative find surface."
        }
        LocatorSelectionOutcome::ImplicitSingleCandidate => {
            next_safe_actions.push(
                "Use this locator directly, or pair it with `rub explain interactability ...` if actuation is still failing."
                    .to_string(),
            );
            "This locator already resolves to one candidate."
        }
        LocatorSelectionOutcome::AmbiguousWithoutSelection => {
            next_safe_actions.push(
                "Add `--first`, `--last`, or `--nth` to make the winning candidate explicit."
                    .to_string(),
            );
            next_safe_actions.push(
                "Tighten the locator with a more specific label, role, test id, or selector."
                    .to_string(),
            );
            if !locator_disambiguation_hints(candidates)
                .as_object()
                .is_some_and(|hints| {
                    hints
                        .values()
                        .any(|value| value.as_array().is_some_and(|items| !items.is_empty()))
                })
            {
                next_safe_actions.push(
                    "Run `rub state --format compact` or `rub observe` to look for a stronger stable anchor."
                        .to_string(),
                );
            } else {
                next_safe_actions.push(
                    "Review `disambiguation_hints` for labels, roles, test ids, or texts that can narrow the locator."
                        .to_string(),
                );
            }
            "Multiple candidates resolved; this locator still needs an explicit winner or a tighter anchor."
        }
        LocatorSelectionOutcome::ExplicitSelectionResolved => {
            next_safe_actions.push(
                "Selection flags are applied after ordering candidates by snapshot index."
                    .to_string(),
            );
            next_safe_actions.push(
                "Use `rub explain interactability ...` next if you need to understand why the selected target still cannot be actuated."
                    .to_string(),
            );
            "Selection flags resolved one winning candidate from the authoritative candidate list."
        }
        LocatorSelectionOutcome::ExplicitSelectionOutOfRange => {
            next_safe_actions.push(
                "Lower `--nth` or remove the selection flag, then rerun the explain surface."
                    .to_string(),
            );
            next_safe_actions.push(
                "Inspect the returned candidates to pick a valid rank before retrying the real command."
                    .to_string(),
            );
            "The requested selection does not map to any candidate in the current ordered result set."
        }
    };

    if !matches!(outcome, LocatorSelectionOutcome::NoCandidates) {
        next_safe_actions.push(
            "Run `rub state --format compact` or `rub observe` if you still need more page context."
                .to_string(),
        );
    }
    if selection.is_none()
        && matches!(
            outcome,
            LocatorSelectionOutcome::ImplicitSingleCandidate
                | LocatorSelectionOutcome::AmbiguousWithoutSelection
        )
    {
        next_safe_actions.push(
            "Add `--first`, `--last`, or `--nth` anyway if you want the selection contract to stay explicit across page drift."
                .to_string(),
        );
    }

    json!({
        "summary": summary,
        "next_safe_actions": next_safe_actions,
    })
}

#[cfg(test)]
mod tests {
    use super::{project_explain, project_locator_explain_response};
    use crate::commands::ExplainSubcommand;
    use rub_core::error::ErrorCode;
    use serde_json::{Value, json};

    #[test]
    fn explain_extract_projects_normalized_spec() {
        let result = project_explain(
            &ExplainSubcommand::Extract {
                spec: Some(r#"{"title":"h1","items":{"collection":"li.item","fields":{"name":{"kind":"text"}}}}"#.to_string()),
                file: None,
            },
            std::path::Path::new("/tmp/nonexistent-rub-home-for-explain"),
        )
        .expect("explain extract should project");

        assert_eq!(result["subject"]["kind"], "extract_explain");
        assert_eq!(result["result"]["normalized_spec"]["title"]["kind"], "text");
        assert_eq!(
            result["result"]["normalized_spec"]["items"]["fields"]["name"]["kind"],
            "text"
        );
    }

    #[test]
    fn explain_extract_errors_include_schema_guidance() {
        let error = project_explain(
            &ExplainSubcommand::Extract {
                spec: Some(r#"{"title":{"unknown_key":true}}"#.to_string()),
                file: None,
            },
            std::path::Path::new("/tmp/nonexistent-rub-home-for-explain"),
        )
        .expect_err("invalid explain extract should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("explain error context");
        assert_eq!(context["schema_command"], json!("rub extract --schema"));
        assert_eq!(context["examples_command"], json!("rub extract --examples"));
    }

    #[test]
    fn locator_explain_projects_ordered_candidates_and_selected_rank() {
        let result = project_locator_explain_response(
            &crate::commands::ElementAddressArgs {
                snapshot: None,
                element_ref: None,
                selector: None,
                target_text: Some("New Topic".to_string()),
                role: None,
                label: None,
                testid: None,
                visible: true,
                prefer_enabled: true,
                topmost: false,
                first: false,
                last: false,
                nth: Some(1),
            },
            Some(json!({
                "subject": {
                    "surface": "interactive_snapshot",
                    "locator": { "target_text": "New Topic" }
                },
                "result": {
                    "match_count": 2,
                    "returned_count": 2,
                    "snapshot_id": "snap-1",
                    "matches": [
                        { "index": 9, "text": "Second", "role": "button", "label": "Second" },
                        { "index": 4, "text": "First", "role": "button", "label": "First" }
                    ]
                }
            })),
        )
        .expect("locator explain should project");

        assert_eq!(result["subject"]["kind"], "locator_explain");
        assert_eq!(result["result"]["selection"]["kind"], "nth");
        assert_eq!(
            result["result"]["selection_outcome"]["kind"],
            "explicit_selection_resolved"
        );
        assert_eq!(result["result"]["selected_candidate"]["index"], 9);
        assert_eq!(result["result"]["candidates"][0]["index"], 4);
        assert_eq!(result["result"]["candidates"][0]["selected"], false);
        assert_eq!(result["result"]["candidates"][1]["selected"], true);
        assert_eq!(
            result["result"]["candidates"][0]["ranking_hints"]["unique_anchors"]["label"],
            "First"
        );
        assert_eq!(
            result["result"]["ordering_policy"],
            "visible_filter_then_prefer_enabled_then_snapshot_index"
        );
        assert_eq!(result["result"]["ranking_policy"]["visible"], true);
        assert_eq!(result["result"]["ranking_policy"]["prefer_enabled"], true);
        assert_eq!(result["result"]["ranking_policy"]["topmost"], false);
        assert_eq!(
            result["result"]["disambiguation_hints"]["labels"],
            json!(["First", "Second"])
        );
    }

    #[test]
    fn locator_explain_reports_out_of_range_selection() {
        let result = project_locator_explain_response(
            &crate::commands::ElementAddressArgs {
                snapshot: None,
                element_ref: None,
                selector: None,
                target_text: Some("Consent".to_string()),
                role: None,
                label: None,
                testid: None,
                visible: false,
                prefer_enabled: false,
                topmost: false,
                first: false,
                last: false,
                nth: Some(4),
            },
            Some(json!({
                "subject": {
                    "surface": "interactive_snapshot",
                    "locator": { "target_text": "Consent" }
                },
                "result": {
                    "match_count": 2,
                    "returned_count": 2,
                    "snapshot_id": "snap-2",
                    "matches": [
                        { "index": 1, "text": "Consent", "role": "button", "label": "Consent" },
                        { "index": 2, "text": "Consent", "role": "button", "label": "Accept cookies" }
                    ]
                }
            })),
        )
        .expect("locator explain should project out-of-range selections");

        assert_eq!(
            result["result"]["selection_outcome"]["kind"],
            "explicit_selection_out_of_range"
        );
        assert_eq!(result["result"]["selected_candidate"], Value::Null);
        assert!(
            result["result"]["guidance"]["summary"]
                .as_str()
                .expect("summary string")
                .contains("does not map to any candidate")
        );
    }
}
