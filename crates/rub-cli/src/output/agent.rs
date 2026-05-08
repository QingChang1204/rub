use rub_core::model::CommandResult;
use serde_json::{Map, Value, json};
use std::path::Path;

const AGENT_SCHEMA_VERSION: &str = "1";
const DEFAULT_ARRAY_LIMIT: usize = 10;
const DEFAULT_STRING_LIMIT: usize = 500;

pub(super) fn project_agent_result(result: &CommandResult, rub_home: &Path) -> Value {
    let mut root = Map::new();
    root.insert(
        "agent_schema_version".to_string(),
        Value::String(AGENT_SCHEMA_VERSION.to_string()),
    );
    root.insert("ok".to_string(), Value::Bool(result.success));

    let mut ctx = base_context(result);
    let mut more = more_object(result, rub_home);
    let mut evidence = Vec::new();

    if result.success {
        if let Some(data) = result.data.as_ref() {
            extend_context_from_data(&mut ctx, data);
            evidence.extend(evidence_from_data(data));
            if let Some(value) = interaction_projection(data) {
                root.insert("result".to_string(), value);
            } else if let Some(value) = navigation_projection(data) {
                root.insert("result".to_string(), value);
            } else if let Some(value) = extract_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else if let Some(value) = storage_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else if let Some(value) = cookies_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else if let Some(value) = dialog_projection(data) {
                root.insert("result".to_string(), value);
            } else if let Some(value) = network_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else if let Some(value) = find_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else if let Some(value) = observation_projection(data) {
                merge_projection(&mut root, value, &mut more);
            } else {
                root.insert("result".to_string(), generic_result(data));
            }
        } else {
            root.insert("result".to_string(), json!({ "kind": "empty" }));
        }
    } else if let Some(error) = result.error.as_ref() {
        root.insert("error".to_string(), error_projection(error));
        evidence.extend(error_evidence(error));
    }

    root.insert("ctx".to_string(), Value::Object(ctx));
    if !evidence.is_empty() {
        root.insert("evidence".to_string(), Value::Array(evidence));
    }
    if !more.is_empty() {
        root.insert("more".to_string(), Value::Object(more));
    }

    Value::Object(root)
}

fn base_context(result: &CommandResult) -> Map<String, Value> {
    let mut ctx = Map::new();
    ctx.insert("cmd".to_string(), Value::String(result.command.clone()));
    ctx.insert("session".to_string(), Value::String(result.session.clone()));
    ctx.insert(
        "id".to_string(),
        Value::String(
            result
                .command_id
                .clone()
                .unwrap_or_else(|| result.request_id.clone()),
        ),
    );
    ctx.insert(
        "request_id".to_string(),
        Value::String(result.request_id.clone()),
    );
    ctx.insert(
        "elapsed_ms".to_string(),
        json!(result.timing.total_ms.max(result.timing.exec_ms)),
    );
    ctx
}

fn more_object(result: &CommandResult, rub_home: &Path) -> Map<String, Value> {
    let mut more = Map::new();
    let mut history = "rub history --last 1".to_string();
    if !rub_home.as_os_str().is_empty() {
        history.push_str(&format!(
            " --rub-home {}",
            shell_quote(&rub_home.display().to_string())
        ));
    }
    more.insert("history".to_string(), Value::String(history));
    if let Some(command_id) = result.command_id.as_deref() {
        more.insert(
            "command_id".to_string(),
            Value::String(command_id.to_string()),
        );
    }
    more
}

fn extend_context_from_data(ctx: &mut Map<String, Value>, data: &Value) {
    if let Some(snapshot) = snapshot_value(data) {
        copy_path(snapshot, ctx, "snapshot_id", "snapshot");
        copy_path(snapshot, ctx, "dom_epoch", "epoch");
        copy_path(snapshot, ctx, "url", "url");
        copy_path(snapshot, ctx, "title", "title");
        if let Some(frame_id) = snapshot
            .get("frame_context")
            .and_then(|value| value.get("frame_id"))
            .cloned()
        {
            ctx.insert("frame".to_string(), frame_id);
        }
    }
    if let Some(page) = data
        .get("result")
        .and_then(|value| value.get("page"))
        .or_else(|| {
            data.get("result")
                .and_then(|value| value.get("page_metadata"))
        })
    {
        copy_path(page, ctx, "url", "url");
        copy_path(page, ctx, "title", "title");
    }
}

fn copy_path(source: &Value, target: &mut Map<String, Value>, source_key: &str, target_key: &str) {
    if let Some(value) = source.get(source_key) {
        target.insert(target_key.to_string(), value.clone());
    }
}

fn observation_projection(data: &Value) -> Option<Value> {
    let snapshot = snapshot_value(data)?;
    let format = snapshot
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            snapshot
                .get("summary")
                .and_then(|value| value.get("format"))
                .and_then(Value::as_str)
                .unwrap_or("snapshot")
        });
    let snapshot_id = snapshot.get("snapshot_id").and_then(Value::as_str);
    let items = observation_items(snapshot, snapshot_id);

    let mut projected = Map::new();
    projected.insert(
        "result".to_string(),
        json!({
            "kind": "page_observation",
            "format": format,
            "entry_count": snapshot.get("entry_count").cloned()
                .or_else(|| snapshot.get("elements").and_then(Value::as_array).map(|items| json!(items.len())))
                .unwrap_or(Value::Null),
            "total_count": snapshot.get("total_count").cloned().unwrap_or(Value::Null),
            "truncated": snapshot.get("truncated").cloned().unwrap_or(Value::Bool(false)),
        }),
    );
    if !items.is_empty() {
        projected.insert("items".to_string(), Value::Array(items));
    } else if let Some(text) = snapshot
        .get("compact_text")
        .or_else(|| snapshot.get("a11y_text"))
        .or_else(|| {
            snapshot
                .get("summary")
                .and_then(|summary| summary.get("text"))
        })
        .and_then(Value::as_str)
    {
        projected.insert(
            "items_text".to_string(),
            Value::String(truncate_string(text, DEFAULT_STRING_LIMIT)),
        );
    }
    Some(Value::Object(projected))
}

fn snapshot_value(data: &Value) -> Option<&Value> {
    data.get("result").and_then(|value| value.get("snapshot"))
}

fn observation_items(snapshot: &Value, snapshot_id: Option<&str>) -> Vec<Value> {
    if let Some(elements) = snapshot.get("elements").and_then(Value::as_array) {
        return elements
            .iter()
            .take(DEFAULT_ARRAY_LIMIT)
            .map(|element| element_item(element, snapshot_id))
            .collect();
    }
    if let Some(entries) = snapshot.get("entries").and_then(Value::as_array) {
        return entries
            .iter()
            .take(DEFAULT_ARRAY_LIMIT)
            .map(|entry| compact_entry_item(entry, snapshot_id))
            .collect();
    }
    if let Some(entries) = snapshot.get("element_map").and_then(Value::as_array) {
        return entries
            .iter()
            .take(DEFAULT_ARRAY_LIMIT)
            .map(|entry| compact_entry_item(entry, snapshot_id))
            .collect();
    }
    Vec::new()
}

fn element_item(element: &Value, snapshot_id: Option<&str>) -> Value {
    let index = element.get("index").cloned().unwrap_or(Value::Null);
    let name = element
        .get("ax_info")
        .and_then(|ax| ax.get("accessible_name"))
        .or_else(|| {
            element
                .get("attributes")
                .and_then(|attrs| attrs.get("aria-label"))
        })
        .or_else(|| {
            element
                .get("attributes")
                .and_then(|attrs| attrs.get("placeholder"))
        })
        .or_else(|| element.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json_strip_nulls(json!({
        "snapshot": snapshot_id,
        "index": index,
        "ref": element.get("element_ref").cloned(),
        "role": element.get("ax_info").and_then(|ax| ax.get("role")).cloned()
            .or_else(|| element.get("attributes").and_then(|attrs| attrs.get("role")).cloned())
            .or_else(|| element.get("tag").cloned()),
        "name": truncate_string(name, 120),
    }))
}

fn compact_entry_item(entry: &Value, snapshot_id: Option<&str>) -> Value {
    json_strip_nulls(json!({
        "snapshot": snapshot_id,
        "index": entry.get("index").cloned(),
        "role": entry.get("role").cloned(),
        "name": entry.get("label").cloned(),
        "flags": entry.get("flags").cloned(),
    }))
}

fn interaction_projection(data: &Value) -> Option<Value> {
    let interaction = data.get("interaction")?;
    let subject = data.get("subject");
    Some(json_strip_nulls(json!({
        "kind": "interaction",
        "status": interaction.get("confirmation_status").cloned()
            .unwrap_or_else(|| if data.get("wait_after").and_then(|wait| wait.get("matched")).and_then(Value::as_bool).unwrap_or(false) {
                Value::String("confirmed_by_wait_after".to_string())
            } else {
                Value::String("unknown".to_string())
            }),
        "semantic_class": interaction.get("semantic_class").cloned(),
        "confirmed": interaction.get("interaction_confirmed").cloned(),
        "confirmation_kind": interaction.get("confirmation_kind").cloned(),
        "target": subject.and_then(target_from_subject),
    })))
}

fn target_from_subject(subject: &Value) -> Option<Value> {
    let object = subject.as_object()?;
    if object.get("kind").and_then(Value::as_str) != Some("element") {
        return None;
    }
    Some(json_strip_nulls(json!({
        "snapshot": object.get("snapshot_id").cloned(),
        "index": object.get("index").cloned(),
        "role": object.get("tag").cloned(),
        "name": object.get("text").and_then(Value::as_str).map(|text| truncate_string(text, 120)),
    })))
}

fn navigation_projection(data: &Value) -> Option<Value> {
    let subject = data.get("subject")?;
    if subject.get("kind").and_then(Value::as_str) != Some("tab_navigation") {
        return None;
    }
    let result = data.get("result")?;
    let page = result.get("page");
    let active_tab = result.get("active_tab");
    Some(json_strip_nulls(json!({
        "kind": "tab_navigation",
        "requested_url": subject.get("requested_url").cloned(),
        "url": page.and_then(|page| page.get("url")).cloned()
            .or_else(|| page.and_then(|page| page.get("final_url")).cloned()),
        "title": page.and_then(|page| page.get("title")).cloned(),
        "http_status": page.and_then(|page| page.get("http_status")).cloned(),
        "navigation_warning": page.and_then(|page| page.get("navigation_warning")).cloned(),
        "active_tab": active_tab.map(|tab| json_strip_nulls(json!({
            "index": tab.get("index").cloned(),
            "active": tab.get("active").cloned(),
            "authority": tab.get("active_authority").cloned(),
        }))),
    })))
}

fn network_projection(data: &Value) -> Option<Value> {
    let subject_kind = data
        .get("subject")
        .and_then(|subject| subject.get("kind"))
        .and_then(Value::as_str)?;
    if !matches!(
        subject_kind,
        "network_request" | "network_request_registry" | "network_request_wait"
    ) {
        return None;
    }
    let result = data.get("result")?;
    let mut projected = Map::new();
    projected.insert(
        "result".to_string(),
        json!({
            "kind": subject_kind,
            "matched": result.get("matched").cloned(),
            "elapsed_ms": result.get("elapsed_ms").cloned(),
        }),
    );
    let items = if let Some(items) = result.get("items").and_then(Value::as_array) {
        items
            .iter()
            .take(DEFAULT_ARRAY_LIMIT)
            .map(network_item)
            .collect()
    } else if let Some(request) = result.get("request") {
        vec![network_item(request)]
    } else {
        Vec::new()
    };
    if !items.is_empty() {
        projected.insert("items".to_string(), Value::Array(items));
    }
    Some(Value::Object(projected))
}

fn network_item(request: &Value) -> Value {
    json_strip_nulls(json!({
        "id": request.get("request_id").cloned(),
        "sequence": request.get("sequence").cloned(),
        "method": request.get("method").cloned(),
        "url": request.get("url").and_then(Value::as_str).map(|url| truncate_string(url, 240)),
        "status": request.get("status").cloned(),
        "lifecycle": request.get("lifecycle").cloned(),
    }))
}

fn extract_projection(data: &Value) -> Option<Value> {
    let subject = data.get("subject")?;
    if subject.get("kind").and_then(Value::as_str) != Some("extract_query") {
        return None;
    }
    let result = data.get("result")?;
    let fields = result.get("fields").cloned().unwrap_or(Value::Null);
    Some(Value::Object(Map::from_iter([
        (
            "result".to_string(),
            json_strip_nulls(json!({
                "kind": "structured_extract",
                "field_count": result.get("field_count").cloned(),
                "source": subject.get("source").cloned(),
                "snapshot": result.get("snapshot").and_then(|snapshot| snapshot.get("snapshot_id")).cloned(),
            })),
        ),
        ("fields".to_string(), compact_value(&fields, 0)),
    ])))
}

fn storage_projection(data: &Value) -> Option<Value> {
    let subject = data.get("subject")?;
    if subject.get("kind").and_then(Value::as_str) != Some("storage") {
        return None;
    }
    let result = data.get("result")?;
    let runtime = data.get("runtime");
    let matches = result
        .get("matches")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(DEFAULT_ARRAY_LIMIT)
                .map(storage_match_item)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut projected = Map::new();
    projected.insert(
        "result".to_string(),
        json_strip_nulls(json!({
            "kind": "storage",
            "key": subject.get("key").cloned(),
            "area": subject.get("area").cloned(),
            "origin": subject.get("origin").cloned(),
            "value": result.get("value").cloned(),
            "match_count": result.get("matches").and_then(Value::as_array).map(|items| items.len()),
            "local_key_count": runtime.and_then(|runtime| runtime.get("local_storage_keys")).and_then(Value::as_array).map(|items| items.len()),
            "session_key_count": runtime.and_then(|runtime| runtime.get("session_storage_keys")).and_then(Value::as_array).map(|items| items.len()),
            "last_mutation": runtime
                .and_then(|runtime| runtime.get("recent_mutations"))
                .and_then(Value::as_array)
                .and_then(|items| items.last())
                .map(storage_mutation_item),
        })),
    );
    if !matches.is_empty() {
        projected.insert("items".to_string(), Value::Array(matches));
    }
    Some(Value::Object(projected))
}

fn storage_match_item(item: &Value) -> Value {
    json_strip_nulls(json!({
        "area": item.get("area").cloned(),
        "value": item.get("value").cloned(),
    }))
}

fn storage_mutation_item(item: &Value) -> Value {
    json_strip_nulls(json!({
        "kind": item.get("kind").cloned(),
        "area": item.get("area").cloned(),
        "key": item.get("key").cloned(),
        "commit_status": item.get("commit_status").cloned(),
    }))
}

fn cookies_projection(data: &Value) -> Option<Value> {
    let subject = data.get("subject")?;
    let subject_kind = subject.get("kind").and_then(Value::as_str)?;
    if !matches!(subject_kind, "cookie" | "cookies") {
        return None;
    }
    let result = data.get("result")?;
    let items = if let Some(cookies) = result.get("cookies").and_then(Value::as_array) {
        cookies
            .iter()
            .take(DEFAULT_ARRAY_LIMIT)
            .map(cookie_item)
            .collect()
    } else if let Some(cookie) = result.get("cookie") {
        vec![cookie_item(cookie)]
    } else {
        Vec::new()
    };

    let mut projected = Map::new();
    projected.insert(
        "result".to_string(),
        json_strip_nulls(json!({
            "kind": subject_kind,
            "name": subject.get("name").cloned(),
            "url": subject.get("url").cloned(),
            "cookie_count": result.get("cookies").and_then(Value::as_array).map(|items| items.len())
                .or_else(|| result.get("cookie").map(|_| 1)),
        })),
    );
    if !items.is_empty() {
        projected.insert("items".to_string(), Value::Array(items));
    }
    Some(Value::Object(projected))
}

fn cookie_item(item: &Value) -> Value {
    json_strip_nulls(json!({
        "name": item.get("name").cloned(),
        "value": item.get("value").cloned(),
        "domain": item.get("domain").cloned(),
        "path": item.get("path").cloned(),
        "secure": item.get("secure").cloned(),
        "http_only": item.get("http_only").cloned(),
        "same_site": item.get("same_site").cloned(),
    }))
}

fn dialog_projection(data: &Value) -> Option<Value> {
    let subject = data.get("subject")?;
    let subject_kind = subject.get("kind").and_then(Value::as_str)?;
    if !matches!(
        subject_kind,
        "dialog_runtime" | "dialog_action" | "dialog_intercept"
    ) {
        return None;
    }
    let runtime = data.get("runtime");
    let result = data.get("result");
    let intercept = data.get("intercept");
    Some(json_strip_nulls(json!({
        "kind": subject_kind,
        "action": subject.get("action").cloned(),
        "status": runtime.and_then(|runtime| runtime.get("status")).cloned(),
        "pending": runtime
            .and_then(|runtime| runtime.get("pending_dialog"))
            .or_else(|| result.and_then(|result| result.get("pending_dialog")))
            .map(dialog_item),
        "last_dialog": runtime
            .and_then(|runtime| runtime.get("last_dialog"))
            .or_else(|| result.and_then(|result| result.get("last_dialog")))
            .map(dialog_item),
        "last_result": runtime
            .and_then(|runtime| runtime.get("last_result"))
            .or_else(|| result.and_then(|result| result.get("last_result")))
            .map(compact_value_shallow),
        "intercept": intercept.map(compact_value_shallow),
    })))
}

fn dialog_item(item: &Value) -> Value {
    json_strip_nulls(json!({
        "kind": item.get("kind").cloned(),
        "message": item.get("message").and_then(Value::as_str).map(|message| truncate_string(message, 160)),
        "url": item.get("url").and_then(Value::as_str).map(|url| truncate_string(url, 240)),
        "default_prompt": item.get("default_prompt").cloned(),
        "has_browser_handler": item.get("has_browser_handler").cloned(),
    }))
}

fn compact_value_shallow(value: &Value) -> Value {
    compact_value(value, 2)
}

fn find_projection(data: &Value) -> Option<Value> {
    let result = data.get("result")?;
    let matches = result.get("matches").and_then(Value::as_array)?;
    let items = matches
        .iter()
        .take(DEFAULT_ARRAY_LIMIT)
        .enumerate()
        .map(|(fallback_index, item)| find_item(item, fallback_index))
        .collect::<Vec<_>>();
    let mut projected = Map::new();
    projected.insert(
        "result".to_string(),
        json_strip_nulls(json!({
            "kind": "locator_matches",
            "match_count": result.get("match_count").cloned(),
            "returned_count": result.get("returned_count").cloned(),
            "truncated": result.get("truncated").cloned(),
        })),
    );
    if !items.is_empty() {
        projected.insert("items".to_string(), Value::Array(items));
    }
    Some(Value::Object(projected))
}

fn find_item(item: &Value, fallback_index: usize) -> Value {
    let name = item
        .get("label")
        .or_else(|| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json_strip_nulls(json!({
        "index": item.get("index").cloned().unwrap_or_else(|| json!(fallback_index)),
        "role": item.get("role").cloned(),
        "tag": item.get("tag_name").cloned().or_else(|| item.get("tag").cloned()),
        "name": truncate_string(name, 160),
    }))
}

fn evidence_from_data(data: &Value) -> Vec<Value> {
    let mut evidence = Vec::new();
    if let Some(wait_after) = data.get("wait_after")
        && wait_after
            .get("matched")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        evidence.push(json!({ "kind": "wait_after", "matched": true }));
    }
    if let Some(requests) = data
        .get("interaction")
        .and_then(|interaction| interaction.get("network_requests"))
        && let Some(last) = requests.get("last_request")
    {
        evidence.push(json_strip_nulls(json!({
            "kind": "network",
            "id": last.get("request_id").cloned(),
            "status": last.get("status").cloned(),
            "url": last.get("url").and_then(Value::as_str).map(|url| truncate_string(url, 240)),
        })));
    }
    if let Some(download) = data
        .get("interaction")
        .and_then(|interaction| interaction.get("downloads"))
        .and_then(|downloads| downloads.get("last_download"))
    {
        evidence.push(json_strip_nulls(json!({
            "kind": "download",
            "id": download.get("guid").cloned().or_else(|| download.get("id").cloned()),
            "state": download.get("state").cloned(),
        })));
    }
    evidence
}

fn error_evidence(error: &rub_core::error::ErrorEnvelope) -> Vec<Value> {
    let committed = error
        .context
        .as_ref()
        .and_then(|context| context.get("daemon_request_committed"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || error
            .context
            .as_ref()
            .and_then(|context| context.get("committed_response_projection"))
            .is_some();
    if committed {
        vec![json!({ "kind": "commit_state", "committed": true })]
    } else {
        Vec::new()
    }
}

fn error_projection(error: &rub_core::error::ErrorEnvelope) -> Value {
    let reason = error
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(Value::as_str);
    let effect_state = error
        .context
        .as_ref()
        .and_then(|context| context.get("effect_state"))
        .and_then(|state| state.get("confirmation_status"))
        .cloned();
    json_strip_nulls(json!({
        "code": error.code,
        "message": truncate_string(&error.message, 300),
        "reason": reason,
        "status": effect_state,
        "committed": error.context.as_ref()
            .and_then(|context| context.get("daemon_request_committed"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || error.context.as_ref()
                .and_then(|context| context.get("committed_response_projection"))
                .is_some(),
    }))
}

fn generic_result(data: &Value) -> Value {
    if let Some(result) = data.get("result") {
        compact_value(result, 0)
    } else {
        compact_value(data, 0)
    }
}

fn compact_value(value: &Value, depth: usize) -> Value {
    match value {
        Value::Object(object) => {
            if depth >= 4 {
                return json!({ "omitted": true, "reason": "agent_brief_depth_limit" });
            }
            let mut projected = Map::new();
            for (key, value) in object {
                if is_heavy_key(key) {
                    continue;
                }
                projected.insert(key.clone(), compact_value(value, depth + 1));
            }
            Value::Object(projected)
        }
        Value::Array(items) => {
            if depth >= 4 {
                return json!({
                    "omitted": true,
                    "reason": "agent_brief_depth_limit",
                    "count": items.len()
                });
            }
            Value::Array(
                items
                    .iter()
                    .take(DEFAULT_ARRAY_LIMIT)
                    .map(|item| compact_value(item, depth + 1))
                    .collect(),
            )
        }
        Value::String(text) => Value::String(truncate_string(text, DEFAULT_STRING_LIMIT)),
        other => other.clone(),
    }
}

fn is_heavy_key(key: &str) -> bool {
    matches!(
        key,
        "attributes"
            | "bounding_box"
            | "bbox"
            | "listeners"
            | "element_map"
            | "base64"
            | "request_headers"
            | "response_headers"
            | "request_body"
            | "response_body"
            | "committed_response_projection"
            | "confirmation_details"
            | "runtime_state_delta"
            | "runtime_observatory_events"
            | "interference"
            | "observed_effects"
            | "suggestion"
            | "next_command_hints"
            | "next_safe_actions"
            | "workflow_guidance"
            | "authority_guidance"
            | "workflow_continuity"
    )
}

fn merge_projection(root: &mut Map<String, Value>, value: Value, more: &mut Map<String, Value>) {
    let Some(object) = value.as_object() else {
        root.insert("result".to_string(), value);
        return;
    };
    for (key, value) in object {
        if key == "more" {
            if let Some(object) = value.as_object() {
                more.extend(object.clone());
            }
        } else {
            root.insert(key.clone(), value.clone());
        }
    }
}

fn json_strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter_map(|(key, value)| {
                    if value.is_null() {
                        None
                    } else {
                        Some((key, json_strip_nulls(value)))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(json_strip_nulls).collect()),
        other => other,
    }
}

fn truncate_string(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = 0;
    for (idx, _) in value.char_indices() {
        if idx > limit {
            break;
        }
        end = idx;
    }
    format!("{}...[truncated]", &value[..end])
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-._~/:".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
