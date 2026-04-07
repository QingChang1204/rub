use super::*;

/// Strip the `sub` routing key from inspect args before forwarding to a sub-handler.
/// `cmd_inspect` reads `sub` for dispatch; sub-handlers must not see it as an unknown field.
fn strip_inspect_routing_key(args: &serde_json::Value) -> serde_json::Value {
    let mut forwarded = args.clone();
    if let Some(object) = forwarded.as_object_mut() {
        object.remove("sub");
    }
    forwarded
}

pub(super) async fn cmd_inspect(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let sub = args
        .get("sub")
        .and_then(|value| value.as_str())
        .ok_or_else(|| RubError::Internal("Missing 'sub' argument for inspect".to_string()))?;

    // Strip the routing key before forwarding. Sub-handlers must not receive "sub"
    // as an implicit extra field — any handler using deny_unknown_fields would reject it.
    // Handlers that need "sub" for their own internal dispatch (InspectReadCommand,
    // ExtractCommand, InspectNetworkCommand) re-derive it from the matched arm or their
    // own parse() methods and do not rely on it being present in the forwarded args.
    let forwarded = strip_inspect_routing_key(args);

    match sub {
        "page" => navigation::cmd_state(router, &forwarded, state).await,
        "text" | "html" | "value" | "attributes" | "bbox" => {
            // sub is passed explicitly so cmd_inspect_read doesn't need to re-read it from args.
            query::cmd_inspect_read(router, &forwarded, sub, state).await
        }
        "list" => {
            // sub_override = Some("list") so ExtractCommand selects list mode even though
            // the routing key has been stripped from forwarded args.
            extract::cmd_extract(router, &forwarded, Some("list"), state).await
        }
        "storage" => storage::cmd_inspect_storage(router, &forwarded, state).await,
        "network" | "curl" => {
            // sub is passed explicitly so InspectNetworkCommand::parse doesn't insert the
            // wrong default when the routing key is absent from forwarded args.
            network_inspection::cmd_inspect_network(&forwarded, sub, state).await
        }
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Unknown inspect subcommand: '{other}'. Valid: page, text, html, value, attributes, bbox, list, storage, network, curl"
            ),
        )),
    }
}
