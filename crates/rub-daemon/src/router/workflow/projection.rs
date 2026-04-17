use rub_core::error::ErrorEnvelope;

fn workflow_step_action_projection(
    command: &str,
    label: Option<String>,
    role: Option<&str>,
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

    serde_json::Value::Object(action)
}

pub(super) fn workflow_step_projection(
    step_index: usize,
    command: &str,
    label: Option<String>,
    role: Option<&str>,
    data: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "step_index": step_index,
        "status": "committed",
        "action": workflow_step_action_projection(command, label, role),
        "result": data,
    })
}

pub(super) fn workflow_error_projection(envelope: &ErrorEnvelope) -> serde_json::Value {
    serde_json::json!({
        "code": envelope.code,
        "message": envelope.message,
        "suggestion": envelope.suggestion,
        "context": envelope.context,
    })
}

pub(super) fn workflow_failed_step_projection(
    step_index: usize,
    command: &str,
    label: Option<String>,
    role: Option<&str>,
    envelope: &ErrorEnvelope,
) -> serde_json::Value {
    serde_json::json!({
        "step_index": step_index,
        "status": "failed",
        "action": workflow_step_action_projection(command, label, role),
        "result": serde_json::Value::Null,
        "error": workflow_error_projection(envelope),
    })
}
