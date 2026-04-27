use super::pending_requests::{
    PendingRequest, PendingRequestRegistry, TerminalRequestIdentity, merge_pending_request,
    new_shared_pending_request_registry, normalize_status, pending_request_from_terminal,
    prune_stale_pending_request_registries, terminal_correlation_lookup_headers,
    terminal_correlation_lookup_url,
};
use super::{
    ObservatoryCallbacks, PENDING_REQUEST_RETENTION_LIMIT, RequestCorrelation, console_message,
    exception_message, needs_loading_failed_listener, needs_loading_finished_listener,
    remote_object_summary, terminal_failure_method,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EventExceptionThrown, ExceptionDetails, RemoteObject, RemoteObjectType, Timestamp,
};
use rub_core::model::{NetworkRequestLifecycle, NetworkRuleEffect, NetworkRuleEffectKind};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

fn sample_pending_request(request_id: &str) -> PendingRequest {
    PendingRequest {
        request_id: request_id.to_string(),
        lifecycle: NetworkRequestLifecycle::Pending,
        url: format!("https://example.com/{request_id}"),
        method: "GET".to_string(),
        tab_target_id: Some("tab-1".to_string()),
        frame_id: Some("frame-1".to_string()),
        resource_type: Some("xhr".to_string()),
        request_headers: BTreeMap::new(),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        mime_type: None,
    }
}

#[test]
fn remote_object_summary_prefers_value_then_description() {
    let string_object = RemoteObject {
        r#type: RemoteObjectType::String,
        subtype: None,
        class_name: None,
        value: Some(serde_json::Value::String("boom".to_string())),
        unserializable_value: None,
        description: Some("ignored".to_string()),
        deep_serialized_value: None,
        object_id: None,
        preview: None,
        custom_preview: None,
    };
    assert_eq!(remote_object_summary(&string_object), "boom");
    assert_eq!(console_message(&[string_object]), "boom");
}

#[test]
fn exception_message_prefers_exception_description() {
    let event = EventExceptionThrown {
        timestamp: Timestamp::new(0.0),
        exception_details: ExceptionDetails {
            exception_id: 1,
            text: "fallback".to_string(),
            line_number: 0,
            column_number: 0,
            script_id: None,
            url: Some("https://example.com/app.js".to_string()),
            stack_trace: None,
            exception: Some(RemoteObject {
                r#type: RemoteObjectType::Object,
                subtype: None,
                class_name: Some("Error".to_string()),
                value: None,
                unserializable_value: None,
                description: Some("Error: boom".to_string()),
                deep_serialized_value: None,
                object_id: None,
                preview: None,
                custom_preview: None,
            }),
            execution_context_id: None,
            exception_meta_data: None,
        },
    };

    assert_eq!(exception_message(&event), "Error: boom");
}

#[test]
fn normalize_status_rejects_invalid_values() {
    assert_eq!(normalize_status(200), Some(200));
    assert_eq!(normalize_status(-1), None);
    assert_eq!(normalize_status(i64::MAX), None);
}

#[test]
fn late_request_merge_does_not_downgrade_responded_pending() {
    let mut responded = PendingRequest {
        request_id: "req-1".to_string(),
        lifecycle: NetworkRequestLifecycle::Responded,
        url: "https://example.com/final".to_string(),
        method: "GET".to_string(),
        tab_target_id: Some("tab-1".to_string()),
        frame_id: None,
        resource_type: Some("xhr".to_string()),
        request_headers: BTreeMap::from([("x-test".to_string(), "1".to_string())]),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: Some(200),
        original_url: Some("https://example.com/original".to_string()),
        rewritten_url: Some("https://example.com/final".to_string()),
        applied_rule_effects: vec![NetworkRuleEffect {
            rule_id: 1,
            kind: NetworkRuleEffectKind::Rewrite,
        }],
        error_text: None,
        mime_type: Some("application/json".to_string()),
    };

    let late_pending = PendingRequest {
        request_id: "req-1".to_string(),
        lifecycle: NetworkRequestLifecycle::Pending,
        url: "https://example.com/original".to_string(),
        method: "POST".to_string(),
        tab_target_id: Some("tab-1".to_string()),
        frame_id: Some("main".to_string()),
        resource_type: Some("xhr".to_string()),
        request_headers: BTreeMap::from([(
            "content-type".to_string(),
            "application/json".to_string(),
        )]),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        mime_type: None,
    };

    merge_pending_request(&mut responded, late_pending);

    assert_eq!(responded.lifecycle, NetworkRequestLifecycle::Responded);
    assert_eq!(responded.method, "GET");
    assert_eq!(
        responded.request_headers["content-type"],
        "application/json"
    );
    assert_eq!(
        responded.original_url.as_deref(),
        Some("https://example.com/original")
    );
    assert_eq!(
        responded.rewritten_url.as_deref(),
        Some("https://example.com/final")
    );
}

#[test]
fn terminal_fallback_uses_correlation_when_pending_request_is_missing() {
    let correlation = RequestCorrelation {
        tab_target_id: Some("tab-9".to_string()),
        original_url: "https://example.com/image".to_string(),
        rewritten_url: Some("https://cdn.example.com/image.webp".to_string()),
        effective_request_headers: None,
        applied_rule_effects: vec![NetworkRuleEffect {
            rule_id: 9,
            kind: NetworkRuleEffectKind::Rewrite,
        }],
    };

    let pending = pending_request_from_terminal(
        "req-9",
        NetworkRequestLifecycle::Completed,
        None,
        Some(&correlation),
        "tab-9",
    );

    assert_eq!(pending.lifecycle, NetworkRequestLifecycle::Completed);
    assert_eq!(pending.url, "https://cdn.example.com/image.webp");
    assert!(pending.method.is_empty());
    assert_eq!(
        pending.original_url.as_deref(),
        Some("https://example.com/image")
    );
}

#[test]
fn terminal_correlation_merges_into_existing_pending_request() {
    let mut pending = PendingRequest {
        request_id: "req-10".to_string(),
        lifecycle: NetworkRequestLifecycle::Responded,
        url: "https://example.com/image.webp".to_string(),
        method: "GET".to_string(),
        tab_target_id: Some("tab-10".to_string()),
        frame_id: Some("frame-10".to_string()),
        resource_type: Some("image".to_string()),
        request_headers: BTreeMap::new(),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: Some(200),
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        mime_type: Some("image/webp".to_string()),
    };
    let correlation = RequestCorrelation {
        tab_target_id: Some("tab-10".to_string()),
        original_url: "https://example.com/image".to_string(),
        rewritten_url: Some("https://cdn.example.com/image.webp".to_string()),
        effective_request_headers: None,
        applied_rule_effects: vec![NetworkRuleEffect {
            rule_id: 11,
            kind: NetworkRuleEffectKind::Rewrite,
        }],
    };

    super::apply_terminal_correlation(
        &mut pending,
        "req-10",
        NetworkRequestLifecycle::Completed,
        Some(&correlation),
        "tab-10",
    );

    assert_eq!(
        pending.original_url.as_deref(),
        Some("https://example.com/image")
    );
    assert_eq!(
        pending.rewritten_url.as_deref(),
        Some("https://cdn.example.com/image.webp")
    );
    assert_eq!(pending.applied_rule_effects.len(), 1);
}

#[test]
fn terminal_fallback_uses_stored_request_identity_when_pending_is_missing() {
    let identity = TerminalRequestIdentity {
        url: "https://example.com/image.webp".to_string(),
        method: "GET".to_string(),
        tab_target_id: Some("tab-11".to_string()),
        frame_id: Some("frame-11".to_string()),
        resource_type: Some("image".to_string()),
        request_headers: BTreeMap::from([("accept".to_string(), "image/webp".to_string())]),
    };

    let pending = pending_request_from_terminal(
        "req-11",
        NetworkRequestLifecycle::Failed,
        Some(&identity),
        None,
        "tab-11",
    );

    assert_eq!(pending.url, "https://example.com/image.webp");
    assert_eq!(pending.method, "GET");
    assert_eq!(pending.frame_id.as_deref(), Some("frame-11"));
    assert_eq!(
        pending.request_headers.get("accept").map(String::as_str),
        Some("image/webp")
    );
}

#[test]
fn terminal_correlation_lookup_uses_stored_request_identity_when_pending_is_missing() {
    let identity = TerminalRequestIdentity {
        url: "https://cdn.example.com/image.webp".to_string(),
        method: "GET".to_string(),
        tab_target_id: Some("tab-12".to_string()),
        frame_id: Some("frame-12".to_string()),
        resource_type: Some("image".to_string()),
        request_headers: BTreeMap::from([("accept".to_string(), "image/webp".to_string())]),
    };

    assert_eq!(
        terminal_correlation_lookup_url(None, Some(&identity)),
        "https://cdn.example.com/image.webp"
    );
    assert_eq!(
        terminal_correlation_lookup_headers(None, Some(&identity))
            .and_then(|headers| headers.get("accept"))
            .map(String::as_str),
        Some("image/webp")
    );
}

#[test]
fn late_request_merge_populates_unknown_method_without_fabricating_get() {
    let mut responded = PendingRequest {
        request_id: "req-2".to_string(),
        lifecycle: NetworkRequestLifecycle::Responded,
        url: "https://example.com/upload".to_string(),
        method: String::new(),
        tab_target_id: Some("tab-2".to_string()),
        frame_id: None,
        resource_type: Some("fetch".to_string()),
        request_headers: BTreeMap::new(),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: Some(201),
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        mime_type: Some("application/json".to_string()),
    };

    let late_pending = PendingRequest {
        request_id: "req-2".to_string(),
        lifecycle: NetworkRequestLifecycle::Pending,
        url: "https://example.com/upload".to_string(),
        method: "POST".to_string(),
        tab_target_id: Some("tab-2".to_string()),
        frame_id: Some("frame-1".to_string()),
        resource_type: Some("fetch".to_string()),
        request_headers: BTreeMap::new(),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: None,
        status: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        mime_type: None,
    };

    merge_pending_request(&mut responded, late_pending);

    assert_eq!(responded.lifecycle, NetworkRequestLifecycle::Responded);
    assert_eq!(responded.method, "POST");
}

#[test]
fn pending_request_registry_bounds_pending_and_terminal_tracking() {
    let mut registry = PendingRequestRegistry::default();
    let mut saw_eviction = false;
    for index in 0..(PENDING_REQUEST_RETENTION_LIMIT + 16) {
        let request_id = format!("req-{index}");
        let pending = sample_pending_request(&request_id);
        let upsert = registry.record_request(&request_id, pending);
        saw_eviction |= upsert.evicted;
    }

    assert!(
        saw_eviction,
        "bounded registry should report when it evicts old authority"
    );
    assert_eq!(registry.pending_len(), PENDING_REQUEST_RETENTION_LIMIT);
    assert_eq!(
        registry.terminal_identity_len(),
        PENDING_REQUEST_RETENTION_LIMIT
    );
    assert!(registry.remove("req-0").0.is_none());
    assert!(registry.remove("req-0").1.is_none());
    assert!(registry.remove("req-16").0.is_some());
}

#[test]
fn record_request_updates_terminal_identity_from_merged_pending_state() {
    let mut registry = PendingRequestRegistry::default();
    let request_id = "req-merged";
    registry.record_request(request_id, sample_pending_request(request_id));

    let mut responded = sample_pending_request(request_id);
    responded.lifecycle = NetworkRequestLifecycle::Responded;
    responded.url = "https://example.com/final".to_string();
    responded.method = String::new();
    responded.request_headers = BTreeMap::new();
    registry.record_request(request_id, responded);

    let terminal = registry
        .take_terminal_state(request_id)
        .terminal_identity
        .expect("terminal identity should be retained");
    assert_eq!(terminal.url, "https://example.com/final");
    assert_eq!(terminal.method, "GET");
}

#[test]
fn response_correlation_can_peek_request_method_from_pending_identity() {
    let mut registry = PendingRequestRegistry::default();
    let request_id = "req-response";
    let mut pending = sample_pending_request(request_id);
    pending.method = "POST".to_string();
    pending.request_headers = BTreeMap::from([("x-rub-test".to_string(), "1".to_string())]);
    registry.record_request(request_id, pending);

    let terminal = registry
        .peek_terminal_identity(request_id)
        .expect("responseReceived should be able to reuse requestWillBeSent identity");

    assert_eq!(terminal.method, "POST");
    assert_eq!(
        terminal
            .request_headers
            .get("x-rub-test")
            .map(String::as_str),
        Some("1")
    );
}

#[test]
fn degraded_only_callbacks_are_not_empty() {
    let callbacks = ObservatoryCallbacks {
        on_runtime_degraded: Some(Arc::new(|_| {})),
        ..Default::default()
    };

    assert!(
        !callbacks.is_empty(),
        "degraded-only observatory installs must still keep listeners alive"
    );
}

#[test]
fn summary_only_observatory_does_not_install_terminal_body_listeners() {
    let callbacks = ObservatoryCallbacks {
        on_request_summary: Some(Arc::new(|_| {})),
        ..Default::default()
    };

    assert!(
        !needs_loading_finished_listener(&callbacks),
        "summary-only observatory must not fetch response bodies with no record consumer"
    );
    assert!(
        !needs_loading_failed_listener(&callbacks),
        "summary-only observatory must not consume terminal failure authority without a consumer"
    );
}

#[test]
fn request_record_observatory_installs_terminal_listeners() {
    let callbacks = ObservatoryCallbacks {
        on_request_record: Some(Arc::new(|_| {})),
        ..Default::default()
    };

    assert!(needs_loading_finished_listener(&callbacks));
    assert!(needs_loading_failed_listener(&callbacks));
}

#[test]
fn request_correlation_degraded_reasons_are_forwarded_to_runtime_callback() {
    let observed = Arc::new(StdMutex::new(Vec::new()));
    let sink = observed.clone();
    let callback = Arc::new(move |reason: String| {
        sink.lock().expect("degraded sink lock").push(reason);
    });

    super::notify_request_correlation_degraded(
        vec![
            "request_correlation_registry_evicted",
            "request_correlation_unresolved_fallback",
        ],
        &Some(callback),
    );

    assert_eq!(
        observed.lock().expect("degraded sink lock").as_slice(),
        [
            "request_correlation_registry_evicted",
            "request_correlation_unresolved_fallback",
        ]
    );
}

#[test]
fn terminal_failure_method_reuses_fallback_method_when_pending_is_missing() {
    let fallback = PendingRequest {
        method: "POST".to_string(),
        ..sample_pending_request("req-fallback")
    };

    assert_eq!(terminal_failure_method(None, &fallback), "POST");
    assert_eq!(
        terminal_failure_method(Some(&sample_pending_request("req-live")), &fallback),
        "GET"
    );
}

#[tokio::test]
async fn prune_stale_pending_request_registries_drops_missing_targets() {
    let registries = Arc::new(Mutex::new(HashMap::from([
        (
            "tab-live".to_string(),
            new_shared_pending_request_registry(),
        ),
        (
            "tab-stale".to_string(),
            new_shared_pending_request_registry(),
        ),
    ])));

    prune_stale_pending_request_registries(&registries, &HashSet::from(["tab-live".to_string()]))
        .await;

    let guard = registries.lock().await;
    assert!(guard.contains_key("tab-live"));
    assert!(!guard.contains_key("tab-stale"));
}
