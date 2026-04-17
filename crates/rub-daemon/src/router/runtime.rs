mod admin;
mod binding_capture;
mod blockers;
mod cookies;
mod doctor;
mod intercept;
mod intercept_args;
mod projection;
mod surface;
mod takeover;

pub(super) use self::admin::{cmd_close, cmd_handshake, cmd_upgrade_check};
pub(super) use self::doctor::cmd_doctor;
use self::projection::{
    intercept_payload, intercept_registry_subject, intercept_rule_id_subject,
    intercept_rule_subject, project_network_rule, project_network_rules,
};
use super::artifacts::{INPUT_ARTIFACT_DURABILITY, output_artifact_durability};
#[cfg(test)]
use super::request_args::parse_json_args;
use super::*;

pub(super) async fn cmd_runtime(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    surface::cmd_runtime(router, state, args).await
}

pub(super) async fn cmd_blocker_diagnose(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    blockers::cmd_blocker_diagnose(router, state).await
}

pub(super) async fn cmd_handoff(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    takeover::cmd_handoff(router, args, state).await
}

pub(super) async fn cmd_takeover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    takeover::cmd_takeover(router, args, state).await
}

pub(super) async fn cmd_cookies(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    cookies::cmd_cookies(router, args).await
}

pub(super) async fn cmd_intercept(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    intercept::cmd_intercept(router, args, state).await
}

#[cfg(test)]
mod tests;
