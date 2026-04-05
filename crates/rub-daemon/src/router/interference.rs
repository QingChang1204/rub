use std::sync::Arc;

use super::*;
use crate::interference_recovery;
use crate::runtime_refresh::refresh_live_runtime_and_interference;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::InterferenceMode;

pub(super) async fn cmd_interference(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let sub = args
        .get("sub")
        .and_then(|v| v.as_str())
        .unwrap_or("recover");
    match sub {
        "mode" => {
            let mode = parse_interference_mode(args.get("mode").and_then(|v| v.as_str()))?;
            state.set_interference_mode(mode).await;
            let runtime = state.interference_runtime().await;
            Ok(interference_payload(
                serde_json::json!({
                    "kind": "interference_policy",
                    "action": "mode",
                    "requested_mode": mode,
                }),
                serde_json::json!({
                    "mode": runtime.mode.clone(),
                    "active_policies": runtime.active_policies.clone(),
                }),
                runtime,
            ))
        }
        "recover" => {
            let recovery = interference_recovery::recover(&router.browser, state).await;
            let runtime = state.interference_runtime().await;
            Ok(interference_payload(
                serde_json::json!({
                    "kind": "interference_recovery",
                    "action": "recover",
                }),
                serde_json::json!({
                    "recovery": recovery,
                    "handoff": state.human_verification_handoff().await,
                }),
                runtime,
            ))
        }
        "status" => {
            let _ = refresh_live_runtime_and_interference(&router.browser, state).await;
            let runtime = state.interference_runtime().await;
            Ok(interference_payload(
                serde_json::json!({
                    "kind": "interference_runtime",
                    "action": "status",
                }),
                serde_json::json!({}),
                runtime,
            ))
        }
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown interference subcommand: '{other}'"),
        )),
    }
}

fn interference_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: rub_core::model::InterferenceRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

fn parse_interference_mode(raw: Option<&str>) -> Result<InterferenceMode, RubError> {
    match raw {
        Some("normal") => Ok(InterferenceMode::Normal),
        Some("public_web_stable") => Ok(InterferenceMode::PublicWebStable),
        Some("strict") => Ok(InterferenceMode::Strict),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown interference mode: '{other}'"),
        )),
        None => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Interference mode update requires a 'mode' argument",
        )),
    }
}
