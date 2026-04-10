use super::export::WorkflowExportProjection;

pub(super) fn history_subject(
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "command_history",
        "selection": export_selection_json(last, from, to),
    })
}

pub(super) fn history_capture_window_json(
    projection: &WorkflowExportProjection,
) -> serde_json::Value {
    serde_json::json!({
        "oldest_retained_sequence": projection.capture_oldest_retained_sequence,
        "newest_retained_sequence": projection.capture_newest_retained_sequence,
        "dropped_before_retention": projection.capture_dropped_before_retention,
        "dropped_before_projection": projection.capture_dropped_before_projection,
        "truncated": !projection.complete,
    })
}

pub(super) fn command_history_projection_state_json(
    projection: &crate::history::CommandHistoryProjection,
) -> serde_json::Value {
    let mut lossy_reasons = Vec::new();
    if projection.dropped_before_projection > 0 {
        lossy_reasons.push("dropped_before_projection");
    }
    if projection.dropped_before_retention > 0 {
        lossy_reasons.push("dropped_before_retention");
    }

    serde_json::json!({
        "surface": "command_history",
        "projection_kind": "bounded_post_commit_projection",
        "projection_authority": "session.history",
        "upstream_commit_truth": "daemon_response_committed",
        "lossy": !lossy_reasons.is_empty(),
        "lossy_reasons": lossy_reasons,
    })
}

pub(super) fn command_history_retention_window_json(
    projection: &crate::history::CommandHistoryProjection,
) -> serde_json::Value {
    serde_json::json!({
        "oldest_retained_sequence": projection.oldest_retained_sequence,
        "newest_retained_sequence": projection.newest_retained_sequence,
        "dropped_before_retention": projection.dropped_before_retention,
    })
}

pub(super) fn workflow_export_projection_state_json(
    projection: &WorkflowExportProjection,
) -> serde_json::Value {
    let mut lossy_reasons = Vec::new();
    if projection.capture_dropped_before_projection > 0 {
        lossy_reasons.push("dropped_before_projection");
    }
    if projection.capture_dropped_before_retention > 0 {
        lossy_reasons.push("dropped_before_retention");
    }
    if !projection.complete {
        lossy_reasons.push("retention_truncated");
    }

    serde_json::json!({
        "surface": "workflow_capture_export",
        "projection_kind": "bounded_post_commit_projection",
        "projection_authority": "session.workflow_capture",
        "upstream_commit_truth": "daemon_response_committed",
        "lossy": !lossy_reasons.is_empty(),
        "lossy_reasons": lossy_reasons,
    })
}

pub(super) fn export_selection_json(
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
) -> serde_json::Value {
    if from.is_some() || to.is_some() {
        serde_json::json!({
            "from": from,
            "to": to,
        })
    } else {
        serde_json::json!({
            "last": last,
        })
    }
}
