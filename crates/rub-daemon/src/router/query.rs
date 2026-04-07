use super::addressing::resolve_element;
use super::projection::element_subject;
use super::request_args::{
    LocatorParseOptions, LocatorRequestArgs, canonical_locator_json, locator_json,
    parse_canonical_locator_from_value, parse_json_args, require_live_locator,
};
use super::*;
use rub_core::locator::{CanonicalLocator, LiveLocator};

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "sub", rename_all = "lowercase")]
enum GetCommand {
    Title,
    Html(GetHtmlArgs),
    Text(QueryReadArgs),
    Value(QueryReadArgs),
    Attributes(QueryReadArgs),
    Bbox(QueryReadArgs),
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    code: String,
    #[serde(default, rename = "raw")]
    _raw: bool,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GetHtmlArgs {
    #[serde(default)]
    selector: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryReadArgs {
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(flatten)]
    locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectReadArgs {
    #[serde(default)]
    many: bool,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(flatten)]
    locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

pub(super) async fn cmd_exec(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ExecArgs = parse_json_args(args, "exec")?;
    let frame_id = super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let result = router
        .browser
        .execute_js_in_frame(frame_id.as_deref(), &parsed.code)
        .await?;
    let mut subject = serde_json::Map::new();
    subject.insert(
        "kind".to_string(),
        serde_json::Value::String("script_execution".to_string()),
    );
    if let Some(frame_id) = frame_id {
        subject.insert("frame_id".to_string(), serde_json::Value::String(frame_id));
    }
    Ok(serde_json::json!({
        "subject": serde_json::Value::Object(subject),
        "result": result,
    }))
}

pub(super) async fn cmd_get(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match parse_json_args::<GetCommand>(args, "get")? {
        GetCommand::Title => {
            let title = router.browser.get_title().await?;
            Ok(read_payload(
                page_subject(None),
                scalar_read_result("title", serde_json::json!(title)),
            ))
        }
        GetCommand::Html(parsed) => cmd_get_html(router, args, parsed, state).await,
        GetCommand::Text(parsed) => {
            cmd_get_text_like(router, args, parsed, state, GetReadKind::Text).await
        }
        GetCommand::Value(parsed) => {
            cmd_get_text_like(router, args, parsed, state, GetReadKind::Value).await
        }
        GetCommand::Attributes(parsed) => {
            cmd_get_text_like(router, args, parsed, state, GetReadKind::Attributes).await
        }
        GetCommand::Bbox(parsed) => {
            cmd_get_text_like(router, args, parsed, state, GetReadKind::Bbox).await
        }
    }
}

async fn cmd_get_html(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: GetHtmlArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let selector = args.selector.as_deref();
    let frame_id = super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

    let (subject, html) = if let Some(selector) = selector {
        let locator = CanonicalLocator::Selector {
            css: selector.to_string(),
            selection: None,
        };
        let live_locator = LiveLocator::try_from(locator.clone())
            .expect("selector addressing is always valid for live HTML reads");
        (
            live_read_subject("html", &locator, frame_id.as_deref()),
            router
                .browser
                .query_html(frame_id.as_deref(), &live_locator)
                .await?,
        )
    } else if frame_id.is_some() {
        let value = router
            .browser
            .execute_js_in_frame(frame_id.as_deref(), "document.documentElement.outerHTML")
            .await?;
        (
            page_subject(frame_id.as_deref()),
            serde_json::from_value::<String>(value).map_err(|error| {
                RubError::Internal(format!("Parse get_html result failed: {error}"))
            })?,
        )
    } else {
        (page_subject(None), router.browser.get_html(None).await?)
    };

    Ok(read_payload(
        subject,
        scalar_read_result("html", serde_json::json!(html)),
    ))
}

pub(super) async fn cmd_inspect_read(
    router: &DaemonRouter,
    args: &serde_json::Value,
    inspect_sub: &str,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    // "sub" has already been stripped by cmd_inspect; dispatch using the explicit
    // inspect_sub parameter instead of re-reading it from args via a serde tag enum.
    let parsed: InspectReadArgs = parse_json_args(args, "inspect")?;
    match inspect_sub {
        "text" => cmd_inspect_text_like(router, args, parsed, state, InspectReadKind::Text).await,
        "html" => cmd_inspect_html(router, args, parsed, state).await,
        "value" => cmd_inspect_text_like(router, args, parsed, state, InspectReadKind::Value).await,
        "attributes" => {
            cmd_inspect_text_like(router, args, parsed, state, InspectReadKind::Attributes).await
        }
        "bbox" => cmd_inspect_text_like(router, args, parsed, state, InspectReadKind::Bbox).await,
        other => Err(RubError::Internal(format!(
            "Unexpected inspect read sub-command reached handler: '{other}'"
        ))),
    }
}

#[derive(Debug, Clone, Copy)]
enum GetReadKind {
    Text,
    Value,
    Attributes,
    Bbox,
}

impl GetReadKind {
    fn command_name(self) -> &'static str {
        match self {
            Self::Text => "get text",
            Self::Value => "get value",
            Self::Attributes => "get attributes",
            Self::Bbox => "get bbox",
        }
    }

    fn response_field(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Value => "value",
            Self::Attributes => "attributes",
            Self::Bbox => "bbox",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum InspectReadKind {
    Text,
    Value,
    Attributes,
    Bbox,
}

impl InspectReadKind {
    fn kind_name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Value => "value",
            Self::Attributes => "attributes",
            Self::Bbox => "bbox",
        }
    }
}

async fn cmd_get_text_like(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: QueryReadArgs,
    state: &Arc<SessionState>,
    kind: GetReadKind,
) -> Result<serde_json::Value, RubError> {
    let command_name = kind.command_name();
    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    let uses_snapshot_authority = args.snapshot_id.is_some()
        || matches!(
            locator,
            Some(CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. })
        );

    if uses_snapshot_authority {
        let resolved = resolve_element(router, raw_args, state, command_name).await?;
        let value = match kind {
            GetReadKind::Text => {
                serde_json::json!(router.browser.get_text(&resolved.element).await?)
            }
            GetReadKind::Value => {
                serde_json::json!(router.browser.get_value(&resolved.element).await?)
            }
            GetReadKind::Attributes => {
                serde_json::to_value(router.browser.get_attributes(&resolved.element).await?)
                    .map_err(RubError::from)?
            }
            GetReadKind::Bbox => {
                serde_json::to_value(router.browser.get_bbox(&resolved.element).await?)
                    .map_err(RubError::from)?
            }
        };
        return Ok(read_payload(
            element_subject(&resolved.element, &resolved.snapshot_id),
            scalar_read_result(kind.response_field(), value),
        ));
    }

    let locator = locator.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{command_name} requires an index, ref, selector, target_text, role, label, or testid"
            ),
        )
    })?;
    let locator = require_live_locator(
        locator,
        serde_json::json!({
            "command": command_name,
            "kind": kind.response_field(),
        }),
        format!(
            "{command_name} live reads require selector, target_text, role, label, or testid addressing"
        ),
        "Use --selector, --target-text, --role, --label, or --testid for a live read, or add --snapshot/--index/--ref to stay on snapshot authority",
    )?;
    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

    let value = match kind {
        GetReadKind::Text => serde_json::json!(
            router
                .browser
                .query_text(selected_frame_id.as_deref(), &locator)
                .await?
        ),
        GetReadKind::Value => serde_json::json!(
            router
                .browser
                .query_value(selected_frame_id.as_deref(), &locator)
                .await?
        ),
        GetReadKind::Attributes => serde_json::to_value(
            router
                .browser
                .query_attributes(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
        GetReadKind::Bbox => serde_json::to_value(
            router
                .browser
                .query_bbox(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
    };

    Ok(read_payload(
        live_read_subject(
            kind.response_field(),
            &locator,
            selected_frame_id.as_deref(),
        ),
        scalar_read_result(kind.response_field(), value),
    ))
}

async fn cmd_inspect_text_like(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: InspectReadArgs,
    state: &Arc<SessionState>,
    kind: InspectReadKind,
) -> Result<serde_json::Value, RubError> {
    if !args.many {
        let get_args = QueryReadArgs {
            snapshot_id: args.snapshot_id.clone(),
            locator: args.locator.clone(),
            _orchestration: args._orchestration.clone(),
        };
        return match kind {
            InspectReadKind::Text => {
                cmd_get_text_like(router, raw_args, get_args, state, GetReadKind::Text).await
            }
            InspectReadKind::Value => {
                cmd_get_text_like(router, raw_args, get_args, state, GetReadKind::Value).await
            }
            InspectReadKind::Attributes => {
                cmd_get_text_like(router, raw_args, get_args, state, GetReadKind::Attributes).await
            }
            InspectReadKind::Bbox => {
                cmd_get_text_like(router, raw_args, get_args, state, GetReadKind::Bbox).await
            }
        };
    }

    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    let uses_snapshot_authority = args.snapshot_id.is_some()
        || matches!(
            locator,
            Some(CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. })
        );

    if uses_snapshot_authority {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "inspect {} --many requires a live DOM locator, not snapshot-bound addressing",
                kind.kind_name()
            ),
            serde_json::json!({
                "kind": kind.kind_name(),
                "many": true,
                "snapshot_id": args.snapshot_id,
                "locator": locator,
            }),
            "Use --selector, --target-text, --role, --label, or --testid for a live multi-value read, or drop --many to inspect one selected element",
        ));
    }

    let locator = locator.ok_or_else(|| {
        RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "inspect {} --many requires a selector, target_text, role, label, or testid locator",
                kind.kind_name()
            ),
            serde_json::json!({
                "kind": kind.kind_name(),
                "many": true,
            }),
            "Provide a live DOM locator such as --selector, --target-text, --role, --label, or --testid",
        )
    })?;
    let locator = require_live_locator(
        locator,
        serde_json::json!({
            "kind": kind.kind_name(),
            "many": true,
        }),
        format!(
            "inspect {} --many requires a live DOM locator, not snapshot-bound addressing",
            kind.kind_name()
        ),
        "Use --selector, --target-text, --role, --label, or --testid for a live multi-value read, or drop --many to inspect one selected element",
    )?;
    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

    let items = match kind {
        InspectReadKind::Text => serde_json::to_value(
            router
                .browser
                .query_text_many(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
        InspectReadKind::Value => serde_json::to_value(
            router
                .browser
                .query_value_many(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
        InspectReadKind::Attributes => serde_json::to_value(
            router
                .browser
                .query_attributes_many(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
        InspectReadKind::Bbox => serde_json::to_value(
            router
                .browser
                .query_bbox_many(selected_frame_id.as_deref(), &locator)
                .await?,
        )
        .map_err(RubError::from)?,
    };

    Ok(read_payload(
        live_read_subject(kind.kind_name(), &locator, selected_frame_id.as_deref()),
        multi_read_result(kind.kind_name(), items),
    ))
}

async fn cmd_inspect_html(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: InspectReadArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    let uses_snapshot_authority = args.snapshot_id.is_some()
        || matches!(
            locator,
            Some(CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. })
        );

    if args.many && uses_snapshot_authority {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            "inspect html --many requires a live DOM locator, not snapshot-bound addressing",
            serde_json::json!({
                "kind": "html",
                "many": true,
                "snapshot_id": args.snapshot_id,
                "locator": locator,
            }),
            "Use --selector, --target-text, --role, --label, or --testid for a live multi-value read, or drop --many to inspect one selected element",
        ));
    }

    match (locator, args.many, uses_snapshot_authority) {
        (None, false, _) => Ok(read_payload(
            page_subject(None),
            scalar_read_result(
                "html",
                serde_json::json!(router.browser.get_html(None).await?),
            ),
        )),
        (None, true, _) => Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            "inspect html --many requires a locator",
            serde_json::json!({
                "kind": "html",
                "many": true,
            }),
            "Provide --selector, --target-text, --role, --label, or --testid to inspect multiple HTML matches",
        )),
        (Some(_locator), false, true) => {
            let resolved = resolve_element(router, raw_args, state, "inspect html").await?;
            Ok(read_payload(
                element_subject(&resolved.element, &resolved.snapshot_id),
                scalar_read_result(
                    "html",
                    serde_json::json!(router.browser.get_outer_html(&resolved.element).await?),
                ),
            ))
        }
        (Some(locator), false, false) => {
            let locator = require_live_locator(
                locator,
                serde_json::json!({
                    "kind": "html",
                    "many": false,
                }),
                "inspect html requires a live DOM locator when not using snapshot-bound addressing",
                "Use --selector, --target-text, --role, --label, or --testid for a live HTML read, or add --snapshot/--index/--ref to stay on snapshot authority",
            )?;
            let selected_frame_id =
                super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
            Ok(read_payload(
                live_read_subject("html", &locator, selected_frame_id.as_deref()),
                scalar_read_result(
                    "html",
                    serde_json::json!(
                        router
                            .browser
                            .query_html(selected_frame_id.as_deref(), &locator)
                            .await?
                    ),
                ),
            ))
        }
        (Some(locator), true, false) => {
            let locator = require_live_locator(
                locator,
                serde_json::json!({
                    "kind": "html",
                    "many": true,
                }),
                "inspect html --many requires a live DOM locator, not snapshot-bound addressing",
                "Use --selector, --target-text, --role, --label, or --testid for a live multi-value HTML read",
            )?;
            let selected_frame_id =
                super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
            let items = router
                .browser
                .query_html_many(selected_frame_id.as_deref(), &locator)
                .await?;
            Ok(read_payload(
                live_read_subject("html", &locator, selected_frame_id.as_deref()),
                multi_read_result("html", serde_json::json!(items)),
            ))
        }
        (Some(_), true, true) => unreachable!("snapshot-bound multi HTML reads are rejected above"),
    }
}

fn read_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

fn page_subject(frame_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "page",
        "frame_id": frame_id,
    })
}

fn live_read_subject(
    read_kind: &str,
    locator: &impl IntoCanonicalLocatorRef,
    frame_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "live_read",
        "read_kind": read_kind,
        "frame_id": frame_id,
        "locator": canonical_locator_json(locator.as_canonical_locator()),
    })
}

fn scalar_read_result(kind: &str, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "value": value,
    })
}

fn multi_read_result(kind: &str, items: serde_json::Value) -> serde_json::Value {
    let item_count = items.as_array().map(|value| value.len()).unwrap_or(0);
    serde_json::json!({
        "kind": kind,
        "items": items,
        "item_count": item_count,
    })
}

trait IntoCanonicalLocatorRef {
    fn as_canonical_locator(&self) -> &CanonicalLocator;
}

impl IntoCanonicalLocatorRef for CanonicalLocator {
    fn as_canonical_locator(&self) -> &CanonicalLocator {
        self
    }
}

impl IntoCanonicalLocatorRef for LiveLocator {
    fn as_canonical_locator(&self) -> &CanonicalLocator {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExecArgs, GetCommand, InspectReadArgs, live_read_subject, multi_read_result, page_subject,
        read_payload, scalar_read_result,
    };
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::locator::CanonicalLocator;
    use serde_json::json;

    #[test]
    fn live_read_subject_projects_canonical_locator_identity() {
        let locator = CanonicalLocator::Selector {
            css: ".cta".to_string(),
            selection: None,
        };
        let subject = live_read_subject("text", &locator, Some("root"));
        assert_eq!(subject["kind"], "live_read");
        assert_eq!(subject["read_kind"], "text");
        assert_eq!(subject["frame_id"], "root");
        assert_eq!(subject["locator"]["selector"], ".cta");
    }

    #[test]
    fn scalar_and_multi_read_results_share_canonical_shape() {
        let scalar = scalar_read_result("text", json!("Alpha"));
        assert_eq!(scalar["kind"], "text");
        assert_eq!(scalar["value"], "Alpha");

        let many = multi_read_result("text", json!(["Alpha", "Beta"]));
        assert_eq!(many["kind"], "text");
        assert_eq!(many["items"], json!(["Alpha", "Beta"]));
        assert_eq!(many["item_count"], 2);
    }

    #[test]
    fn read_payload_wraps_subject_and_result() {
        let payload = read_payload(
            page_subject(None),
            scalar_read_result("title", json!("Example")),
        );
        assert_eq!(payload["subject"]["kind"], "page");
        assert_eq!(payload["result"]["kind"], "title");
        assert_eq!(payload["result"]["value"], "Example");
    }

    #[test]
    fn typed_get_payload_uses_tagged_subcommand_dispatch() {
        let parsed: GetCommand = parse_json_args(
            &json!({
                "sub": "text",
                "selector": ".cta",
            }),
            "get",
        )
        .expect("get text payload should parse");
        assert!(matches!(parsed, GetCommand::Text(_)));
    }

    #[test]
    fn inspect_payload_rejects_unknown_fields_in_stripped_args() {
        // After cmd_inspect strips "sub", InspectReadArgs is parsed directly.
        // Verify that unknown fields are still rejected.
        let error = parse_json_args::<InspectReadArgs>(
            &json!({
                "selector": ".cta",
                "mystery": true,
            }),
            "inspect",
        )
        .expect_err("unknown inspect fields should be rejected")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn typed_exec_payload_accepts_raw_compat_flag() {
        let parsed: ExecArgs = parse_json_args(
            &json!({
                "code": "document.title",
                "raw": true,
            }),
            "exec",
        )
        .expect("exec payload should accept raw compatibility flag");
        assert!(parsed._raw);
    }
}
