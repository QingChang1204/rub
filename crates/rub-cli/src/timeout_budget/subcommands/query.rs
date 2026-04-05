use crate::commands::{CookiesSubcommand, GetSubcommand, InspectSubcommand};
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::protocol::IpcRequest;

use super::super::{
    WAIT_IPC_BUFFER_MS, element_address_args, merge_json_objects, mutating_request,
    observation_projection_args, observation_scope_args, resolve_cli_path,
    resolve_inspect_list_spec_source,
};

pub(crate) fn build_get_request(
    timeout: u64,
    subcommand: &GetSubcommand,
) -> Result<IpcRequest, RubError> {
    let args = match subcommand {
        GetSubcommand::Title => serde_json::json!({ "sub": "title" }),
        GetSubcommand::Html { selector } => {
            serde_json::json!({ "sub": "html", "selector": selector })
        }
        GetSubcommand::Text { index, target } => merge_json_objects(
            serde_json::json!({ "sub": "text" }),
            element_address_args(*index, target)?,
        ),
        GetSubcommand::Value { index, target } => merge_json_objects(
            serde_json::json!({ "sub": "value" }),
            element_address_args(*index, target)?,
        ),
        GetSubcommand::Attributes { index, target } => merge_json_objects(
            serde_json::json!({ "sub": "attributes" }),
            element_address_args(*index, target)?,
        ),
        GetSubcommand::Bbox { index, target } => merge_json_objects(
            serde_json::json!({ "sub": "bbox" }),
            element_address_args(*index, target)?,
        ),
    };
    Ok(IpcRequest::new("get", args, timeout))
}

pub(crate) fn build_inspect_request(
    timeout: u64,
    subcommand: &InspectSubcommand,
) -> Result<IpcRequest, RubError> {
    let effective_network_wait_timeout = effective_network_wait_timeout_ms(timeout, subcommand);
    let args = match subcommand {
        InspectSubcommand::Page {
            limit,
            format,
            a11y,
            viewport,
            listeners,
            scope,
            projection,
        } => merge_json_objects(
            serde_json::json!({
                "sub": "page",
                "limit": limit,
                "format": format.map(|value| value.as_str()),
                "a11y": a11y,
                "viewport": viewport,
                "listeners": listeners,
            }),
            merge_json_objects(
                observation_scope_args(scope)?,
                observation_projection_args(projection),
            ),
        ),
        InspectSubcommand::Text {
            index,
            target,
            many,
        } => merge_json_objects(
            serde_json::json!({ "sub": "text" }),
            merge_json_objects(
                element_address_args(*index, target)?,
                serde_json::json!({ "many": many }),
            ),
        ),
        InspectSubcommand::Html {
            index,
            target,
            many,
        } => merge_json_objects(
            serde_json::json!({ "sub": "html" }),
            merge_json_objects(
                element_address_args(*index, target)?,
                serde_json::json!({ "many": many }),
            ),
        ),
        InspectSubcommand::Value {
            index,
            target,
            many,
        } => merge_json_objects(
            serde_json::json!({ "sub": "value" }),
            merge_json_objects(
                element_address_args(*index, target)?,
                serde_json::json!({ "many": many }),
            ),
        ),
        InspectSubcommand::Attributes {
            index,
            target,
            many,
        } => merge_json_objects(
            serde_json::json!({ "sub": "attributes" }),
            merge_json_objects(
                element_address_args(*index, target)?,
                serde_json::json!({ "many": many }),
            ),
        ),
        InspectSubcommand::Bbox {
            index,
            target,
            many,
        } => merge_json_objects(
            serde_json::json!({ "sub": "bbox" }),
            merge_json_objects(
                element_address_args(*index, target)?,
                serde_json::json!({ "many": many }),
            ),
        ),
        InspectSubcommand::List {
            spec,
            file,
            collection,
            row_scope,
            field,
            snapshot,
            scan_until,
            scan_key,
            max_scrolls,
            scroll_amount,
            settle_ms,
            stall_limit,
        } => {
            let (resolved_spec, spec_source) = resolve_inspect_list_spec_source(
                spec.as_deref(),
                file.as_deref(),
                collection.as_deref(),
                row_scope.as_deref(),
                field,
            )?;
            serde_json::json!({
                "sub": "list",
                "spec": resolved_spec,
                "spec_source": spec_source,
                "snapshot_id": snapshot,
                "scan_until": scan_until,
                "scan_key": scan_key,
                "max_scrolls": max_scrolls,
                "scroll_amount": scroll_amount,
                "settle_ms": settle_ms,
                "stall_limit": stall_limit,
            })
        }
        InspectSubcommand::Harvest { .. } => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "inspect harvest is handled locally and should not build an IPC request",
            ));
        }
        InspectSubcommand::Storage { area, key } => serde_json::json!({
            "sub": "storage",
            "area": area.map(|value| value.as_str()),
            "key": key,
        }),
        InspectSubcommand::Network {
            id,
            wait,
            last,
            url_match,
            method,
            status,
            lifecycle,
            timeout: _,
        } => serde_json::json!({
            "sub": "network",
            "id": id,
            "wait": wait,
            "last": last,
            "url_match": url_match,
            "method": method,
            "status": status,
            "lifecycle": lifecycle,
            "timeout_ms": effective_network_wait_timeout,
        }),
        InspectSubcommand::Curl { id } => serde_json::json!({
            "sub": "curl",
            "id": id,
        }),
    };
    Ok(IpcRequest::new(
        "inspect",
        args,
        inspect_request_timeout(timeout, subcommand),
    ))
}

fn inspect_request_timeout(timeout: u64, subcommand: &InspectSubcommand) -> u64 {
    match subcommand {
        InspectSubcommand::List {
            scan_until: Some(_),
            max_scrolls,
            settle_ms,
            ..
        } => {
            let max_scrolls = u64::from(max_scrolls.unwrap_or(100));
            let settle_ms = settle_ms.unwrap_or(1_200);
            timeout
                .saturating_add(max_scrolls.saturating_mul(settle_ms))
                .saturating_add(WAIT_IPC_BUFFER_MS)
        }
        InspectSubcommand::Network { wait: true, .. } => {
            effective_network_wait_timeout_ms(timeout, subcommand)
                .unwrap_or(timeout)
                .saturating_add(WAIT_IPC_BUFFER_MS)
        }
        _ => timeout,
    }
}

fn effective_network_wait_timeout_ms(timeout: u64, subcommand: &InspectSubcommand) -> Option<u64> {
    match subcommand {
        InspectSubcommand::Network {
            wait: true,
            timeout: Some(wait_timeout),
            ..
        } => Some(*wait_timeout),
        InspectSubcommand::Network {
            wait: true,
            timeout: None,
            ..
        } => Some(timeout),
        _ => None,
    }
}

pub(crate) fn build_cookies_request(
    timeout: u64,
    subcommand: &CookiesSubcommand,
) -> Result<IpcRequest, RubError> {
    let (args, mutating) = match subcommand {
        CookiesSubcommand::Get { url } => (serde_json::json!({ "sub": "get", "url": url }), false),
        CookiesSubcommand::Set {
            name,
            value,
            domain,
            path,
            secure,
            http_only,
            same_site,
            expires,
        } => (
            serde_json::json!({
                "sub": "set",
                "name": name,
                "value": value,
                "domain": domain,
                "path": path,
                "secure": secure,
                "http_only": http_only,
                "same_site": same_site,
                "expires": expires,
            }),
            true,
        ),
        CookiesSubcommand::Clear { url } => {
            (serde_json::json!({ "sub": "clear", "url": url }), true)
        }
        CookiesSubcommand::Export { path } => {
            let abs = resolve_cli_path(path);
            (
                serde_json::json!({ "sub": "export", "path": abs.to_string_lossy() }),
                false,
            )
        }
        CookiesSubcommand::Import { path } => {
            let abs = resolve_cli_path(path);
            (
                serde_json::json!({ "sub": "import", "path": abs.to_string_lossy() }),
                true,
            )
        }
    };
    if mutating {
        Ok(mutating_request("cookies", args, timeout))
    } else {
        Ok(IpcRequest::new("cookies", args, timeout))
    }
}
