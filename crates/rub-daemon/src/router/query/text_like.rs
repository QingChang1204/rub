use super::super::addressing::resolve_element;
use super::super::projection::element_subject;
use super::super::request_args::{
    LocatorParseOptions, locator_json, parse_canonical_locator_from_value, require_live_locator,
};
use super::args::{GetReadKind, InspectReadArgs, InspectReadKind, QueryReadArgs};
use super::result_projection::{
    live_read_subject, multi_read_result, read_payload, scalar_read_result,
};
use super::{reject_live_many_locator_selection, reject_snapshot_without_locator, *};

use rub_core::locator::CanonicalLocator;

pub(super) async fn cmd_get_text_like(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: QueryReadArgs,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    kind: GetReadKind,
    command_name: &str,
) -> Result<serde_json::Value, RubError> {
    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    reject_snapshot_without_locator(command_name, args.snapshot_id.as_deref(), locator.as_ref())?;
    let uses_snapshot_authority = args.snapshot_id.is_some()
        || matches!(
            locator,
            Some(CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. })
        );

    if uses_snapshot_authority {
        let resolved = resolve_element(router, raw_args, state, deadline, command_name).await?;
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
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

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

pub(super) async fn cmd_inspect_text_like(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: InspectReadArgs,
    deadline: TransactionDeadline,
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
                cmd_get_text_like(
                    router,
                    raw_args,
                    get_args,
                    deadline,
                    state,
                    GetReadKind::Text,
                    "inspect text",
                )
                .await
            }
            InspectReadKind::Value => {
                cmd_get_text_like(
                    router,
                    raw_args,
                    get_args,
                    deadline,
                    state,
                    GetReadKind::Value,
                    "inspect value",
                )
                .await
            }
            InspectReadKind::Attributes => {
                cmd_get_text_like(
                    router,
                    raw_args,
                    get_args,
                    deadline,
                    state,
                    GetReadKind::Attributes,
                    "inspect attributes",
                )
                .await
            }
            InspectReadKind::Bbox => {
                cmd_get_text_like(
                    router,
                    raw_args,
                    get_args,
                    deadline,
                    state,
                    GetReadKind::Bbox,
                    "inspect bbox",
                )
                .await
            }
        };
    }

    let locator = parse_canonical_locator_from_value(
        &locator_json(args.locator.clone()),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )?;
    let inspect_command = format!("inspect {}", kind.kind_name());
    reject_snapshot_without_locator(
        inspect_command.as_str(),
        args.snapshot_id.as_deref(),
        locator.as_ref(),
    )?;
    reject_live_many_locator_selection(locator.as_ref(), kind.kind_name())?;
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
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;

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
