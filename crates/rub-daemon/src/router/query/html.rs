use super::super::addressing::resolve_element;
use super::super::projection::element_subject;
use super::super::request_args::{
    LocatorParseOptions, locator_json, parse_canonical_locator_from_value, require_live_locator,
};
use super::args::{GetHtmlArgs, InspectReadArgs};
use super::result_projection::{
    live_read_subject, multi_read_result, page_subject, read_payload, scalar_read_result,
};
use super::{reject_live_many_locator_selection, reject_snapshot_without_locator, *};

use rub_core::locator::{CanonicalLocator, LiveLocator};

pub(super) async fn cmd_get_html(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: GetHtmlArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let selector = args.selector.as_deref();
    let frame_id =
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

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

pub(super) async fn cmd_inspect_html(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: InspectReadArgs,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    reject_snapshot_without_locator(
        "inspect html",
        args.snapshot_id.as_deref(),
        locator.as_ref(),
    )?;
    if args.many {
        reject_live_many_locator_selection(locator.as_ref(), "html")?;
    }
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
        (None, false, _) => {
            let frame_id =
                super::super::frame_scope::effective_request_frame_id(router, raw_args, state)
                    .await?;
            let html = if frame_id.is_some() {
                let value = router
                    .browser
                    .execute_js_in_frame(frame_id.as_deref(), "document.documentElement.outerHTML")
                    .await?;
                serde_json::from_value::<String>(value).map_err(|error| {
                    RubError::Internal(format!("Parse inspect_html result failed: {error}"))
                })?
            } else {
                router.browser.get_html(None).await?
            };
            Ok(read_payload(
                page_subject(frame_id.as_deref()),
                scalar_read_result("html", serde_json::json!(html)),
            ))
        }
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
            let resolved =
                resolve_element(router, raw_args, state, deadline, "inspect html").await?;
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
                super::super::frame_scope::effective_request_frame_id(router, raw_args, state)
                    .await?;
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
                super::super::frame_scope::effective_request_frame_id(router, raw_args, state)
                    .await?;
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
