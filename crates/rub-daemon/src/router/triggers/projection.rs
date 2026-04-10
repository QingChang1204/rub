use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    TabInfo, TriggerInfo, TriggerRegistrationSpec, TriggerStatus, TriggerTabBindingInfo,
};

pub(super) fn trigger_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: &rub_core::model::TriggerRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

pub(super) fn trigger_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "trigger_registry",
    })
}

pub(super) fn trigger_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "trigger",
        "id": id,
    })
}

pub(super) fn resolve_trigger_tab_binding(
    tabs: &[TabInfo],
    index: u32,
    role: &str,
) -> Result<TriggerTabBindingInfo, RubError> {
    let tab = tabs.iter().find(|tab| tab.index == index).ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("{role} tab index {index} is not present in the current session"),
        )
    })?;
    Ok(TriggerTabBindingInfo {
        index: tab.index,
        target_id: tab.target_id.clone(),
        url: tab.url.clone(),
        title: tab.title.clone(),
    })
}

pub(super) fn trigger_registration_equivalent(
    existing: &TriggerInfo,
    source_tab: &TriggerTabBindingInfo,
    target_tab: &TriggerTabBindingInfo,
    spec: &TriggerRegistrationSpec,
) -> bool {
    existing.mode == spec.mode
        && existing.source_tab.target_id == source_tab.target_id
        && existing.target_tab.target_id == target_tab.target_id
        && existing.condition == spec.condition
        && existing.action == spec.action
}

pub(super) fn trigger_registration_reusable(
    existing: &TriggerInfo,
    source_tab: &TriggerTabBindingInfo,
    target_tab: &TriggerTabBindingInfo,
    spec: &TriggerRegistrationSpec,
) -> bool {
    matches!(existing.status, TriggerStatus::Armed)
        && trigger_registration_equivalent(existing, source_tab, target_tab, spec)
}

pub(super) fn trigger_status_name(status: TriggerStatus) -> &'static str {
    match status {
        TriggerStatus::Armed => "armed",
        TriggerStatus::Paused => "paused",
        TriggerStatus::Fired => "fired",
        TriggerStatus::Blocked => "blocked",
        TriggerStatus::Degraded => "degraded",
        TriggerStatus::Expired => "expired",
    }
}
