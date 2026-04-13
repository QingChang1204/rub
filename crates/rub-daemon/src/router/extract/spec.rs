use std::collections::BTreeMap;
use std::time::Duration;

use rub_core::json_spec::NormalizedJsonSpec;
use serde::Deserialize;

use super::scan::ExtractScanConfig;
use crate::router::extract_postprocess::{ExtractTransform, ExtractValueType};
use crate::router::request_args::parse_json_args;
use crate::router::secret_resolution::{
    ResolvedJsonSpec, resolve_json_value_with_secret_resolution,
};
use rub_core::error::{ErrorCode, RubError};

#[derive(Debug)]
pub(super) struct ExtractFieldSpec {
    pub(super) index: Option<u32>,
    pub(super) element_ref: Option<String>,
    pub(super) selector: Option<String>,
    pub(super) target_text: Option<String>,
    pub(super) role: Option<String>,
    pub(super) label: Option<String>,
    pub(super) testid: Option<String>,
    pub(super) first: bool,
    pub(super) last: bool,
    pub(super) nth: Option<u32>,
    pub(super) kind: ExtractKind,
    pub(super) attribute: Option<String>,
    pub(super) many: bool,
    pub(super) value_type: Option<ExtractValueType>,
    pub(super) required: bool,
    pub(super) default: Option<serde_json::Value>,
    pub(super) map: BTreeMap<String, serde_json::Value>,
    pub(super) transform: Option<ExtractTransform>,
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
pub(super) enum ExtractKind {
    Text,
    Value,
    Html,
    Bbox,
    Attributes,
    Attribute,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ContentExtractPayload {
    pub(super) match_count: usize,
    pub(super) selected_count: usize,
    pub(super) values: Vec<serde_json::Value>,
    pub(super) selector_error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtractQueryArgs {
    spec: NormalizedJsonSpec,
    #[serde(default, rename = "spec_source")]
    _spec_source: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) snapshot_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtractListArgs {
    spec: NormalizedJsonSpec,
    #[serde(default, rename = "spec_source")]
    _spec_source: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) snapshot_id: Option<String>,
    #[serde(flatten)]
    scan: ExtractScanArgs,
    #[serde(flatten)]
    wait: ExtractWaitArgs,
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

#[derive(Debug, Clone)]
pub(super) struct ExtractListWaitConfig {
    pub(super) field_path: String,
    pub(super) contains: String,
    pub(super) timeout: Duration,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExtractWaitArgs {
    wait_field: Option<String>,
    wait_contains: Option<String>,
    wait_timeout_ms: Option<u64>,
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
            .unwrap_or(u64::from(super::DEFAULT_SCAN_MAX_SCROLLS));
        if max_scrolls == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --max-scrolls must be greater than 0",
            ));
        }

        let scroll_amount = self
            .scroll_amount
            .unwrap_or(u64::from(super::DEFAULT_SCAN_SCROLL_AMOUNT));
        if scroll_amount == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list --scroll-amount must be greater than 0",
            ));
        }

        let settle_ms = self.settle_ms.unwrap_or(super::DEFAULT_SCAN_SETTLE_MS);
        let stall_limit = self
            .stall_limit
            .unwrap_or(u64::from(super::DEFAULT_SCAN_STALL_LIMIT));
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

impl ExtractWaitArgs {
    fn parse_config(&self) -> Result<Option<ExtractListWaitConfig>, RubError> {
        let field_path = self
            .wait_field
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let contains = self
            .wait_contains
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        match (field_path, contains) {
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect list wait requires both wait_field and wait_contains",
            )),
            (Some(field_path), Some(contains)) => Ok(Some(ExtractListWaitConfig {
                field_path,
                contains,
                timeout: Duration::from_millis(
                    self.wait_timeout_ms
                        .unwrap_or(rub_core::DEFAULT_WAIT_TIMEOUT_MS),
                ),
            })),
        }
    }
}

#[derive(Debug)]
pub(super) enum ExtractCommand {
    Query(ExtractQueryArgs),
    List(ExtractListArgs),
}

impl ExtractCommand {
    pub(super) fn parse(
        args: &serde_json::Value,
        sub_override: Option<&str>,
    ) -> Result<Self, RubError> {
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            if let Some(sub) = sub_override {
                object.insert("sub".to_string(), serde_json::json!(sub));
            } else {
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

    pub(super) fn spec(&self) -> &NormalizedJsonSpec {
        match self {
            Self::Query(args) => &args.spec,
            Self::List(args) => &args.spec,
        }
    }

    pub(super) fn snapshot_id(&self) -> Option<&str> {
        match self {
            Self::Query(args) => args.snapshot_id.as_deref(),
            Self::List(args) => args.snapshot_id.as_deref(),
        }
    }

    pub(super) fn scan_config(&self) -> Result<Option<ExtractScanConfig>, RubError> {
        match self {
            Self::Query(_) => Ok(None),
            Self::List(args) => args.scan.parse_config(),
        }
    }

    pub(super) fn wait_config(&self) -> Result<Option<ExtractListWaitConfig>, RubError> {
        match self {
            Self::Query(_) => Ok(None),
            Self::List(args) => args.wait.parse_config(),
        }
    }

    pub(super) fn source_kind(&self) -> &'static str {
        if self.snapshot_id().is_some() {
            "snapshot"
        } else {
            "live_page"
        }
    }
}

fn default_extract_required() -> bool {
    true
}

pub(super) fn parse_extract_fields(
    raw: &NormalizedJsonSpec,
    rub_home: &std::path::Path,
) -> Result<ResolvedJsonSpec<BTreeMap<String, super::collection::ExtractEntrySpec>>, RubError> {
    let mut spec = raw.as_value().clone();
    normalize_extract_spec_shorthands_in_place(&mut spec)?;
    resolve_json_value_with_secret_resolution(spec, "extract", rub_home)
}

/// Normalize string-shorthand extract specs into full field objects in-place.
///
/// Converts `{"title": "h1"}` entries to `{"title": {"selector": "h1", "kind": "text"}}`.
/// Object entries and non-object roots pass through unchanged.
pub(super) fn normalize_extract_spec_shorthands_in_place(
    spec: &mut serde_json::Value,
) -> Result<(), RubError> {
    let Some(object) = spec.as_object_mut() else {
        return Ok(());
    };

    normalize_extract_field_map_in_place(object, "$")
}

fn normalize_extract_field_map_in_place(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), RubError> {
    for (field_name, value) in fields.iter_mut() {
        let field_path = format!("{path}.{field_name}");
        match value {
            serde_json::Value::String(selector) => {
                *value = serde_json::json!({
                    "selector": selector,
                    "kind": "text"
                });
            }
            serde_json::Value::Object(object) => {
                if let Some(nested_fields) = object.get_mut("fields") {
                    let Some(nested_map) = nested_fields.as_object_mut() else {
                        return Err(RubError::domain_with_context(
                            ErrorCode::InvalidInput,
                            format!(
                                "Invalid JSON spec for 'extract': collection fields at '{field_path}.fields' must be an object"
                            ),
                            serde_json::json!({
                                "path": format!("{field_path}.fields"),
                                "field": field_name,
                                "surface": "extract_field_map",
                            }),
                        ));
                    };
                    normalize_extract_field_map_in_place(
                        nested_map,
                        &format!("{field_path}.fields"),
                    )?;
                }
            }
            _ => {
                return Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "Invalid JSON spec for 'extract': field entry at '{field_path}' must be an object or selector string shorthand"
                    ),
                    serde_json::json!({
                        "path": field_path,
                        "field": field_name,
                        "surface": "extract_field_map",
                    }),
                ));
            }
        }
    }

    Ok(())
}

impl ExtractKind {
    pub(super) fn as_str(self) -> &'static str {
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
        ExtractCommand, ExtractListArgs, ExtractQueryArgs, ExtractWaitArgs, parse_extract_fields,
    };
    use rub_core::error::ErrorCode;
    use rub_core::json_spec::NormalizedJsonSpec;

    #[test]
    fn extract_wait_args_require_both_field_and_contains() {
        let error = ExtractWaitArgs {
            wait_field: Some("subject".to_string()),
            wait_contains: None,
            wait_timeout_ms: None,
        }
        .parse_config()
        .expect_err("partial wait config should fail")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("requires both"), "{error}");
    }

    #[test]
    fn extract_list_command_parses_wait_config() {
        let command = ExtractCommand::parse(
            &serde_json::json!({
                "sub": "list",
                "spec": "{\"items\":{\"collection\":\".mail-row\",\"fields\":{\"subject\":{\"kind\":\"text\",\"selector\":\".subject\"}}}}",
                "wait_field": "subject",
                "wait_contains": "Confirm your account",
                "wait_timeout_ms": 5000,
            }),
            None,
        )
        .expect("list command should parse");

        let wait = command
            .wait_config()
            .expect("wait config should parse")
            .expect("wait config should be present");
        assert_eq!(wait.field_path, "subject");
        assert_eq!(wait.contains, "Confirm your account");
        assert_eq!(wait.timeout.as_millis(), 5000);
    }

    #[test]
    fn extract_args_accept_string_and_structured_spec() {
        let query = serde_json::from_value::<ExtractQueryArgs>(serde_json::json!({
            "spec": "{\"title\":{\"selector\":\"h1\",\"kind\":\"text\"}}"
        }))
        .expect("stringified extract query spec should parse");
        assert_eq!(
            query.spec.as_value(),
            &serde_json::json!({"title":{"selector":"h1","kind":"text"}})
        );

        let structured_query = serde_json::from_value::<ExtractQueryArgs>(serde_json::json!({
            "spec": { "title": { "selector": "h1", "kind": "text" } }
        }))
        .expect("structured extract query spec should parse");
        assert_eq!(
            structured_query.spec.as_value(),
            &serde_json::json!({ "title": { "selector": "h1", "kind": "text" } })
        );

        let list = serde_json::from_value::<ExtractListArgs>(serde_json::json!({
            "spec": "{\"items\":{\"collection\":\".mail-row\",\"fields\":{\"subject\":{\"selector\":\".subject\",\"kind\":\"text\"}}}}"
        }))
        .expect("stringified extract list spec should parse");
        assert_eq!(
            list.spec.as_value(),
            &serde_json::json!({
                "items":{"collection":".mail-row","fields":{"subject":{"selector":".subject","kind":"text"}}}
            })
        );

        let structured_list = serde_json::from_value::<ExtractListArgs>(serde_json::json!({
            "spec": {
                "items": {
                    "collection": ".mail-row",
                    "fields": {
                        "subject": { "selector": ".subject", "kind": "text" }
                    }
                }
            }
        }))
        .expect("structured extract list spec should parse");
        assert_eq!(
            structured_list.spec.as_value(),
            &serde_json::json!({
                "items": {
                    "collection": ".mail-row",
                    "fields": {
                        "subject": { "selector": ".subject", "kind": "text" }
                    }
                }
            })
        );
    }

    #[test]
    fn parse_extract_fields_normalizes_nested_collection_string_shorthand() {
        let home = std::env::temp_dir().join(format!(
            "rub-extract-nested-shorthand-{}",
            uuid::Uuid::now_v7()
        ));
        let spec = NormalizedJsonSpec::from_raw_str(
            r#"{
                "items": {
                    "collection": ".mail-row",
                    "fields": {
                        "subject": ".subject"
                    }
                }
            }"#,
            "extract",
        )
        .expect("nested collection shorthand fixture should parse as normalized spec");
        let parsed = parse_extract_fields(&spec, &home)
            .expect("nested collection shorthand should normalize recursively");

        assert!(parsed.value.contains_key("items"));
    }

    #[test]
    fn parse_extract_fields_accepts_nested_collection_object_entries() {
        let home = std::env::temp_dir().join(format!(
            "rub-extract-nested-object-{}",
            uuid::Uuid::now_v7()
        ));
        let spec = NormalizedJsonSpec::from_raw_str(
            r#"{
                "items": {
                    "collection": ".mail-row",
                    "fields": {
                        "subject": {
                            "selector": ".subject",
                            "kind": "text"
                        }
                    }
                }
            }"#,
            "extract",
        )
        .expect("nested collection object fixture should parse as normalized spec");
        let parsed = parse_extract_fields(&spec, &home)
            .expect("nested collection object entries should parse");

        assert!(parsed.value.contains_key("items"));
    }

    #[test]
    fn parse_extract_fields_reports_nested_field_path_for_invalid_collection_entry() {
        let home = std::env::temp_dir().join(format!(
            "rub-extract-nested-invalid-entry-{}",
            uuid::Uuid::now_v7()
        ));
        let spec = NormalizedJsonSpec::from_raw_str(
            r#"{
                "items": {
                    "collection": ".mail-row",
                    "fields": {
                        "subject": 42
                    }
                }
            }"#,
            "extract",
        )
        .expect("invalid nested entry fixture should parse as normalized spec");
        let error = parse_extract_fields(&spec, &home)
            .expect_err("invalid nested collection entry should fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope
            .context
            .expect("nested invalid entry should include path context");
        assert_eq!(context["path"], "$.items.fields.subject");
        assert_eq!(context["surface"], "extract_field_map");
    }
}
