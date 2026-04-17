use super::{attach_download_events, attach_observed_effects};
use rub_core::model::{DownloadEntry, DownloadEvent, DownloadEventKind, DownloadState};

const DUPLICATED_INTERACTION_FIELDS: &[&str] = &[
    "context_turnover",
    "frame_context_status",
    "frame_context",
    "frame_lineage",
    "runtime_state_delta",
    "interference",
    "runtime_observatory_events",
    "network_requests",
    "downloads",
];

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
fn download_surface_truth_labels_remain_on_interaction_surface() {
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
    assert!(
        data["interaction"]["observed_effects"]["downloads"].is_null(),
        "{data}"
    );
}

#[test]
fn non_authoritative_empty_download_window_still_projects_degraded_interaction_surface() {
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
    assert!(
        data["interaction"]["observed_effects"]["downloads"].is_null(),
        "{data}"
    );
}

#[test]
fn observed_effects_do_not_recopy_interaction_projection_fields() {
    let mut data = serde_json::json!({
        "interaction": {
            "context_turnover": {
                "context_changed": true,
                "context_replaced": false,
                "before_page": { "url": "https://before.test" },
                "after_page": { "url": "https://after.test" }
            },
            "frame_context_status": "current",
            "frame_context": {
                "frame_id": "frame-main",
                "url": "https://after.test"
            },
            "frame_lineage": [
                { "frame_id": "frame-main" }
            ],
            "runtime_state_delta": {
                "title_changed": true
            },
            "interference": {
                "status_changed": true
            },
            "runtime_observatory_events": [
                { "sequence": 7, "payload": { "kind": "console_error" } }
            ],
            "network_requests": {
                "requests": [
                    { "request_id": "req-1" }
                ],
                "terminal_count": 1,
                "last_request": { "request_id": "req-1" },
                "authoritative": true,
                "degraded_reason": null
            },
            "downloads": {
                "events": [
                    { "sequence": 3 }
                ],
                "last_download": { "guid": "dl-1" },
                "authoritative": true,
                "degraded_reason": null
            }
        }
    });

    attach_observed_effects(&mut data);

    let interaction = &data["interaction"];
    let observed = &interaction["observed_effects"];
    for key in DUPLICATED_INTERACTION_FIELDS {
        assert!(
            observed[*key].is_null(),
            "expected observed_effects.{key} to be omitted: {data}"
        );
    }
}
