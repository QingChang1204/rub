use super::*;

pub(super) async fn cmd_inspect(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let sub = args
        .get("sub")
        .and_then(|value| value.as_str())
        .ok_or_else(|| RubError::Internal("Missing 'sub' argument for inspect".to_string()))?;

    match sub {
        "page" => navigation::cmd_state(router, args, state).await,
        "text" | "html" | "value" | "attributes" | "bbox" => {
            query::cmd_inspect_read(router, args, state).await
        }
        "list" => extract::cmd_extract(router, args, state).await,
        "storage" => storage::cmd_inspect_storage(router, args, state).await,
        "network" | "curl" => network_inspection::cmd_inspect_network(args, state).await,
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Unknown inspect subcommand: '{other}'. Valid: page, text, html, value, attributes, bbox, list, storage, network, curl"
            ),
        )),
    }
}
