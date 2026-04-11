use rub_core::model::{
    DownloadEvent, FrameRuntimeInfo, InteractionActuation, InteractionConfirmation,
    InteractionConfirmationStatus, InteractionOutcome, InteractionSemanticClass,
    InterferenceRuntimeInfo, NetworkRequestLifecycle, NetworkRequestRecord,
    RuntimeObservatoryEvent, RuntimeStateSnapshot, SelectOutcome,
};

pub(crate) struct ProjectionSignals<'a> {
    pub frame_runtime: &'a FrameRuntimeInfo,
    pub runtime_before: Option<&'a RuntimeStateSnapshot>,
    pub runtime_after: Option<&'a RuntimeStateSnapshot>,
    pub interference_before: Option<&'a InterferenceRuntimeInfo>,
    pub interference_after: Option<&'a InterferenceRuntimeInfo>,
    pub observatory_events: &'a [RuntimeObservatoryEvent],
    pub observatory_authoritative: bool,
    pub observatory_degraded_reason: Option<&'a str>,
    pub network_requests: &'a [NetworkRequestRecord],
    pub network_authoritative: bool,
    pub network_degraded_reason: Option<&'a str>,
    pub download_events: &'a [DownloadEvent],
    pub download_authoritative: bool,
    pub download_degraded_reason: Option<&'a str>,
}

pub(crate) fn attach_interaction_projection(
    data: &mut serde_json::Value,
    outcome: &InteractionOutcome,
    signals: ProjectionSignals<'_>,
) {
    attach_interaction_metadata(
        data,
        outcome.semantic_class,
        outcome.element_verified,
        outcome.actuation,
        outcome.confirmation.as_ref(),
    );
    attach_frame_runtime(data, signals.frame_runtime);
    attach_context_turnover(data);
    attach_runtime_state_delta(data, signals.runtime_before, signals.runtime_after);
    attach_interference_delta(
        data,
        signals.interference_before,
        signals.interference_after,
    );
    attach_runtime_observatory_events(
        data,
        signals.observatory_events,
        signals.observatory_authoritative,
        signals.observatory_degraded_reason,
    );
    attach_network_requests(
        data,
        signals.network_requests,
        signals.network_authoritative,
        signals.network_degraded_reason,
    );
    attach_download_events(
        data,
        signals.download_events,
        signals.download_authoritative,
        signals.download_degraded_reason,
    );
    attach_observed_effects(data);
}

pub(crate) fn attach_select_projection(
    data: &mut serde_json::Value,
    outcome: &SelectOutcome,
    signals: ProjectionSignals<'_>,
) {
    attach_interaction_metadata(
        data,
        outcome.semantic_class,
        outcome.element_verified,
        outcome.actuation,
        outcome.confirmation.as_ref(),
    );
    attach_frame_runtime(data, signals.frame_runtime);
    attach_context_turnover(data);
    attach_runtime_state_delta(data, signals.runtime_before, signals.runtime_after);
    attach_interference_delta(
        data,
        signals.interference_before,
        signals.interference_after,
    );
    attach_runtime_observatory_events(
        data,
        signals.observatory_events,
        signals.observatory_authoritative,
        signals.observatory_degraded_reason,
    );
    attach_network_requests(
        data,
        signals.network_requests,
        signals.network_authoritative,
        signals.network_degraded_reason,
    );
    attach_download_events(
        data,
        signals.download_events,
        signals.download_authoritative,
        signals.download_degraded_reason,
    );
    attach_observed_effects(data);
}

fn attach_frame_runtime(data: &mut serde_json::Value, frame_runtime: &FrameRuntimeInfo) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    interaction.insert(
        "frame_context_status".to_string(),
        match serde_json::to_value(frame_runtime.status) {
            Ok(value) => value,
            Err(error) => {
                debug_assert!(
                    false,
                    "frame runtime status should serialize without failure: {error}"
                );
                tracing::warn!(error = %error, "Failed to serialize frame runtime status");
                return;
            }
        },
    );
    if let Some(current_frame) = frame_runtime.current_frame.as_ref() {
        interaction.insert(
            "frame_context".to_string(),
            match serde_json::to_value(current_frame) {
                Ok(value) => value,
                Err(error) => {
                    debug_assert!(
                        false,
                        "frame context should serialize without failure: {error}"
                    );
                    tracing::warn!(error = %error, "Failed to serialize frame context");
                    return;
                }
            },
        );
    }
    if !frame_runtime.frame_lineage.is_empty() {
        interaction.insert(
            "frame_lineage".to_string(),
            match serde_json::to_value(&frame_runtime.frame_lineage) {
                Ok(value) => value,
                Err(error) => {
                    debug_assert!(
                        false,
                        "frame lineage should serialize without failure: {error}"
                    );
                    tracing::warn!(error = %error, "Failed to serialize frame lineage");
                    return;
                }
            },
        );
    }
}

fn attach_context_turnover(data: &mut serde_json::Value) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let Some(details) = interaction
        .get("confirmation_details")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };

    let before_page = details.get("before_page").cloned();
    let after_page = details.get("after_page").cloned();
    let context_changed = details.get("context_changed").cloned();

    if before_page.is_none() && after_page.is_none() && context_changed.is_none() {
        return;
    }

    let context_replaced = after_page
        .as_ref()
        .and_then(|page| page.get("context_replaced"))
        .cloned()
        .unwrap_or(serde_json::Value::Bool(false));

    let mut turnover = serde_json::Map::new();
    if let Some(value) = context_changed {
        turnover.insert("context_changed".to_string(), value);
    }
    turnover.insert("context_replaced".to_string(), context_replaced);
    if let Some(value) = before_page {
        turnover.insert("before_page".to_string(), value);
    }
    if let Some(value) = after_page {
        turnover.insert("after_page".to_string(), value);
    }

    interaction.insert(
        "context_turnover".to_string(),
        serde_json::Value::Object(turnover),
    );
}

fn attach_runtime_state_delta(
    data: &mut serde_json::Value,
    runtime_before: Option<&RuntimeStateSnapshot>,
    runtime_after: Option<&RuntimeStateSnapshot>,
) {
    let Some(delta) = runtime_before
        .zip(runtime_after)
        .and_then(|(before, after)| crate::interaction_trace::runtime_state_delta(before, after))
    else {
        return;
    };

    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    interaction.insert(
        "runtime_state_delta".to_string(),
        match serde_json::to_value(delta) {
            Ok(value) => value,
            Err(error) => {
                debug_assert!(
                    false,
                    "runtime state delta should serialize without failure: {error}"
                );
                tracing::warn!(error = %error, "Failed to serialize runtime state delta");
                return;
            }
        },
    );
}

fn attach_runtime_observatory_events(
    data: &mut serde_json::Value,
    observatory_events: &[RuntimeObservatoryEvent],
    authoritative: bool,
    degraded_reason: Option<&str>,
) {
    if observatory_events.is_empty() && authoritative {
        return;
    }

    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    if !observatory_events.is_empty() {
        interaction.insert(
            "runtime_observatory_events".to_string(),
            match serde_json::to_value(observatory_events) {
                Ok(value) => value,
                Err(error) => {
                    debug_assert!(
                        false,
                        "runtime observatory events should serialize without failure: {error}"
                    );
                    tracing::warn!(error = %error, "Failed to serialize runtime observatory events");
                    return;
                }
            },
        );
    }
    if !authoritative {
        interaction.insert(
            "runtime_observatory_events_meta".to_string(),
            serde_json::json!({
                "authoritative": false,
                "degraded_reason": degraded_reason,
            }),
        );
    }
}

fn attach_download_events(
    data: &mut serde_json::Value,
    download_events: &[DownloadEvent],
    authoritative: bool,
    degraded_reason: Option<&str>,
) {
    if download_events.is_empty() && authoritative {
        return;
    }

    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let last_download = download_events.last().map(|event| event.download.clone());
    interaction.insert(
        "downloads".to_string(),
        serde_json::json!({
            "events": download_events,
            "last_download": last_download,
            "authoritative": authoritative,
            "degraded_reason": degraded_reason,
        }),
    );
}

fn attach_network_requests(
    data: &mut serde_json::Value,
    network_requests: &[NetworkRequestRecord],
    authoritative: bool,
    degraded_reason: Option<&str>,
) {
    if network_requests.is_empty() && authoritative {
        return;
    }

    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let last_request = network_requests.last().cloned();
    let terminal_count = network_requests
        .iter()
        .filter(|request| {
            matches!(
                request.lifecycle,
                NetworkRequestLifecycle::Completed | NetworkRequestLifecycle::Failed
            )
        })
        .count();
    interaction.insert(
        "network_requests".to_string(),
        serde_json::json!({
            "requests": network_requests,
            "terminal_count": terminal_count,
            "last_request": last_request,
            "authoritative": authoritative,
            "degraded_reason": degraded_reason,
        }),
    );
}

fn attach_interference_delta(
    data: &mut serde_json::Value,
    interference_before: Option<&InterferenceRuntimeInfo>,
    interference_after: Option<&InterferenceRuntimeInfo>,
) {
    let Some(delta) = interference_before
        .zip(interference_after)
        .and_then(|(before, after)| {
            crate::interaction_trace::interference_state_delta(before, after)
        })
    else {
        return;
    };

    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    interaction.insert(
        "interference".to_string(),
        match serde_json::to_value(delta) {
            Ok(value) => value,
            Err(error) => {
                debug_assert!(
                    false,
                    "interference delta should serialize without failure: {error}"
                );
                tracing::warn!(error = %error, "Failed to serialize interference delta");
                return;
            }
        },
    );
}

fn attach_observed_effects(data: &mut serde_json::Value) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get_mut("interaction")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let mut observed = serde_json::Map::new();
    if let Some(details) = interaction
        .get("confirmation_details")
        .and_then(serde_json::Value::as_object)
    {
        copy_json_field(details, &mut observed, "context_changed");
        copy_json_field(details, &mut observed, "before_checked");
        copy_json_field(details, &mut observed, "after_checked");
        copy_json_field(details, &mut observed, "before_hovered");
        copy_json_field(details, &mut observed, "after_hovered");
        copy_json_field(details, &mut observed, "before_active");
        copy_json_field(details, &mut observed, "after_active");
        copy_json_field(details, &mut observed, "value");
        copy_json_field(details, &mut observed, "selected_value");
        copy_json_field(details, &mut observed, "selected_text");
        copy_json_field(details, &mut observed, "file_names");
        copy_json_field(details, &mut observed, "expected");
        copy_json_field(details, &mut observed, "expected_value");
        copy_json_field(details, &mut observed, "expected_text");
        copy_json_field(details, &mut observed, "expected_file_name");

        if let Some(value) = details
            .get("observed")
            .and_then(|value| value.get("value"))
            .cloned()
        {
            observed.insert("observed_value".to_string(), value);
        }
        if let Some(value) = details
            .get("observed")
            .and_then(|value| value.get("selected_text"))
            .cloned()
        {
            observed.insert("observed_selected_text".to_string(), value);
        }
        if let Some(value) = details
            .get("observed")
            .and_then(|value| value.get("selected_value"))
            .cloned()
        {
            observed.insert("observed_selected_value".to_string(), value);
        }
        if let Some(value) = details
            .get("observed")
            .and_then(|value| value.get("file_names"))
            .cloned()
        {
            observed.insert("observed_file_names".to_string(), value);
        }

        if let Some(summary) = summarize_page(details.get("before_page")) {
            observed.insert("before_page".to_string(), summary);
        }
        if let Some(summary) = summarize_page(details.get("after_page")) {
            observed.insert("after_page".to_string(), summary);
        }
    }

    copy_interaction_field(interaction, &mut observed, "context_turnover");
    copy_interaction_field(interaction, &mut observed, "frame_context_status");
    copy_interaction_field(interaction, &mut observed, "frame_context");
    copy_interaction_field(interaction, &mut observed, "frame_lineage");
    copy_interaction_field(interaction, &mut observed, "runtime_state_delta");
    copy_interaction_field(interaction, &mut observed, "interference");
    copy_interaction_field(interaction, &mut observed, "runtime_observatory_events");
    copy_interaction_field(interaction, &mut observed, "network_requests");
    copy_interaction_field(interaction, &mut observed, "downloads");

    if !observed.is_empty() {
        interaction.insert(
            "observed_effects".to_string(),
            serde_json::Value::Object(observed),
        );
    }
}

fn copy_json_field(
    source: &serde_json::Map<String, serde_json::Value>,
    dest: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = source.get(key) {
        dest.insert(key.to_string(), value.clone());
    }
}

fn copy_interaction_field(
    interaction: &serde_json::Map<String, serde_json::Value>,
    observed: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) {
    if let Some(value) = interaction.get(key) {
        observed.insert(key.to_string(), value.clone());
    }
}

fn summarize_page(page: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let page = page?.as_object()?;
    let mut summary = serde_json::Map::new();
    copy_json_field(page, &mut summary, "url");
    copy_json_field(page, &mut summary, "title");
    copy_json_field(page, &mut summary, "context_replaced");
    if summary.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(summary))
    }
}

fn attach_interaction_metadata(
    data: &mut serde_json::Value,
    semantic_class: InteractionSemanticClass,
    element_verified: bool,
    actuation: Option<InteractionActuation>,
    confirmation: Option<&InteractionConfirmation>,
) {
    let Some(object) = data.as_object_mut() else {
        return;
    };

    let mut interaction = serde_json::Map::new();
    interaction.insert(
        "semantic_class".to_string(),
        serde_json::json!(semantic_class),
    );
    interaction.insert(
        "element_verified".to_string(),
        serde_json::json!(element_verified),
    );
    if let Some(actuation) = actuation {
        interaction.insert("actuation".to_string(), serde_json::json!(actuation));
    }
    if let Some(confirmation) = confirmation {
        interaction.insert(
            "interaction_confirmed".to_string(),
            serde_json::json!(confirmation.status == InteractionConfirmationStatus::Confirmed),
        );
        interaction.insert(
            "confirmation_status".to_string(),
            serde_json::json!(confirmation.status),
        );
        if let Some(kind) = confirmation.kind {
            interaction.insert("confirmation_kind".to_string(), serde_json::json!(kind));
        }
        if let Some(details) = &confirmation.details {
            interaction.insert("confirmation_details".to_string(), details.clone());
        }
    }

    object.insert(
        "interaction".to_string(),
        serde_json::Value::Object(interaction),
    );
}

#[cfg(test)]
mod tests {
    use super::{attach_download_events, attach_observed_effects};
    use rub_core::model::{DownloadEntry, DownloadEvent, DownloadEventKind, DownloadState};

    fn sample_download_event() -> DownloadEvent {
        DownloadEvent {
            sequence: 7,
            kind: DownloadEventKind::Completed,
            download: DownloadEntry {
                guid: "guid-1".to_string(),
                state: DownloadState::Completed,
                url: Some("https://example.test/report.csv".to_string()),
                suggested_filename: Some("report.csv".to_string()),
                final_path: Some("/tmp/rub-downloads/guid-1".to_string()),
                mime_hint: None,
                received_bytes: 128,
                total_bytes: Some(128),
                started_at: "2026-04-11T00:00:00Z".to_string(),
                completed_at: Some("2026-04-11T00:00:01Z".to_string()),
                frame_id: Some("frame-main".to_string()),
                trigger_command_id: None,
            },
        }
    }

    #[test]
    fn download_surface_truth_labels_propagate_into_observed_effects() {
        let mut data = serde_json::json!({
            "interaction": {}
        });

        attach_download_events(&mut data, &[sample_download_event()], true, None);
        attach_observed_effects(&mut data);

        assert_eq!(data["interaction"]["downloads"]["authoritative"], true);
        assert_eq!(
            data["interaction"]["downloads"]["degraded_reason"],
            serde_json::Value::Null
        );
        assert_eq!(
            data["interaction"]["observed_effects"]["downloads"]["authoritative"],
            true
        );
        assert_eq!(
            data["interaction"]["observed_effects"]["downloads"]["last_download"]["suggested_filename"],
            "report.csv"
        );
    }

    #[test]
    fn non_authoritative_empty_download_window_still_projects_degraded_surface() {
        let mut data = serde_json::json!({
            "interaction": {}
        });

        attach_download_events(
            &mut data,
            &[],
            false,
            Some("browser_event_ingress_overflow:download_progress"),
        );
        attach_observed_effects(&mut data);

        assert_eq!(data["interaction"]["downloads"]["authoritative"], false);
        assert_eq!(
            data["interaction"]["downloads"]["degraded_reason"],
            "browser_event_ingress_overflow:download_progress"
        );
        assert_eq!(
            data["interaction"]["observed_effects"]["downloads"]["authoritative"],
            false
        );
        assert!(
            data["interaction"]["observed_effects"]["downloads"]["events"]
                .as_array()
                .is_some_and(|events| events.is_empty())
        );
    }
}
