mod args;
mod html;
mod result_projection;
mod text_like;

use self::args::{ExecArgs, GetCommand, GetReadKind, InspectReadArgs, InspectReadKind};
use self::html::{cmd_get_html, cmd_inspect_html};
use self::result_projection::{page_subject, read_payload, scalar_read_result};
use self::text_like::{cmd_get_text_like, cmd_inspect_text_like};
use super::request_args::parse_json_args;
use super::*;

pub(super) async fn cmd_exec(
    router: &DaemonRouter,
    args: &serde_json::Value,
    _state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ExecArgs = parse_json_args(args, "exec")?;
    let frame_id = super::frame_scope::explicit_or_top_frame_request_id(router, args).await?;
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
    deadline: TransactionDeadline,
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
            cmd_get_text_like(router, args, parsed, deadline, state, GetReadKind::Text).await
        }
        GetCommand::Value(parsed) => {
            cmd_get_text_like(router, args, parsed, deadline, state, GetReadKind::Value).await
        }
        GetCommand::Attributes(parsed) => {
            cmd_get_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                GetReadKind::Attributes,
            )
            .await
        }
        GetCommand::Bbox(parsed) => {
            cmd_get_text_like(router, args, parsed, deadline, state, GetReadKind::Bbox).await
        }
    }
}

pub(super) async fn cmd_inspect_read(
    router: &DaemonRouter,
    args: &serde_json::Value,
    inspect_sub: &str,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    // "sub" has already been stripped by cmd_inspect; dispatch using the explicit
    // inspect_sub parameter instead of re-reading it from args via a serde tag enum.
    let parsed: InspectReadArgs = parse_json_args(args, "inspect")?;
    match inspect_sub {
        "text" => {
            cmd_inspect_text_like(router, args, parsed, deadline, state, InspectReadKind::Text)
                .await
        }
        "html" => cmd_inspect_html(router, args, parsed, deadline, state).await,
        "value" => {
            cmd_inspect_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                InspectReadKind::Value,
            )
            .await
        }
        "attributes" => {
            cmd_inspect_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                InspectReadKind::Attributes,
            )
            .await
        }
        "bbox" => {
            cmd_inspect_text_like(router, args, parsed, deadline, state, InspectReadKind::Bbox)
                .await
        }
        other => Err(RubError::Internal(format!(
            "Unexpected inspect read sub-command reached handler: '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExecArgs, GetCommand, InspectReadArgs, page_subject, read_payload, scalar_read_result,
    };
    use crate::router::query::result_projection::{live_read_subject, multi_read_result};
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
