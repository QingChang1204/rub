use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{TabInfo, TriggerConditionSpec};
use rub_core::storage::{StorageArea, StorageSnapshot};

pub(super) fn resolve_tab<'a>(
    tabs: &'a [TabInfo],
    tab_target_id: &str,
) -> Result<&'a TabInfo, RubError> {
    tabs.iter()
        .find(|tab| tab.target_id == tab_target_id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::TabNotFound,
                format!(
                    "Source tab target '{}' is not present in the current session",
                    tab_target_id
                ),
            )
        })
}

pub(super) fn readiness_matches(
    readiness: &rub_core::model::ReadinessInfo,
    requested: &str,
) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return false;
    }

    if requested == "ready" {
        return matches!(
            readiness.route_stability,
            rub_core::model::RouteStability::Stable
        ) && readiness.degraded_reason.is_none();
    }

    requested == format!("{:?}", readiness.status).to_ascii_lowercase()
        || requested == format!("{:?}", readiness.route_stability).to_ascii_lowercase()
        || readiness
            .document_ready_state
            .as_deref()
            .map(|state| state.eq_ignore_ascii_case(&requested))
            .unwrap_or(false)
}

pub(super) fn network_request_matches(
    record: &rub_core::model::NetworkRequestRecord,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
) -> bool {
    if record.tab_target_id.as_deref() != Some(tab_target_id) {
        return false;
    }

    if let Some(frame_id) = frame_id
        && record.frame_id.as_deref() != Some(frame_id)
    {
        return false;
    }

    let url_pattern = condition.url_pattern.as_deref().unwrap_or_default();
    if !record.url.contains(url_pattern) {
        return false;
    }

    if let Some(method) = condition.method.as_deref()
        && !record.method.eq_ignore_ascii_case(method)
    {
        return false;
    }

    if let Some(status_code) = condition.status_code
        && record.status != Some(status_code)
    {
        return false;
    }

    true
}

pub(super) fn storage_snapshot_matches(
    snapshot: &StorageSnapshot,
    condition: &TriggerConditionSpec,
) -> Result<bool, RubError> {
    let key = condition.key.as_deref().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "orchestration storage_value condition is missing condition.key",
        )
    })?;
    let expected_value = condition.value.as_deref();
    let area = condition.storage_area;

    let empty = std::collections::BTreeMap::new();
    let areas = match area {
        Some(StorageArea::Local) => [&snapshot.local_storage, &empty],
        Some(StorageArea::Session) => [&empty, &snapshot.session_storage],
        None => [&snapshot.local_storage, &snapshot.session_storage],
    };

    for entries in areas {
        if let Some(value) = entries.get(key)
            && expected_value
                .map(|expected| expected == value)
                .unwrap_or(true)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
