use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn workflow_new_item_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let matched_item = result.get("matched_item").and_then(Value::as_object);
    let open_hint = matched_item.and_then(matched_item_open_command_hint);
    let text_hint = matched_item.and_then(matched_item_text_command_hint);
    let mut hints = vec![super::command_hint(
        "rub inspect list ...",
        "re-read the current list surface in the same runtime",
    )];
    if let Some(open_hint) = open_hint {
        hints.push(open_hint);
    } else if let Some(text_hint) = text_hint {
        hints.push(text_hint);
    } else {
        hints.push(super::command_hint(
            "rub click ...",
            "open or act on the newly observed item without switching runtimes",
        ));
    }

    Some(super::workflow_same_runtime_projection(
        command,
        session,
        rub_home,
        "confirmed_new_item_observed",
        "A new matching item was observed in this runtime. Keep follow-up inspection or actuation in the same RUB_HOME/session.",
        hints,
        super::same_runtime_roles(
            "observation_runtime",
            "Keep using the current runtime as the observation surface while you follow up on the newly observed item.",
        ),
    ))
}

pub(super) fn workflow_find_query_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let subject = data.get("subject")?.as_object()?;
    let result = data.get("result")?.as_object()?;
    let surface = subject.get("surface").and_then(Value::as_str)?;
    let match_count = result
        .get("match_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let returned_count = result
        .get("returned_count")
        .and_then(Value::as_u64)
        .unwrap_or(match_count);
    let truncated = result
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let observation = json!({
        "kind": "find_surface_observation",
        "surface": surface,
        "match_count": match_count,
        "returned_count": returned_count,
        "truncated": truncated,
    });

    match surface {
        "content" => Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "find_content_anchor",
                "The current runtime resolved live content anchors. Stay on read/extract surfaces unless you specifically need an interactive target.",
                vec![
                    super::command_hint(
                        "rub get text ...",
                        "read a single value directly if you only need one content field from the current page",
                    ),
                    super::command_hint(
                        "rub extract ...",
                        "promote the content anchor into structured fields or repeated records without switching planes",
                    ),
                    super::command_hint(
                        "rub find ...",
                        "switch back to interactive targeting only if the next step is actuation rather than read/extract work",
                    ),
                ],
                super::same_runtime_roles(
                    "content_runtime",
                    "Keep using the current runtime as the content/read surface while you extract or inspect read-only page content.",
                ),
                Some(observation),
            ),
        )),
        "interactive_snapshot" => Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "find_interactive_candidates",
                "The current snapshot resolved interactive candidates. Stay on interaction or live-read surfaces, and pivot to content search only if you need read-only page text.",
                vec![
                    super::command_hint(
                        "rub click ...",
                        "act on one of the resolved interactive candidates in the current runtime",
                    ),
                    super::command_hint(
                        "rub get text ...",
                        "read a value from the resolved target without switching to a different runtime",
                    ),
                    super::command_hint(
                        "rub find --content ...",
                        "pivot to content-anchor discovery if the next step is reading page text rather than interacting",
                    ),
                ],
                super::same_runtime_roles(
                    "active_execution_runtime",
                    "Keep using the current runtime as the interaction surface while you choose the next actuation or live-read step.",
                ),
                Some(observation),
            ),
        )),
        _ => None,
    }
}

fn matched_item_open_command_hint(matched_item: &Map<String, Value>) -> Option<Value> {
    for key in ["activation_url", "target_url", "href", "url", "link"] {
        let Some(value) = matched_item.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return Some(super::command_hint(
            &format!("rub open {}", super::shell_double_quoted(value)),
            &format!("continue directly from the extracted `{key}` field in the current runtime"),
        ));
    }
    None
}

fn matched_item_text_command_hint(matched_item: &Map<String, Value>) -> Option<Value> {
    for key in ["subject", "title", "name", "label", "text"] {
        let Some(value) = matched_item.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return Some(super::command_hint(
            &format!(
                "rub click --target-text {}",
                super::shell_double_quoted(value)
            ),
            &format!(
                "act on the newly observed item using the extracted `{key}` text anchor in the current runtime"
            ),
        ));
    }
    None
}
