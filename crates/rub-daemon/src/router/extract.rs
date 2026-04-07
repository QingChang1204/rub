use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::time::{Duration, sleep};

mod collection;

use super::addressing::resolve_elements_against_snapshot;
use super::extract_postprocess::{
    ExtractTransform, ExtractValueType, apply_postprocess, resolve_missing_field,
};
use super::projection::snapshot_entity;
use super::request_args::{LocatorRequestArgs, locator_json, parse_json_args};
use super::secret_resolution::redact_json_value;
use super::snapshot::build_stable_snapshot;
use super::*;
use collection::{ExtractEntrySpec, extract_collection};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};

const DEFAULT_SCAN_MAX_SCROLLS: u32 = 100;
const DEFAULT_SCAN_SCROLL_AMOUNT: u32 = 1_800;
const DEFAULT_SCAN_SETTLE_MS: u64 = 1_200;
const DEFAULT_SCAN_STALL_LIMIT: u32 = 3;

#[derive(Debug)]
struct ExtractFieldSpec {
    index: Option<u32>,
    element_ref: Option<String>,
    selector: Option<String>,
    target_text: Option<String>,
    role: Option<String>,
    label: Option<String>,
    testid: Option<String>,
    first: bool,
    last: bool,
    nth: Option<u32>,
    kind: ExtractKind,
    attribute: Option<String>,
    many: bool,
    value_type: Option<ExtractValueType>,
    required: bool,
    default: Option<serde_json::Value>,
    map: BTreeMap<String, serde_json::Value>,
    transform: Option<ExtractTransform>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtractFieldSpec {
    index: Option<u32>,
    #[serde(rename = "ref")]
    element_ref: Option<String>,
    selector: Option<String>,
    target_text: Option<String>,
    role: Option<String>,
    label: Option<String>,
    testid: Option<String>,
    #[serde(default)]
    first: bool,
    #[serde(default)]
    last: bool,
    nth: Option<u32>,
    kind: Option<ExtractKind>,
    #[serde(rename = "type")]
    type_hint: Option<ExtractTypeHint>,
    #[serde(alias = "attr")]
    attribute: Option<String>,
    #[serde(default)]
    many: bool,
    #[serde(default = "default_extract_required")]
    required: bool,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    map: BTreeMap<String, serde_json::Value>,
    transform: Option<ExtractTransform>,
}

#[derive(Debug, Clone, Copy)]
enum ExtractTypeHint {
    Kind(ExtractKind),
    ValueType(ExtractValueType),
}

impl<'de> Deserialize<'de> for ExtractTypeHint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if let Ok(kind) = serde_json::from_value::<ExtractKind>(serde_json::json!(raw)) {
            return Ok(Self::Kind(kind));
        }
        if let Ok(value_type) = serde_json::from_value::<ExtractValueType>(serde_json::json!(raw)) {
            return Ok(Self::ValueType(value_type));
        }
        Err(serde::de::Error::custom(format!(
            "unknown extract type '{raw}'; use kind one of [text,value,html,bbox,attributes,attribute] or value type one of [string,number,boolean,array,object]"
        )))
    }
}

impl<'de> Deserialize<'de> for ExtractFieldSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawExtractFieldSpec::deserialize(deserializer)?;
        let (kind, value_type) = match (raw.kind, raw.type_hint) {
            (Some(kind), Some(ExtractTypeHint::ValueType(value_type))) => (kind, Some(value_type)),
            (Some(kind), Some(ExtractTypeHint::Kind(type_kind))) if kind == type_kind => {
                (kind, None)
            }
            (Some(kind), Some(ExtractTypeHint::Kind(type_kind))) => {
                return Err(serde::de::Error::custom(format!(
                    "extract field has conflicting kind '{}' and type shorthand '{}'",
                    kind.as_str(),
                    type_kind.as_str()
                )));
            }
            (Some(kind), None) => (kind, None),
            (None, Some(ExtractTypeHint::Kind(kind))) => (kind, None),
            (None, Some(ExtractTypeHint::ValueType(value_type))) => {
                return Err(serde::de::Error::custom(format!(
                    "extract field is missing 'kind'; use kind:'text'/'value'/... and reserve type:'{}' for output validation",
                    value_type.as_str()
                )));
            }
            (None, None) => {
                // Infer kind from context: attribute field present → Attribute, else → Text
                let inferred = if raw.attribute.is_some() {
                    ExtractKind::Attribute
                } else {
                    ExtractKind::Text
                };
                (inferred, None)
            }
        };

        Ok(Self {
            index: raw.index,
            element_ref: raw.element_ref,
            selector: raw.selector,
            target_text: raw.target_text,
            role: raw.role,
            label: raw.label,
            testid: raw.testid,
            first: raw.first,
            last: raw.last,
            nth: raw.nth,
            kind,
            attribute: raw.attribute,
            many: raw.many,
            value_type,
            required: raw.required,
            default: raw.default,
            map: raw.map,
            transform: raw.transform,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExtractKind {
    Text,
    Value,
    Html,
    Bbox,
    Attributes,
    Attribute,
}

#[derive(Debug, serde::Deserialize)]
struct ContentExtractPayload {
    match_count: usize,
    selected_count: usize,
    values: Vec<serde_json::Value>,
    selector_error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractQueryArgs {
    spec: String,
    #[serde(default, rename = "spec_source")]
    _spec_source: Option<serde_json::Value>,
    #[serde(default)]
    snapshot_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractListArgs {
    spec: String,
    #[serde(default, rename = "spec_source")]
    _spec_source: Option<serde_json::Value>,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(flatten)]
    scan: ExtractScanArgs,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExtractScanArgs {
    scan_until: Option<u64>,
    scan_key: Option<String>,
    max_scrolls: Option<u64>,
    scroll_amount: Option<u64>,
    settle_ms: Option<u64>,
    stall_limit: Option<u64>,
}

impl ExtractScanArgs {
    fn parse_config(&self) -> Result<Option<ExtractScanConfig>, RubError> {
        let Some(until_count) = self.scan_until else {
            return Ok(None);
        };
        if until_count == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --scan-until must be greater than 0",
            ));
        }

        let max_scrolls = self
            .max_scrolls
            .unwrap_or(u64::from(DEFAULT_SCAN_MAX_SCROLLS));
        if max_scrolls == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --max-scrolls must be greater than 0",
            ));
        }

        let scroll_amount = self
            .scroll_amount
            .unwrap_or(u64::from(DEFAULT_SCAN_SCROLL_AMOUNT));
        if scroll_amount == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --scroll-amount must be greater than 0",
            ));
        }

        let settle_ms = self.settle_ms.unwrap_or(DEFAULT_SCAN_SETTLE_MS);
        let stall_limit = self
            .stall_limit
            .unwrap_or(u64::from(DEFAULT_SCAN_STALL_LIMIT));
        if stall_limit == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --stall-limit must be greater than 0",
            ));
        }

        let dedupe_key = self
            .scan_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        Ok(Some(ExtractScanConfig {
            until_count: until_count.min(u64::from(u32::MAX)) as u32,
            dedupe_key,
            max_scrolls: max_scrolls.min(u64::from(u32::MAX)) as u32,
            scroll_amount: scroll_amount.min(u64::from(u32::MAX)) as u32,
            settle_ms,
            stall_limit: stall_limit.min(u64::from(u32::MAX)) as u32,
        }))
    }
}

#[derive(Debug)]
enum ExtractCommand {
    Query(ExtractQueryArgs),
    List(ExtractListArgs),
}

impl ExtractCommand {
    fn parse(args: &serde_json::Value, sub_override: Option<&str>) -> Result<Self, RubError> {
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            if let Some(sub) = sub_override {
                // Caller (inspect dispatch) provides the sub explicitly after stripping
                // the routing key; use it directly so the right variant is selected.
                object.insert("sub".to_string(), serde_json::json!(sub));
            } else {
                // Top-level use: sub may already be in args (CLI-provided), or defaults
                // to "extract" when the extract command is invoked without a sub-command.
                object
                    .entry("sub".to_string())
                    .or_insert_with(|| serde_json::json!("extract"));
            }
        }

        #[derive(Debug, serde::Deserialize)]
        #[serde(tag = "sub", rename_all = "snake_case")]
        enum TaggedExtractCommand {
            Extract(ExtractQueryArgs),
            List(ExtractListArgs),
        }

        match parse_json_args::<TaggedExtractCommand>(&normalized, "extract")? {
            TaggedExtractCommand::Extract(args) => Ok(Self::Query(args)),
            TaggedExtractCommand::List(args) => Ok(Self::List(args)),
        }
    }

    fn spec(&self) -> &str {
        match self {
            Self::Query(args) => &args.spec,
            Self::List(args) => &args.spec,
        }
    }

    fn snapshot_id(&self) -> Option<&str> {
        match self {
            Self::Query(args) => args.snapshot_id.as_deref(),
            Self::List(args) => args.snapshot_id.as_deref(),
        }
    }

    fn scan_config(&self) -> Result<Option<ExtractScanConfig>, RubError> {
        match self {
            Self::Query(_) => Ok(None),
            Self::List(args) => args.scan.parse_config(),
        }
    }

    fn source_kind(&self) -> &'static str {
        if self.snapshot_id().is_some() {
            "snapshot"
        } else {
            "live_page"
        }
    }
}

pub(super) async fn cmd_extract(
    router: &DaemonRouter,
    args: &serde_json::Value,
    sub_override: Option<&str>,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed_args = ExtractCommand::parse(args, sub_override)?;
    let parsed = parse_extract_fields(parsed_args.spec(), &state.rub_home)?;
    let fields = parsed.value;
    let metadata = parsed.metadata;
    let scan = parsed_args.scan_config()?;
    let is_inspect_list = matches!(&parsed_args, ExtractCommand::List(_));
    let source_kind = parsed_args.source_kind();

    if scan.is_some() && parsed_args.snapshot_id().is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list scan cannot reuse --snapshot; scanning requires live snapshots across scroll steps",
        ));
    }

    if let Some(scan) = scan {
        let (collection_name, collection) =
            resolve_single_collection(&fields, "inspect list scan")?;
        let outcome =
            scan_collection(router, args, state, collection_name, collection, &scan).await?;
        let mut extracted = serde_json::Map::new();
        extracted.insert(
            collection_name.to_string(),
            serde_json::Value::Array(outcome.rows),
        );
        let mut data = if is_inspect_list {
            extract_payload(
                serde_json::json!({
                    "kind": "collection_extract",
                    "source": "live_page",
                    "collection": collection_name,
                    "scan_requested": true,
                }),
                serde_json::json!({
                    "items": extracted
                        .get(collection_name)
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                    "item_count": outcome.returned_count,
                    "scan": {
                        "complete": outcome.complete,
                        "stop_reason": outcome.stop_reason,
                        "returned_count": outcome.returned_count,
                        "unique_count": outcome.unique_count,
                        "target_count": scan.until_count,
                        "pass_count": outcome.pass_count,
                        "scroll_count": outcome.scroll_count,
                        "scan_key": scan.dedupe_key,
                    },
                }),
            )
        } else {
            extract_payload(
                serde_json::json!({
                    "kind": "extract_query",
                    "source": "live_page",
                }),
                serde_json::json!({
                    "fields": extracted,
                    "field_count": 1,
                    "scan": {
                        "complete": outcome.complete,
                        "stop_reason": outcome.stop_reason,
                        "returned_count": outcome.returned_count,
                        "unique_count": outcome.unique_count,
                        "target_count": scan.until_count,
                        "pass_count": outcome.pass_count,
                        "scroll_count": outcome.scroll_count,
                        "scan_key": scan.dedupe_key,
                    },
                }),
            )
        };
        redact_json_value(&mut data, &metadata);
        return Ok(data);
    }

    let snapshot = if let Some(snapshot_id) = parsed_args.snapshot_id() {
        state.get_snapshot(snapshot_id).await.ok_or_else(|| {
            RubError::domain(
                ErrorCode::StaleSnapshot,
                format!("Snapshot '{snapshot_id}' not found in cache"),
            )
        })?
    } else {
        let snapshot = build_stable_snapshot(router, args, state, None, false, false).await?;
        state.cache_snapshot(snapshot).await
    };

    let mut data = if is_inspect_list {
        let (collection_name, collection) = resolve_single_collection(&fields, "inspect list")?;
        let items = extract_collection(router, &snapshot, collection_name, collection).await?;
        let item_count = items.as_array().map(|value| value.len()).unwrap_or(0);
        extract_payload(
            serde_json::json!({
                "kind": "collection_extract",
                "source": source_kind,
                "collection": collection_name,
                "scan_requested": false,
            }),
            serde_json::json!({
                "snapshot": snapshot_entity(&snapshot),
                "items": items,
                "item_count": item_count,
            }),
        )
    } else {
        let mut extracted = serde_json::Map::new();
        for (name, entry) in fields {
            let value = match entry {
                ExtractEntrySpec::Field(field) => {
                    match extract_field(router, &snapshot, &name, &field).await {
                        Ok(value) => apply_field_postprocess(&name, &field, value)?,
                        Err(error) if should_substitute_missing_field(&field, &error) => {
                            resolve_missing_field(&name, field.required, field.default.as_ref())?
                        }
                        Err(error) => return Err(error),
                    }
                }
                ExtractEntrySpec::Collection(collection) => {
                    extract_collection(router, &snapshot, &name, &collection).await?
                }
            };
            extracted.insert(name, value);
        }
        let field_count = extracted.len();
        extract_payload(
            serde_json::json!({
                "kind": "extract_query",
                "source": source_kind,
            }),
            serde_json::json!({
                "snapshot": snapshot_entity(&snapshot),
                "fields": extracted,
                "field_count": field_count,
            }),
        )
    };
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

fn default_extract_required() -> bool {
    true
}

fn extract_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

#[derive(Clone, Copy)]
enum ExtractMatchSurface<'a> {
    InteractiveField,
    ContentField {
        selector: &'a str,
    },
    CollectionRow {
        collection_name: &'a str,
        row_index: usize,
    },
}

#[derive(Debug, Clone)]
struct ExtractScanConfig {
    until_count: u32,
    dedupe_key: Option<String>,
    max_scrolls: u32,
    scroll_amount: u32,
    settle_ms: u64,
    stall_limit: u32,
}

#[derive(Debug)]
struct ExtractScanOutcome {
    rows: Vec<serde_json::Value>,
    returned_count: usize,
    unique_count: usize,
    pass_count: u32,
    scroll_count: u32,
    complete: bool,
    stop_reason: &'static str,
}

async fn extract_field(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    field_name: &str,
    field: &ExtractFieldSpec,
) -> Result<serde_json::Value, RubError> {
    let locator_args = locator_json(LocatorRequestArgs {
        index: field.index,
        element_ref: field.element_ref.clone(),
        selector: field.selector.clone(),
        target_text: field.target_text.clone(),
        role: field.role.clone(),
        label: field.label.clone(),
        testid: field.testid.clone(),
        first: field.first,
        last: field.last,
        nth: field.nth,
    });

    match resolve_elements_against_snapshot(router, snapshot, &locator_args, "extract").await {
        Ok(resolved) => {
            extract_field_value(
                field_name,
                router,
                &resolved.elements,
                field,
                ExtractMatchSurface::InteractiveField,
            )
            .await
        }
        Err(error) if should_fallback_to_content(field, &error) => {
            let selector = field.selector.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::ElementNotFound,
                    "extract field did not resolve to any content element",
                )
            })?;
            extract_content_field_value(router, snapshot, field_name, selector, field).await
        }
        Err(error) => Err(error),
    }
}

fn should_fallback_to_content(field: &ExtractFieldSpec, error: &RubError) -> bool {
    field.selector.is_some()
        && matches!(
            error,
            RubError::Domain(ErrorEnvelope {
                code: ErrorCode::ElementNotFound,
                ..
            })
        )
}

fn should_substitute_missing_field(field: &ExtractFieldSpec, error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(ErrorEnvelope {
            code: ErrorCode::ElementNotFound,
            ..
        })
    ) && (!field.required || field.default.is_some())
}

fn apply_field_postprocess(
    name: &str,
    field: &ExtractFieldSpec,
    value: serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    apply_postprocess(
        name,
        value,
        field.value_type,
        field.default.as_ref(),
        &field.map,
        field.transform,
    )
}

async fn extract_field_value(
    field_name: &str,
    router: &DaemonRouter,
    elements: &[rub_core::model::Element],
    field: &ExtractFieldSpec,
    surface: ExtractMatchSurface<'_>,
) -> Result<serde_json::Value, RubError> {
    if !field.many && elements.len() > 1 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            extract_multi_match_message(field_name, elements.len(), surface),
            extract_multi_match_context(field_name, field, elements.len(), surface),
            extract_multi_match_suggestion(surface),
        ));
    }

    if field.many {
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(extract_single_value(router, element, field).await?);
        }
        return Ok(serde_json::Value::Array(values));
    }

    let element = elements.first().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any interactive snapshot element",
        )
    })?;
    extract_single_value(router, element, field).await
}

async fn extract_single_value(
    router: &DaemonRouter,
    element: &rub_core::model::Element,
    field: &ExtractFieldSpec,
) -> Result<serde_json::Value, RubError> {
    match field.kind {
        ExtractKind::Text => Ok(serde_json::json!(router.browser.get_text(element).await?)),
        ExtractKind::Value => Ok(serde_json::json!(router.browser.get_value(element).await?)),
        ExtractKind::Html => Ok(serde_json::json!(
            router.browser.get_outer_html(element).await?
        )),
        ExtractKind::Bbox => {
            serde_json::to_value(router.browser.get_bbox(element).await?).map_err(RubError::from)
        }
        ExtractKind::Attributes => {
            serde_json::to_value(router.browser.get_attributes(element).await?)
                .map_err(RubError::from)
        }
        ExtractKind::Attribute => {
            let attribute_name = field.attribute.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "extract field kind 'attribute' requires an 'attribute' name",
                )
            })?;
            let attributes = router.browser.get_attributes(element).await?;
            Ok(match attributes.get(attribute_name) {
                Some(value) => serde_json::json!(value),
                None => serde_json::Value::Null,
            })
        }
    }
}

async fn extract_content_field_value(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    field_name: &str,
    selector: &str,
    field: &ExtractFieldSpec,
) -> Result<serde_json::Value, RubError> {
    let script = build_content_extract_script(selector, field)?;
    let payload: ContentExtractPayload =
        execute_json_payload_in_frame(router, snapshot, &script, "content").await?;

    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid selector for extract content path: {selector_error}"),
            serde_json::json!({
                "selector": selector,
                "kind": field.kind.as_str(),
            }),
        ));
    }

    if payload.selected_count == 0 {
        return Err(RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any content element",
            serde_json::json!({
                "selector": selector,
                "kind": field.kind.as_str(),
                "match_count": payload.match_count,
            }),
        ));
    }

    if !field.many && payload.match_count > 1 && !field_has_selection(field) {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            extract_multi_match_message(
                field_name,
                payload.match_count,
                ExtractMatchSurface::ContentField { selector },
            ),
            extract_multi_match_context(
                field_name,
                field,
                payload.match_count,
                ExtractMatchSurface::ContentField { selector },
            ),
            extract_multi_match_suggestion(ExtractMatchSurface::ContentField { selector }),
        ));
    }

    if field.many {
        return Ok(serde_json::Value::Array(payload.values));
    }

    payload.values.into_iter().next().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any content element",
        )
    })
}

fn extract_multi_match_message(
    field_name: &str,
    match_count: usize,
    surface: ExtractMatchSurface<'_>,
) -> String {
    match surface {
        ExtractMatchSurface::InteractiveField => format!(
            "extract field '{field_name}' matched {match_count} elements; add first/last/nth, set 'many: true', or narrow the locator"
        ),
        ExtractMatchSurface::ContentField { .. } => format!(
            "extract field '{field_name}' matched {match_count} content elements; add first/last/nth, set 'many: true', or narrow the selector"
        ),
        ExtractMatchSurface::CollectionRow {
            collection_name,
            row_index,
        } => format!(
            "collection field '{field_name}' matched {match_count} elements in row {row_index} of '{collection_name}'; add first/last/nth, set 'many: true', or narrow the row-scoped locator"
        ),
    }
}

fn extract_multi_match_suggestion(surface: ExtractMatchSurface<'_>) -> &'static str {
    match surface {
        ExtractMatchSurface::InteractiveField => {
            "Use `many: true` to collect every match, add `first`, `last`, or `nth` to pick one, or narrow the locator to the specific repeated card/content you want"
        }
        ExtractMatchSurface::ContentField { .. } => {
            "Use `many: true` to collect every content match, add `first`, `last`, or `nth` to pick one, or narrow the selector to the specific repeated content you want"
        }
        ExtractMatchSurface::CollectionRow { .. } => {
            "Use `many: true` to collect every row-local match, add `first`, `last`, or `nth` to pick one, or narrow the row-scoped selector/role/label/testid inside the repeated card or list row"
        }
    }
}

fn extract_multi_match_context(
    field_name: &str,
    field: &ExtractFieldSpec,
    match_count: usize,
    surface: ExtractMatchSurface<'_>,
) -> serde_json::Value {
    let mut context = serde_json::Map::from_iter([
        ("field".to_string(), serde_json::json!(field_name)),
        ("kind".to_string(), serde_json::json!(field.kind.as_str())),
        ("match_count".to_string(), serde_json::json!(match_count)),
        ("locator".to_string(), extract_field_locator_context(field)),
        (
            "resolution_examples".to_string(),
            serde_json::json!({
                "pick_first": { "first": true },
                "pick_last": { "last": true },
                "pick_nth": { "nth": 0 },
                "collect_all": { "many": true }
            }),
        ),
    ]);
    if let Some(builder_examples) = extract_builder_field_examples(field_name, field) {
        context.insert("builder_field_examples".to_string(), builder_examples);
    }

    match surface {
        ExtractMatchSurface::InteractiveField => {
            context.insert("surface".to_string(), serde_json::json!("interactive"));
        }
        ExtractMatchSurface::ContentField { selector } => {
            context.insert("surface".to_string(), serde_json::json!("content"));
            context.insert("selector".to_string(), serde_json::json!(selector));
        }
        ExtractMatchSurface::CollectionRow {
            collection_name,
            row_index,
        } => {
            context.insert("surface".to_string(), serde_json::json!("collection_row"));
            context.insert("collection".to_string(), serde_json::json!(collection_name));
            context.insert("row_index".to_string(), serde_json::json!(row_index));
        }
    }

    serde_json::Value::Object(context)
}

fn extract_builder_field_examples(
    field_name: &str,
    field: &ExtractFieldSpec,
) -> Option<serde_json::Value> {
    let locator = builder_locator_expression(field)?;
    let kind = match field.kind {
        ExtractKind::Text => format!("text:{locator}"),
        ExtractKind::Html => format!("html:{locator}"),
        ExtractKind::Value => format!("value:{locator}"),
        ExtractKind::Attributes => format!("attributes:{locator}"),
        ExtractKind::Bbox => format!("bbox:{locator}"),
        ExtractKind::Attribute => format!("attribute:{}:{locator}", field.attribute.as_deref()?),
    };
    Some(serde_json::json!({
        "pick_first": format!("{field_name}={kind}@first"),
        "pick_last": format!("{field_name}={kind}@last"),
        "pick_nth": format!("{field_name}={kind}@nth(0)"),
        "collect_all": format!("{field_name}={kind}@many"),
    }))
}

fn builder_locator_expression(field: &ExtractFieldSpec) -> Option<String> {
    if let Some(selector) = field.selector.as_deref().map(str::trim)
        && !selector.is_empty()
    {
        return Some(selector.to_string());
    }
    if let Some(target_text) = field.target_text.as_deref().map(str::trim)
        && !target_text.is_empty()
    {
        return Some(format!("target_text:{target_text}"));
    }
    if let Some(role) = field.role.as_deref().map(str::trim)
        && !role.is_empty()
    {
        return Some(format!("role:{role}"));
    }
    if let Some(label) = field.label.as_deref().map(str::trim)
        && !label.is_empty()
    {
        return Some(format!("label:{label}"));
    }
    if let Some(testid) = field.testid.as_deref().map(str::trim)
        && !testid.is_empty()
    {
        return Some(format!("testid:{testid}"));
    }
    None
}

fn extract_field_locator_context(field: &ExtractFieldSpec) -> serde_json::Value {
    serde_json::json!({
        "index": field.index,
        "ref": field.element_ref,
        "selector": field.selector,
        "target_text": field.target_text,
        "role": field.role,
        "label": field.label,
        "testid": field.testid,
        "first": field.first,
        "last": field.last,
        "nth": field.nth,
        "many": field.many,
    })
}

fn build_content_extract_script(
    selector: &str,
    field: &ExtractFieldSpec,
) -> Result<String, RubError> {
    if matches!(field.kind, ExtractKind::Attribute) && field.attribute.is_none() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "extract field kind 'attribute' requires an 'attribute' name",
        ));
    }

    let selector = serde_json::to_string(selector)
        .map_err(|error| RubError::Internal(format!("selector serialization failed: {error}")))?;
    let kind = serde_json::to_string(field.kind.as_str())
        .map_err(|error| RubError::Internal(format!("kind serialization failed: {error}")))?;
    let attribute = serde_json::to_string(&field.attribute)
        .map_err(|error| RubError::Internal(format!("attribute serialization failed: {error}")))?;
    let first = serde_json::to_string(&field.first)
        .map_err(|error| RubError::Internal(format!("first serialization failed: {error}")))?;
    let last = serde_json::to_string(&field.last)
        .map_err(|error| RubError::Internal(format!("last serialization failed: {error}")))?;
    let nth = serde_json::to_string(&field.nth)
        .map_err(|error| RubError::Internal(format!("nth serialization failed: {error}")))?;

    Ok(format!(
        r#"(function() {{
            const selector = {selector};
            const kind = {kind};
            const attribute = {attribute};
            const first = {first};
            const last = {last};
            const nth = {nth};
            try {{
                const nodes = Array.from(document.querySelectorAll(selector));
                const selectNodes = (values) => {{
                    if (first) return values.slice(0, 1);
                    if (last) return values.slice(-1);
                    if (nth !== null && nth !== undefined) {{
                        const selected = values[nth];
                        return selected ? [selected] : [];
                    }}
                    return values;
                }};
                const readOne = (el) => {{
                    switch (kind) {{
                        case 'text':
                            return String(el.textContent || '').replace(/\s+/g, ' ').trim();
                        case 'value':
                            return 'value' in el ? String(el.value ?? '') : null;
                        case 'html':
                            return el.outerHTML || null;
                        case 'bbox': {{
                            const rect = el.getBoundingClientRect();
                            return {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }};
                        }}
                        case 'attributes':
                            return Object.fromEntries(Array.from(el.attributes || []).map(attr => [attr.name, attr.value]));
                        case 'attribute':
                            return attribute ? el.getAttribute(attribute) : null;
                        default:
                            return null;
                    }}
                }};
                const selectedNodes = selectNodes(nodes);
                return {{
                    match_count: nodes.length,
                    selected_count: selectedNodes.length,
                    values: selectedNodes.map(readOne),
                    selector_error: null,
                }};
            }} catch (error) {{
                return {{
                    match_count: 0,
                    selected_count: 0,
                    values: [],
                    selector_error: String(error && error.message ? error.message : error),
                }};
            }}
        }})()"#
    ))
}

fn field_has_selection(field: &ExtractFieldSpec) -> bool {
    field.first || field.last || field.nth.is_some()
}

fn resolve_single_collection<'a>(
    fields: &'a BTreeMap<String, ExtractEntrySpec>,
    command_label: &str,
) -> Result<(&'a str, &'a collection::ExtractCollectionSpec), RubError> {
    let mut collections = fields.iter().filter_map(|(name, entry)| match entry {
        ExtractEntrySpec::Collection(collection) => Some((name.as_str(), collection)),
        ExtractEntrySpec::Field(_) => None,
    });
    let first = collections.next();
    let second = collections.next();
    match (first, second) {
        (Some((name, collection)), None) if fields.len() == 1 => Ok((name, collection)),
        (Some(_), Some(_)) | (Some(_), None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command_label} currently requires exactly one top-level collection field"),
        )),
        (None, _) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command_label} requires a top-level collection spec"),
        )),
    }
}

async fn scan_collection(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    collection_name: &str,
    collection: &collection::ExtractCollectionSpec,
    scan: &ExtractScanConfig,
) -> Result<ExtractScanOutcome, RubError> {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut pass_count = 0u32;
    let mut scroll_count = 0u32;
    let mut no_growth_passes = 0u32;
    let mut bottom_hint = false;
    let (complete, stop_reason) = loop {
        pass_count = pass_count.saturating_add(1);
        let snapshot = build_stable_snapshot(router, args, state, None, false, false).await?;
        let snapshot = state.cache_snapshot(snapshot).await;
        let batch_value =
            extract_collection(router, &snapshot, collection_name, collection).await?;
        let batch_rows = batch_value.as_array().cloned().ok_or_else(|| {
            RubError::Internal("collection scan expected array payload".to_string())
        })?;

        let mut new_rows = 0usize;
        for (row_index, row) in batch_rows.into_iter().enumerate() {
            let fingerprint = row_fingerprint(&row, scan.dedupe_key.as_deref(), row_index)?;
            if seen.insert(fingerprint) {
                rows.push(row);
                new_rows += 1;
            }
        }

        if rows.len() >= scan.until_count as usize {
            rows.truncate(scan.until_count as usize);
            break (true, "target_reached");
        }

        if new_rows == 0 {
            no_growth_passes = no_growth_passes.saturating_add(1);
        } else {
            no_growth_passes = 0;
            bottom_hint = false;
        }

        if bottom_hint && new_rows == 0 {
            break (false, "at_bottom");
        }
        if no_growth_passes >= scan.stall_limit {
            break (false, "stalled");
        }
        if scroll_count >= scan.max_scrolls {
            break (false, "max_scrolls_reached");
        }

        let position = router
            .browser
            .scroll(
                rub_core::model::ScrollDirection::Down,
                Some(scan.scroll_amount),
            )
            .await?;
        scroll_count = scroll_count.saturating_add(1);
        bottom_hint = position.at_bottom;
        sleep(Duration::from_millis(scan.settle_ms)).await;
    };

    Ok(ExtractScanOutcome {
        returned_count: rows.len(),
        unique_count: seen.len(),
        rows,
        pass_count,
        scroll_count,
        complete,
        stop_reason,
    })
}

fn row_fingerprint(
    row: &serde_json::Value,
    dedupe_key: Option<&str>,
    row_index: usize,
) -> Result<String, RubError> {
    if let Some(path) = dedupe_key {
        let value = lookup_json_path(row, path).ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("scan_key '{path}' was missing from extracted row {row_index}"),
                serde_json::json!({
                    "scan_key": path,
                    "row_index": row_index,
                    "row": row,
                }),
            )
        })?;
        let fingerprint = match value {
            serde_json::Value::String(text) => text.clone(),
            other => serde_json::to_string(other).map_err(RubError::from)?,
        };
        if fingerprint.trim().is_empty() {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("scan_key '{path}' resolved to an empty value in row {row_index}"),
                serde_json::json!({
                    "scan_key": path,
                    "row_index": row_index,
                    "row": row,
                }),
            ));
        }
        return Ok(fingerprint);
    }

    serde_json::to_string(row).map_err(RubError::from)
}

fn lookup_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

async fn execute_json_payload_in_frame<T: DeserializeOwned>(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    script: &str,
    payload_kind: &str,
) -> Result<T, RubError> {
    let wrapped_script = format!("JSON.stringify({script})");
    let value = router
        .browser
        .execute_js_in_frame(
            Some(snapshot.frame_context.frame_id.as_str()),
            &wrapped_script,
        )
        .await?;
    let payload_json = value.as_str().ok_or_else(|| {
        RubError::Internal(format!(
            "extract {payload_kind} payload returned non-string projection: {value}"
        ))
    })?;
    serde_json::from_str(payload_json).map_err(|error| {
        RubError::Internal(format!(
            "extract {payload_kind} payload parse failed: {error}; payload={payload_json}"
        ))
    })
}

fn parse_extract_fields(
    raw: &str,
    rub_home: &std::path::Path,
) -> Result<super::secret_resolution::ResolvedJsonSpec<BTreeMap<String, ExtractEntrySpec>>, RubError>
{
    // Parse once, normalize string shorthands in-place, then resolve
    // secrets and deserialize — avoids a redundant string round-trip.
    let mut spec = super::request_args::parse_json_spec::<serde_json::Value>(raw, "extract")?;
    normalize_extract_spec_shorthands_in_place(&mut spec);
    super::secret_resolution::resolve_json_value_with_secret_resolution(spec, "extract", rub_home)
}

/// Normalize string-shorthand extract specs into full field objects in-place.
///
/// Converts `{"title": "h1"}` entries to `{"title": {"selector": "h1", "kind": "text"}}`.
/// Object entries and non-object roots pass through unchanged.
fn normalize_extract_spec_shorthands_in_place(spec: &mut serde_json::Value) {
    let Some(object) = spec.as_object_mut() else {
        return;
    };
    // Fast-path: if no string values exist, nothing to normalize.
    if !object.values().any(|v| v.is_string()) {
        return;
    }
    for value in object.values_mut() {
        if let serde_json::Value::String(selector) = value {
            *value = serde_json::json!({
                "selector": *selector,
                "kind": "text"
            });
        }
    }
}

impl ExtractKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Value => "value",
            Self::Html => "html",
            Self::Bbox => "bbox",
            Self::Attributes => "attributes",
            Self::Attribute => "attribute",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExtractCommand, ExtractFieldSpec, ExtractKind, builder_locator_expression,
        extract_builder_field_examples,
    };
    use crate::router::extract_postprocess::ExtractValueType;
    use serde_json::json;

    #[test]
    fn extract_field_supports_type_shorthand_for_kind() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "selector": "#headline",
            "type": "text"
        }))
        .expect("extract field shorthand should deserialize");

        assert_eq!(field.selector.as_deref(), Some("#headline"));
        assert!(matches!(field.kind, ExtractKind::Text));
        assert!(field.value_type.is_none());
    }

    #[test]
    fn extract_field_preserves_value_type_when_kind_is_explicit() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "selector": "#count",
            "kind": "text",
            "transform": "parse_int",
            "type": "number"
        }))
        .expect("extract field with output type should deserialize");

        assert!(matches!(field.kind, ExtractKind::Text));
        assert!(matches!(field.value_type, Some(ExtractValueType::Number)));
    }

    #[test]
    fn builder_examples_support_semantic_locators() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "kind": "attribute",
            "attribute": "src",
            "role": "img"
        }))
        .expect("semantic extract field should deserialize");

        assert_eq!(
            builder_locator_expression(&field).as_deref(),
            Some("role:img")
        );
        assert_eq!(
            extract_builder_field_examples("hero", &field),
            Some(serde_json::json!({
                "pick_first": "hero=attribute:src:role:img@first",
                "pick_last": "hero=attribute:src:role:img@last",
                "pick_nth": "hero=attribute:src:role:img@nth(0)",
                "collect_all": "hero=attribute:src:role:img@many",
            }))
        );
    }

    #[test]
    fn extract_field_rejects_unknown_fields() {
        let error = serde_json::from_value::<ExtractFieldSpec>(serde_json::json!({
            "selector": "#headline",
            "kind": "text",
            "knd": "text"
        }))
        .expect_err("unknown extract fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn extract_command_defaults_to_query_mode() {
        let parsed = ExtractCommand::parse(
            &json!({
                "spec": "{\"title\":{\"kind\":\"text\",\"selector\":\"h1\"}}",
                "snapshot_id": "snap-1",
            }),
            None,
        )
        .expect("plain extract payload should parse");

        match parsed {
            ExtractCommand::Query(args) => {
                assert_eq!(args.snapshot_id.as_deref(), Some("snap-1"));
            }
            ExtractCommand::List(_) => panic!("expected query mode"),
        }
    }

    #[test]
    fn extract_command_parses_list_scan_payload() {
        let parsed = ExtractCommand::parse(
            &json!({
                "sub": "list",
                "spec": "{\"rows\":{\"collection\":{\"selector\":\"li\",\"fields\":{\"title\":{\"kind\":\"text\",\"selector\":\".title\"}}}}}",
                "scan_until": 25,
                "scan_key": "id",
                "max_scrolls": 4,
                "scroll_amount": 900,
                "settle_ms": 300,
                "stall_limit": 2,
            }),
            None,
        )
        .expect("inspect list payload should parse");

        let scan = parsed
            .scan_config()
            .expect("scan config should validate")
            .expect("scan config should be present");
        assert_eq!(scan.until_count, 25);
        assert_eq!(scan.dedupe_key.as_deref(), Some("id"));
        assert_eq!(scan.max_scrolls, 4);
        assert_eq!(scan.scroll_amount, 900);
        assert_eq!(scan.settle_ms, 300);
        assert_eq!(scan.stall_limit, 2);
    }

    #[test]
    fn normalize_shorthand_converts_string_values_to_selector_objects() {
        let mut value: serde_json::Value = serde_json::from_str(
            r#"{"title":"h1","price":".price","link":{"selector":"a","attr":"href"}}"#,
        )
        .expect("test JSON should parse");
        super::normalize_extract_spec_shorthands_in_place(&mut value);

        // String values should be expanded to full objects
        assert_eq!(value["title"]["selector"], "h1");
        assert_eq!(value["title"]["kind"], "text");
        assert_eq!(value["price"]["selector"], ".price");
        assert_eq!(value["price"]["kind"], "text");

        // Object values should pass through unchanged
        assert_eq!(value["link"]["selector"], "a");
        assert_eq!(value["link"]["attr"], "href");
    }

    #[test]
    fn extract_field_defaults_to_text_kind_when_omitted() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "#headline"
        }))
        .expect("extract field without kind should default to text");

        assert_eq!(field.selector.as_deref(), Some("#headline"));
        assert!(matches!(field.kind, ExtractKind::Text));
    }

    #[test]
    fn extract_field_infers_attribute_kind_when_attr_present() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "a.main",
            "attr": "href"
        }))
        .expect("extract field with attr alias should infer attribute kind");

        assert!(matches!(field.kind, ExtractKind::Attribute));
        assert_eq!(field.attribute.as_deref(), Some("href"));
    }

    #[test]
    fn extract_field_accepts_attr_as_alias_for_attribute() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "img",
            "kind": "attribute",
            "attr": "src"
        }))
        .expect("attr alias should be accepted");

        assert_eq!(field.attribute.as_deref(), Some("src"));
        assert!(matches!(field.kind, ExtractKind::Attribute));
    }
}
