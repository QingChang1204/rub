use rub_core::locator::CanonicalLocator;

pub(super) fn read_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

pub(super) fn page_subject(frame_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "page",
        "frame_id": frame_id,
    })
}

pub(super) fn live_read_subject(
    read_kind: &str,
    locator: &impl IntoCanonicalLocatorRef,
    frame_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "live_read",
        "read_kind": read_kind,
        "frame_id": frame_id,
        "locator": crate::router::request_args::canonical_locator_json(locator.as_canonical_locator()),
    })
}

pub(super) fn scalar_read_result(kind: &str, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "value": value,
    })
}

pub(super) fn multi_read_result(kind: &str, items: serde_json::Value) -> serde_json::Value {
    let item_count = items.as_array().map(|value| value.len()).unwrap_or(0);
    serde_json::json!({
        "kind": kind,
        "items": items,
        "item_count": item_count,
    })
}

pub(super) trait IntoCanonicalLocatorRef {
    fn as_canonical_locator(&self) -> &CanonicalLocator;
}

impl IntoCanonicalLocatorRef for CanonicalLocator {
    fn as_canonical_locator(&self) -> &CanonicalLocator {
        self
    }
}

impl IntoCanonicalLocatorRef for rub_core::locator::LiveLocator {
    fn as_canonical_locator(&self) -> &CanonicalLocator {
        self.as_ref()
    }
}
