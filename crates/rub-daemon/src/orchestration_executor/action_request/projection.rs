use super::OrchestrationActionExecutionInfo;
use rub_core::model::TriggerActionKind;

pub(super) fn orchestration_action_label(action: &OrchestrationActionExecutionInfo) -> String {
    match action.kind {
        TriggerActionKind::BrowserCommand => format!(
            "'{}'",
            action.command.as_deref().unwrap_or("browser_command")
        ),
        TriggerActionKind::Workflow => action
            .workflow_name
            .as_deref()
            .map(|name| format!("workflow '{name}'"))
            .unwrap_or_else(|| "inline workflow".to_string()),
        TriggerActionKind::Provider => "provider action".to_string(),
        TriggerActionKind::Script => "script action".to_string(),
        TriggerActionKind::Webhook => "webhook action".to_string(),
    }
}
