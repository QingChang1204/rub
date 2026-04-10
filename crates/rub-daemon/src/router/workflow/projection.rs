pub(super) fn workflow_step_projection(
    step_index: usize,
    command: &str,
    label: Option<String>,
    role: Option<&str>,
    data: serde_json::Value,
) -> serde_json::Value {
    let mut action = serde_json::Map::new();
    action.insert("kind".to_string(), serde_json::json!("command"));
    action.insert("command".to_string(), serde_json::json!(command));
    if let Some(label) = label {
        action.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(role) = role {
        action.insert("role".to_string(), serde_json::json!(role));
    }

    serde_json::json!({
        "step_index": step_index,
        "status": "committed",
        "action": action,
        "result": data,
    })
}
