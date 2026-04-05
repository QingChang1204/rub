use std::collections::BTreeMap;

use super::request_args::{parse_json_args, required_string_arg, subcommand_arg};
use super::*;
use crate::runtime_refresh::{
    refresh_live_dialog_runtime, refresh_live_frame_runtime, refresh_live_interference_state,
    refresh_live_runtime_and_interference, refresh_live_runtime_state,
    refresh_live_storage_runtime, refresh_live_trigger_runtime, refresh_orchestration_runtime,
    refresh_takeover_runtime,
};
use rub_core::fs::atomic_write_bytes;
use rub_core::model::{
    Cookie, FrameContextStatus, HumanVerificationHandoffStatus, IntegrationRuntimeStatus,
    IntegrationSurface, ReadinessStatus, TakeoverRuntimeStatus, TakeoverTransitionKind,
    TakeoverTransitionResult,
};

#[derive(Clone, Copy, Debug)]
enum RuntimeSurface {
    Summary,
    Integration,
    Dialog,
    Downloads,
    Frame,
    Interference,
    Storage,
    Takeover,
    Orchestration,
    Trigger,
    Observatory,
    StateInspector,
    Readiness,
    Handoff,
}

impl RuntimeSurface {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        Self::parse_name(subcommand_arg(args, "summary"))
    }

    fn parse_name(name: &str) -> Result<Self, RubError> {
        match name {
            "summary" => Ok(Self::Summary),
            "integration" => Ok(Self::Integration),
            "dialog" => Ok(Self::Dialog),
            "downloads" => Ok(Self::Downloads),
            "frame" => Ok(Self::Frame),
            "interference" => Ok(Self::Interference),
            "storage" => Ok(Self::Storage),
            "takeover" => Ok(Self::Takeover),
            "orchestration" => Ok(Self::Orchestration),
            "trigger" => Ok(Self::Trigger),
            "observatory" => Ok(Self::Observatory),
            "state-inspector" => Ok(Self::StateInspector),
            "readiness" => Ok(Self::Readiness),
            "handoff" => Ok(Self::Handoff),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown runtime subcommand: '{other}'"),
            )),
        }
    }

    fn subject(self) -> serde_json::Value {
        runtime_subject(self.name())
    }

    fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Integration => "integration",
            Self::Dialog => "dialog",
            Self::Downloads => "downloads",
            Self::Frame => "frame",
            Self::Interference => "interference",
            Self::Storage => "storage",
            Self::Takeover => "takeover",
            Self::Orchestration => "orchestration",
            Self::Trigger => "trigger",
            Self::Observatory => "observatory",
            Self::StateInspector => "state-inspector",
            Self::Readiness => "readiness",
            Self::Handoff => "handoff",
        }
    }

    async fn refresh(self, router: &DaemonRouter, state: &Arc<SessionState>) {
        match self {
            Self::Summary => {
                refresh_live_runtime_state(&router.browser, state).await;
                refresh_live_dialog_runtime(&router.browser, state).await;
                refresh_live_frame_runtime(&router.browser, state).await;
                refresh_live_storage_runtime(&router.browser, state).await;
                refresh_takeover_runtime(&router.browser, state).await;
                refresh_orchestration_runtime(state).await;
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
                let _ = refresh_live_interference_state(&router.browser, state).await;
            }
            Self::Integration | Self::StateInspector | Self::Readiness => {
                refresh_live_runtime_state(&router.browser, state).await;
            }
            Self::Dialog => {
                refresh_live_dialog_runtime(&router.browser, state).await;
            }
            Self::Frame => {
                refresh_live_frame_runtime(&router.browser, state).await;
            }
            Self::Interference => {
                let _ = refresh_live_interference_state(&router.browser, state).await;
            }
            Self::Storage => {
                refresh_live_storage_runtime(&router.browser, state).await;
            }
            Self::Takeover => {
                refresh_takeover_runtime(&router.browser, state).await;
            }
            Self::Orchestration => {
                refresh_orchestration_runtime(state).await;
            }
            Self::Trigger => {
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
            }
            Self::Observatory | Self::Downloads | Self::Handoff => {}
        }
    }

    async fn projection(self, state: &Arc<SessionState>) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Summary => Ok(runtime_summary(state).await),
            Self::Integration => {
                serde_json::to_value(state.integration_runtime().await).map_err(RubError::from)
            }
            Self::Dialog => {
                serde_json::to_value(state.dialog_runtime().await).map_err(RubError::from)
            }
            Self::Downloads => {
                serde_json::to_value(state.download_runtime().await).map_err(RubError::from)
            }
            Self::Frame => {
                serde_json::to_value(state.frame_runtime().await).map_err(RubError::from)
            }
            Self::Interference => {
                serde_json::to_value(state.interference_runtime().await).map_err(RubError::from)
            }
            Self::Storage => {
                serde_json::to_value(state.storage_runtime().await).map_err(RubError::from)
            }
            Self::Takeover => {
                serde_json::to_value(state.takeover_runtime().await).map_err(RubError::from)
            }
            Self::Orchestration => {
                serde_json::to_value(state.orchestration_runtime().await).map_err(RubError::from)
            }
            Self::Trigger => {
                serde_json::to_value(state.trigger_runtime().await).map_err(RubError::from)
            }
            Self::Observatory => {
                serde_json::to_value(state.observatory().await).map_err(RubError::from)
            }
            Self::StateInspector => {
                serde_json::to_value(state.state_inspector().await).map_err(RubError::from)
            }
            Self::Readiness => {
                serde_json::to_value(state.readiness_state().await).map_err(RubError::from)
            }
            Self::Handoff => serde_json::to_value(state.human_verification_handoff().await)
                .map_err(RubError::from),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum InterceptAction {
    List,
    Rewrite,
    Block,
    Allow,
    Header,
    Remove,
    Clear,
}

impl InterceptAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "list") {
            "list" => Ok(Self::List),
            "rewrite" => Ok(Self::Rewrite),
            "block" => Ok(Self::Block),
            "allow" => Ok(Self::Allow),
            "header" => Ok(Self::Header),
            "remove" => Ok(Self::Remove),
            "clear" => Ok(Self::Clear),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown intercept subcommand: '{other}'"),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CookieAction {
    Get,
    Set,
    Clear,
    Export,
    Import,
}

impl CookieAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match required_string_arg(args, "sub")?.as_str() {
            "get" => Ok(Self::Get),
            "set" => Ok(Self::Set),
            "clear" => Ok(Self::Clear),
            "export" => Ok(Self::Export),
            "import" => Ok(Self::Import),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown cookies subcommand: '{other}'"),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum HandoffAction {
    Status,
    Start,
    Complete,
}

impl HandoffAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "status") {
            "status" => Ok(Self::Status),
            "start" => Ok(Self::Start),
            "complete" => Ok(Self::Complete),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown handoff subcommand: '{other}'"),
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Start => "start",
            Self::Complete => "complete",
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        match self {
            Self::Status => Ok(()),
            Self::Start => {
                ensure_handoff_available(state).await?;
                state.activate_handoff().await;
                refresh_takeover_runtime(&router.browser, state).await;
                Ok(())
            }
            Self::Complete => {
                ensure_handoff_available(state).await?;
                state.complete_handoff().await;
                refresh_takeover_runtime(&router.browser, state).await;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TakeoverAction {
    Status,
    Start,
    Elevate,
    Resume,
}

impl TakeoverAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "status") {
            "status" => Ok(Self::Status),
            "start" => Ok(Self::Start),
            "elevate" => Ok(Self::Elevate),
            "resume" => Ok(Self::Resume),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown takeover subcommand: '{other}'"),
            )),
        }
    }

    fn kind(self) -> TakeoverTransitionKind {
        match self {
            Self::Status => TakeoverTransitionKind::Start,
            Self::Start => TakeoverTransitionKind::Start,
            Self::Elevate => TakeoverTransitionKind::Elevate,
            Self::Resume => TakeoverTransitionKind::Resume,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Start => "start",
            Self::Elevate => "elevate",
            Self::Resume => "resume",
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        match self {
            Self::Status => Ok(()),
            Self::Start => self.execute_start(router, state).await,
            Self::Elevate => self.execute_elevate(router, state).await,
            Self::Resume => self.execute_resume(router, state).await,
        }
    }

    async fn execute_start(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !matches!(
            current.status,
            TakeoverRuntimeStatus::Available | TakeoverRuntimeStatus::Active
        ) {
            return Err(self
                .reject(
                    state,
                    current.unavailable_reason.clone(),
                    "Session takeover is unavailable for this session",
                    serde_json::json!({ "takeover_runtime": state.takeover_runtime().await }),
                )
                .await);
        }
        state.activate_handoff().await;
        refresh_takeover_runtime(&router.browser, state).await;
        self.record_success(state).await;
        Ok(())
    }

    async fn execute_elevate(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !current.elevate_supported {
            let reason = current
                .unavailable_reason
                .clone()
                .or_else(|| Some("elevation_not_supported".to_string()));
            return Err(self
                .reject(
                    state,
                    reason.clone(),
                    "Session takeover elevation is unavailable for this session",
                    serde_json::json!({
                        "takeover_runtime": state.takeover_runtime().await,
                        "reason": reason,
                    }),
                )
                .await);
        }
        router.browser.elevate_to_visible().await?;
        state.set_handoff_available(true).await;
        refresh_takeover_runtime(&router.browser, state).await;
        if let Err(error) = verify_takeover_continuity(router, state).await {
            self.record_rejection(state, Some("continuity_fence_failed".to_string()))
                .await;
            return Err(error);
        }
        self.record_success(state).await;
        Ok(())
    }

    async fn execute_resume(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !current.automation_paused || !current.resume_supported {
            let reason = if !current.resume_supported {
                current
                    .unavailable_reason
                    .clone()
                    .or_else(|| Some("resume_not_supported".to_string()))
            } else {
                Some("takeover_not_active".to_string())
            };
            return Err(self
                .reject(
                    state,
                    reason.clone(),
                    "Session takeover resume is unavailable for this session",
                    serde_json::json!({
                        "takeover_runtime": state.takeover_runtime().await,
                        "reason": reason,
                    }),
                )
                .await);
        }
        state.complete_handoff().await;
        refresh_takeover_runtime(&router.browser, state).await;
        if let Err(error) = verify_takeover_continuity(router, state).await {
            state.activate_handoff().await;
            refresh_takeover_runtime(&router.browser, state).await;
            self.record_rejection(state, Some("continuity_fence_failed".to_string()))
                .await;
            return Err(error);
        }
        let resumed_runtime = state.takeover_runtime().await;
        let handoff = state.human_verification_handoff().await;
        if let Some(error) = takeover_resume_repaused_error(&resumed_runtime, &handoff) {
            self.record_rejection(state, Some("automation_repaused_by_policy".to_string()))
                .await;
            return Err(error);
        }
        self.record_success(state).await;
        Ok(())
    }

    async fn reject(
        self,
        state: &Arc<SessionState>,
        reason: Option<String>,
        message: &'static str,
        context: serde_json::Value,
    ) -> RubError {
        self.record_rejection(state, reason).await;
        RubError::domain_with_context(ErrorCode::InvalidInput, message, context)
    }

    async fn record_rejection(self, state: &Arc<SessionState>, reason: Option<String>) {
        state
            .record_takeover_transition(self.kind(), TakeoverTransitionResult::Rejected, reason)
            .await;
    }

    async fn record_success(self, state: &Arc<SessionState>) {
        state
            .record_takeover_transition(self.kind(), TakeoverTransitionResult::Succeeded, None)
            .await;
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterceptRewriteArgs {
    #[serde(rename = "sub")]
    _sub: String,
    source_pattern: String,
    target_base: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterceptUrlPatternArgs {
    #[serde(rename = "sub")]
    _sub: String,
    url_pattern: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterceptHeaderArgs {
    #[serde(rename = "sub")]
    _sub: String,
    url_pattern: String,
    headers: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterceptRemoveArgs {
    #[serde(rename = "sub")]
    _sub: String,
    id: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CookiesUrlArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CookiesPathArgs {
    #[serde(rename = "sub")]
    _sub: String,
    path: String,
}

pub(super) async fn cmd_doctor(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let browser_healthy = router.browser.health_check().await.is_ok();
    refresh_live_runtime_state(&router.browser, state).await;
    refresh_live_dialog_runtime(&router.browser, state).await;
    refresh_live_frame_runtime(&router.browser, state).await;
    refresh_live_storage_runtime(&router.browser, state).await;
    refresh_takeover_runtime(&router.browser, state).await;
    refresh_orchestration_runtime(state).await;
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let _ = refresh_live_interference_state(&router.browser, state).await;
    let launch_policy = router.browser.launch_policy();
    let report = crate::health::build_report(
        &state.session_id,
        &state.session_name,
        &state.rub_home,
        true,
    );
    let detection_risks = detection_risks(&launch_policy);

    Ok(serde_json::json!({
        "subject": {
            "kind": "session_diagnostics",
            "session_id": report.session_id,
            "session_name": report.session_name,
        },
        "result": {
            "browser": {
                "found": report.browser_found,
                "path": report.browser_path,
                "version": report.browser_version,
                "healthy": browser_healthy,
            },
            "daemon": {
                "running": true,
                "pid": std::process::id(),
                "session_id": report.session_id,
                "session_name": report.session_name,
                "uptime_seconds": state.uptime_seconds(),
                "in_flight": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
            },
            "socket": {
                "path": state.socket_path(),
                "healthy": true,
            },
            "disk": {
                "rub_home": report.rub_home,
                "log_size_mb": report.daemon_log_size_mb,
            },
            "versions": {
                "rub": report.rub_version,
                "ipc_protocol_version": report.ipc_protocol_version,
            },
            "launch_policy": launch_policy,
            "capabilities": agent_capabilities(),
            "dom_epoch": state.current_epoch(),
            "detection_risks": detection_risks,
        },
        "runtime": runtime_summary(state).await,
    }))
}

pub(super) async fn cmd_runtime(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    super::request_args::reject_unknown_fields(args, &["sub"], "runtime")?;
    let surface = RuntimeSurface::parse(args)?;
    surface.refresh(router, state).await;
    Ok(runtime_surface_payload(
        surface.subject(),
        surface.projection(state).await?,
    ))
}

pub(super) async fn cmd_handoff(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let action = HandoffAction::parse(args)?;
    action.execute(router, state).await?;

    Ok(runtime_surface_payload(
        serde_json::json!({
            "kind": "human_verification_handoff",
            "action": action.name(),
        }),
        serde_json::to_value(state.human_verification_handoff().await).map_err(RubError::from)?,
    ))
}

pub(super) async fn cmd_takeover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_takeover_runtime(&router.browser, state).await;

    let action = TakeoverAction::parse(args)?;
    action.execute(router, state).await?;

    Ok(runtime_surface_payload(
        serde_json::json!({
            "kind": "takeover",
            "action": action.name(),
        }),
        serde_json::to_value(state.takeover_runtime().await).map_err(RubError::from)?,
    ))
}

async fn ensure_handoff_available(state: &Arc<SessionState>) -> Result<(), RubError> {
    let current = state.human_verification_handoff().await;
    if matches!(current.status, HumanVerificationHandoffStatus::Unavailable) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Human verification handoff is unavailable for this session",
        ));
    }
    Ok(())
}

async fn verify_takeover_continuity(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<(), RubError> {
    let tabs = refresh_live_runtime_and_interference(&router.browser, state)
        .await
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!("Takeover continuity fence failed while refreshing runtime: {error}"),
                serde_json::json!({ "phase": "runtime_refresh" }),
            )
        })?;

    let active_tab = tabs.iter().any(|tab| tab.active);
    let frame_runtime = state.frame_runtime().await;
    let readiness = state.readiness_state().await;
    let integration = state.integration_runtime().await;

    let failure = takeover_continuity_failure(active_tab, &frame_runtime, &readiness, &integration);

    if let Some((reason, message)) = failure {
        state.mark_takeover_runtime_degraded(reason).await;
        return Err(RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            message,
            serde_json::json!({
                "reason": reason,
                "frame_runtime": frame_runtime,
                "readiness_state": readiness,
                "integration_runtime": integration,
                "takeover_runtime": state.takeover_runtime().await,
            }),
        ));
    }

    state.clear_takeover_runtime_degraded().await;
    refresh_takeover_runtime(&router.browser, state).await;
    Ok(())
}

fn takeover_continuity_failure(
    active_tab: bool,
    frame_runtime: &rub_core::model::FrameRuntimeInfo,
    readiness: &rub_core::model::ReadinessInfo,
    integration: &rub_core::model::IntegrationRuntimeInfo,
) -> Option<(&'static str, &'static str)> {
    if !active_tab {
        return Some((
            "continuity_no_active_tab",
            "No active tab remained after takeover transition",
        ));
    }
    if matches!(
        frame_runtime.status,
        FrameContextStatus::Unknown | FrameContextStatus::Stale | FrameContextStatus::Degraded
    ) || frame_runtime.current_frame.is_none()
    {
        return Some((
            "continuity_frame_unavailable",
            "Frame context became unavailable after takeover transition",
        ));
    }
    if matches!(readiness.status, ReadinessStatus::Degraded) {
        return Some((
            "continuity_readiness_degraded",
            "Readiness surface degraded after takeover transition",
        ));
    }
    let takeover_required_surface_degraded = integration.degraded_surfaces.iter().any(|surface| {
        matches!(
            surface,
            IntegrationSurface::RequestRules | IntegrationSurface::RuntimeObservatory
        )
    });
    if matches!(integration.status, IntegrationRuntimeStatus::Degraded)
        && takeover_required_surface_degraded
    {
        return Some((
            "continuity_runtime_degraded",
            "Integration runtime degraded after takeover transition",
        ));
    }
    None
}

fn takeover_resume_repaused_error(
    takeover: &rub_core::model::TakeoverRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> Option<RubError> {
    if !takeover.automation_paused && !handoff.automation_paused {
        return None;
    }

    Some(RubError::domain_with_context(
        ErrorCode::AutomationPaused,
        "Session takeover resumed briefly but policy-driven handoff immediately re-paused automation",
        serde_json::json!({
            "takeover_runtime": takeover,
            "handoff": handoff,
        }),
    ))
}

pub(super) async fn cmd_intercept(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match InterceptAction::parse(args)? {
        InterceptAction::List => {
            let rules = state.network_rules().await;
            Ok(intercept_payload(
                intercept_registry_subject(),
                serde_json::json!({
                    "rules": project_network_rules(&rules),
                }),
                serde_json::json!(state.integration_runtime().await),
            ))
        }
        InterceptAction::Rewrite => {
            let parsed = parse_json_args::<InterceptRewriteArgs>(args, "intercept rewrite")?;
            validate_rewrite_pattern(&parsed.source_pattern)?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Rewrite {
                    url_pattern: parsed.source_pattern,
                    target_base: parsed.target_base,
                },
            )
            .await
        }
        InterceptAction::Block => {
            let parsed = parse_json_args::<InterceptUrlPatternArgs>(args, "intercept block")?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Block {
                    url_pattern: parsed.url_pattern,
                },
            )
            .await
        }
        InterceptAction::Allow => {
            let parsed = parse_json_args::<InterceptUrlPatternArgs>(args, "intercept allow")?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Allow {
                    url_pattern: parsed.url_pattern,
                },
            )
            .await
        }
        InterceptAction::Header => {
            let parsed = parse_json_args::<InterceptHeaderArgs>(args, "intercept header")?;
            let headers = parse_header_overrides(&parsed.headers)?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::HeaderOverride {
                    url_pattern: parsed.url_pattern,
                    headers,
                },
            )
            .await
        }
        InterceptAction::Remove => {
            let parsed = parse_json_args::<InterceptRemoveArgs>(args, "intercept remove")?;
            remove_intercept_rule(router, state, parsed.id).await
        }
        InterceptAction::Clear => clear_intercept_rules(router, state).await,
    }
}

async fn create_intercept_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    spec: NetworkRuleSpec,
) -> Result<serde_json::Value, RubError> {
    let mut rules = state.network_rules().await;
    let rule = NetworkRule {
        id: state.next_network_rule_id(),
        status: NetworkRuleStatus::Active,
        spec,
    };
    rules.push(rule.clone());
    commit_intercept_registry_change(
        router,
        state,
        intercept_rule_subject(&rule),
        serde_json::json!({
            "rule": project_network_rule(&rule),
            "rules": project_network_rules(&rules),
        }),
        rules,
    )
    .await
}

async fn remove_intercept_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    id: u32,
) -> Result<serde_json::Value, RubError> {
    let current = state.network_rules().await;
    if !current.iter().any(|rule| rule.id == id) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Intercept rule {id} does not exist"),
        ));
    }

    let rules = current
        .into_iter()
        .filter(|rule| rule.id != id)
        .collect::<Vec<_>>();
    commit_intercept_registry_change(
        router,
        state,
        intercept_rule_id_subject(id),
        serde_json::json!({
            "removed_id": id,
            "rules": project_network_rules(&rules),
        }),
        rules,
    )
    .await
}

async fn clear_intercept_rules(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    commit_intercept_registry_change(
        router,
        state,
        intercept_registry_subject(),
        serde_json::json!({
            "cleared": true,
            "rules": [],
        }),
        Vec::new(),
    )
    .await
}

async fn commit_intercept_registry_change(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    subject: serde_json::Value,
    result: serde_json::Value,
    rules: Vec<NetworkRule>,
) -> Result<serde_json::Value, RubError> {
    router.browser.sync_network_rules(&rules).await?;
    state.replace_network_rules(rules).await;
    Ok(intercept_payload(
        subject,
        result,
        serde_json::json!(state.integration_runtime().await),
    ))
}

pub(super) async fn cmd_close(router: &DaemonRouter) -> Result<serde_json::Value, RubError> {
    router.browser.close().await?;
    Ok(serde_json::json!({
        "subject": {
            "kind": "session_browser",
        },
        "result": {
            "closed": true,
            "daemon_stopped": false,
            "daemon_exit_policy": "idle_timeout_or_shutdown_signal",
        }
    }))
}

pub(super) async fn cmd_handshake(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let runtime_state = state.runtime_state_snapshot().await;
    Ok(serde_json::json!({
        "daemon_session_id": state.session_id,
        "ipc_protocol_version": IPC_PROTOCOL_VERSION,
        "in_flight_count": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
        "connected_client_count": state.connected_client_count.load(std::sync::atomic::Ordering::SeqCst),
        "launch_policy": router.browser.launch_policy(),
        "integration_runtime": state.integration_runtime().await,
        "dialog_runtime": state.dialog_runtime().await,
        "download_runtime": state.download_runtime().await,
        "frame_runtime": state.frame_runtime().await,
        "interference_runtime": state.interference_runtime().await,
        "storage_runtime": state.storage_runtime().await,
        "takeover_runtime": state.takeover_runtime().await,
        "orchestration_runtime": state.orchestration_runtime().await,
        "trigger_runtime": state.trigger_runtime().await,
        "runtime_observatory": state.observatory().await,
        "state_inspector": runtime_state.state_inspector,
        "readiness_state": runtime_state.readiness_state,
        "human_verification_handoff": state.human_verification_handoff().await,
        "capabilities": agent_capabilities(),
    }))
}

pub(super) async fn cmd_upgrade_check(
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let active_trigger_count = state.active_trigger_count().await;
    let active_orchestration_count = state.active_orchestration_count().await;
    let human_control_active = state.has_active_human_control().await;
    Ok(serde_json::json!({
        "idle": state.is_idle_for_upgrade().await && active_trigger_count == 0 && active_orchestration_count == 0,
        "in_flight_count": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
        "connected_client_count": state.connected_client_count.load(std::sync::atomic::Ordering::SeqCst),
        "active_trigger_count": active_trigger_count,
        "active_orchestration_count": active_orchestration_count,
        "human_control_active": human_control_active,
    }))
}

pub(super) async fn cmd_cookies(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CookieSetArgs {
        #[serde(rename = "sub")]
        _sub: String,
        name: String,
        value: String,
        domain: String,
        #[serde(default = "default_cookie_path")]
        path: String,
        #[serde(default)]
        secure: bool,
        #[serde(default)]
        http_only: bool,
        #[serde(default = "default_cookie_same_site")]
        same_site: String,
        #[serde(default)]
        expires: Option<f64>,
    }

    match CookieAction::parse(args)? {
        CookieAction::Get => {
            let parsed = parse_json_args::<CookiesUrlArgs>(args, "cookies get")?;
            let cookies = router.browser.get_cookies(parsed.url.as_deref()).await?;
            Ok(cookie_payload(
                cookies_subject(parsed.url.as_deref()),
                serde_json::json!({
                    "cookies": cookies,
                }),
                None,
            ))
        }
        CookieAction::Set => {
            let parsed = parse_json_args::<CookieSetArgs>(args, "cookies set")?;
            if !matches!(
                parsed.same_site.as_str(),
                "Strict" | "strict" | "Lax" | "lax" | "None" | "none"
            ) {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    format!(
                        "Invalid sameSite value '{}'. Valid: Strict, Lax, None",
                        parsed.same_site
                    ),
                ));
            }
            let cookie = Cookie {
                name: parsed.name,
                value: parsed.value,
                domain: parsed.domain,
                path: parsed.path,
                secure: parsed.secure,
                http_only: parsed.http_only,
                same_site: parsed.same_site,
                expires: parsed.expires,
            };
            router.browser.set_cookie(&cookie).await?;
            Ok(cookie_payload(
                cookie_subject(&cookie),
                serde_json::json!({
                    "cookie": cookie,
                }),
                None,
            ))
        }
        CookieAction::Clear => {
            let parsed = parse_json_args::<CookiesUrlArgs>(args, "cookies clear")?;
            router.browser.delete_cookies(parsed.url.as_deref()).await?;
            Ok(cookie_payload(
                cookies_subject(parsed.url.as_deref()),
                serde_json::json!({
                    "cleared": true,
                }),
                None,
            ))
        }
        CookieAction::Export => {
            let parsed = parse_json_args::<CookiesPathArgs>(args, "cookies export")?;
            let cookies = router.browser.get_cookies(None).await?;
            let json = serde_json::to_string_pretty(&cookies)
                .map_err(|e| RubError::Internal(format!("Serialize cookies failed: {e}")))?;
            atomic_write_bytes(std::path::Path::new(&parsed.path), json.as_bytes(), 0o600)
                .map(|_| ())
                .map_err(|e| RubError::Internal(format!("Cannot write file: {e}")))?;

            Ok(cookie_payload(
                cookies_subject(None),
                serde_json::json!({
                    "count": cookies.len(),
                }),
                Some(cookie_artifact(&parsed.path, "output")),
            ))
        }
        CookieAction::Import => {
            let parsed = parse_json_args::<CookiesPathArgs>(args, "cookies import")?;
            let data = std::fs::read_to_string(&parsed.path).map_err(|e| {
                RubError::domain(ErrorCode::FileNotFound, format!("Cannot read file: {e}"))
            })?;
            let cookies: Vec<Cookie> = serde_json::from_str(&data).map_err(|e| {
                RubError::domain(ErrorCode::InvalidInput, format!("Invalid JSON: {e}"))
            })?;
            let previous_cookies = router.browser.get_cookies(None).await?;
            let count = cookies.len();
            for (index, cookie) in cookies.iter().enumerate() {
                if let Err(error) = router.browser.set_cookie(cookie).await {
                    let rollback = restore_cookie_batch(router, &previous_cookies).await;
                    return Err(cookie_import_error(
                        &parsed.path,
                        index,
                        error,
                        rollback.err(),
                    ));
                }
            }
            Ok(cookie_payload(
                cookies_subject(None),
                serde_json::json!({
                    "imported": count,
                }),
                Some(cookie_artifact(&parsed.path, "input")),
            ))
        }
    }
}

fn default_cookie_path() -> String {
    "/".to_string()
}

fn default_cookie_same_site() -> String {
    "Lax".to_string()
}

fn validate_rewrite_pattern(pattern: &str) -> Result<(), RubError> {
    let wildcard_count = pattern.matches('*').count();
    if wildcard_count > 1 || (wildcard_count == 1 && !pattern.ends_with('*')) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Rewrite patterns must be exact URLs or a single trailing-* prefix pattern",
        ));
    }
    Ok(())
}

fn project_network_rules(rules: &[NetworkRule]) -> Vec<serde_json::Value> {
    rules.iter().map(project_network_rule).collect()
}

fn project_network_rule(rule: &NetworkRule) -> serde_json::Value {
    let (action, pattern, extra) = match &rule.spec {
        NetworkRuleSpec::Rewrite {
            url_pattern,
            target_base,
        } => (
            "rewrite",
            url_pattern.as_str(),
            serde_json::json!({ "target_base": target_base }),
        ),
        NetworkRuleSpec::Block { url_pattern } => {
            ("block", url_pattern.as_str(), serde_json::json!({}))
        }
        NetworkRuleSpec::Allow { url_pattern } => {
            ("allow", url_pattern.as_str(), serde_json::json!({}))
        }
        NetworkRuleSpec::HeaderOverride {
            url_pattern,
            headers,
        } => (
            "header_override",
            url_pattern.as_str(),
            serde_json::json!({ "headers": headers }),
        ),
    };

    let mut value = serde_json::json!({
        "id": rule.id,
        "status": rule.status,
        "action": action,
        "pattern": pattern,
    });
    if let Some(object) = value.as_object_mut()
        && let Some(extra_object) = extra.as_object()
    {
        object.extend(extra_object.clone());
    }
    value
}

fn intercept_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

fn intercept_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "intercept_rule_registry",
    })
}

fn intercept_rule_subject(rule: &NetworkRule) -> serde_json::Value {
    let (action, pattern) = match &rule.spec {
        NetworkRuleSpec::Rewrite { url_pattern, .. } => ("rewrite", url_pattern.as_str()),
        NetworkRuleSpec::Block { url_pattern } => ("block", url_pattern.as_str()),
        NetworkRuleSpec::Allow { url_pattern } => ("allow", url_pattern.as_str()),
        NetworkRuleSpec::HeaderOverride { url_pattern, .. } => {
            ("header_override", url_pattern.as_str())
        }
    };
    serde_json::json!({
        "kind": "intercept_rule",
        "action": action,
        "pattern": pattern,
    })
}

fn intercept_rule_id_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "intercept_rule",
        "id": id,
    })
}

fn cookie_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    artifact: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "subject": subject,
        "result": result,
    });
    if let Some(object) = payload.as_object_mut()
        && let Some(artifact) = artifact
    {
        object.insert("artifact".to_string(), artifact);
    }
    payload
}

fn cookies_subject(url: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "cookies",
        "url": url,
    })
}

fn cookie_subject(cookie: &Cookie) -> serde_json::Value {
    serde_json::json!({
        "kind": "cookie",
        "name": cookie.name,
        "domain": cookie.domain,
        "path": cookie.path,
    })
}

fn cookie_artifact(path: &str, direction: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "cookies_archive",
        "format": "json",
        "path": path,
        "direction": direction,
    })
}

async fn restore_cookie_batch(router: &DaemonRouter, cookies: &[Cookie]) -> Result<(), RubError> {
    router.browser.delete_cookies(None).await?;
    for cookie in cookies {
        router.browser.set_cookie(cookie).await?;
    }
    Ok(())
}

fn cookie_import_error(
    path: &str,
    index: usize,
    import_error: RubError,
    rollback_error: Option<RubError>,
) -> RubError {
    match rollback_error {
        Some(rollback_error) => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cookie import failed at index {index}: {import_error}"),
            serde_json::json!({
                "path": path,
                "cookie_index": index,
                "rollback_failed": true,
                "rollback_error": rollback_error.into_envelope(),
            }),
        ),
        None => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Cookie import failed at index {index}: {import_error}"),
            serde_json::json!({
                "path": path,
                "cookie_index": index,
                "rollback_failed": false,
            }),
        ),
    }
}

fn runtime_surface_payload(
    subject: serde_json::Value,
    runtime: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "runtime": runtime,
    })
}

fn runtime_subject(surface: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "runtime_surface",
        "surface": surface,
    })
}

async fn runtime_summary(state: &Arc<SessionState>) -> serde_json::Value {
    let runtime_state = state.runtime_state_snapshot().await;
    serde_json::json!({
        "integration_runtime": state.integration_runtime().await,
        "dialog_runtime": state.dialog_runtime().await,
        "download_runtime": state.download_runtime().await,
        "frame_runtime": state.frame_runtime().await,
        "interference_runtime": state.interference_runtime().await,
        "storage_runtime": state.storage_runtime().await,
        "takeover_runtime": state.takeover_runtime().await,
        "orchestration_runtime": state.orchestration_runtime().await,
        "trigger_runtime": state.trigger_runtime().await,
        "runtime_observatory": state.observatory().await,
        "state_inspector": runtime_state.state_inspector,
        "readiness_state": runtime_state.readiness_state,
        "human_verification_handoff": state.human_verification_handoff().await,
    })
}

fn parse_header_overrides(raw_headers: &[String]) -> Result<BTreeMap<String, String>, RubError> {
    if raw_headers.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Intercept header requires at least one --header NAME=VALUE entry",
        ));
    }

    let mut headers = BTreeMap::new();
    for entry in raw_headers {
        let Some((name, value)) = entry.split_once('=') else {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid header override '{entry}'. Use NAME=VALUE"),
            ));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid header override '{entry}'. Header name cannot be empty"),
            ));
        }
        headers.insert(name.to_string(), value.to_string());
    }

    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::{
        cmd_cookies, cmd_handoff, cookie_artifact, cookie_payload, cookie_subject, cookies_subject,
        intercept_payload, intercept_registry_subject, intercept_rule_id_subject,
        intercept_rule_subject, project_network_rule, takeover_continuity_failure,
        takeover_resume_repaused_error,
    };
    use crate::session::SessionState;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{
        Cookie, FrameContextInfo, FrameContextStatus, HumanVerificationHandoffInfo,
        HumanVerificationHandoffStatus, IntegrationMode, IntegrationRuntimeInfo,
        IntegrationRuntimeStatus, IntegrationSurface, NetworkRule, NetworkRuleSpec, ReadinessInfo,
        ReadinessStatus, TakeoverRuntimeInfo, TakeoverRuntimeStatus,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn test_router() -> crate::router::DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        crate::router::DaemonRouter::new(adapter)
    }

    #[tokio::test]
    async fn handoff_start_and_complete_follow_session_state_machine() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-test"),
            None,
        ));
        state.set_handoff_available(true).await;

        let started = cmd_handoff(&router, &serde_json::json!({ "sub": "start" }), &state)
            .await
            .expect("handoff start should succeed");
        assert_eq!(started["runtime"]["status"], "active");
        assert_eq!(started["runtime"]["automation_paused"], true);
        assert_eq!(started["runtime"]["resume_supported"], true);
        assert!(state.has_active_human_control().await);
        assert!(state.takeover_runtime().await.automation_paused);

        let completed = cmd_handoff(&router, &serde_json::json!({ "sub": "complete" }), &state)
            .await
            .expect("handoff complete should succeed");
        assert_eq!(completed["runtime"]["status"], "completed");
        assert_eq!(completed["runtime"]["automation_paused"], false);
        assert_eq!(completed["runtime"]["resume_supported"], true);
        assert!(!state.has_active_human_control().await);
        assert!(!state.takeover_runtime().await.automation_paused);
    }

    #[tokio::test]
    async fn handoff_unavailable_rejects_start_and_complete() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-test"),
            None,
        ));

        let start = cmd_handoff(&router, &serde_json::json!({ "sub": "start" }), &state).await;
        let complete =
            cmd_handoff(&router, &serde_json::json!({ "sub": "complete" }), &state).await;

        let start_code = match start {
            Err(RubError::Domain(envelope)) => envelope.code,
            other => panic!("expected invalid-input domain error, got {other:?}"),
        };
        let complete_code = match complete {
            Err(RubError::Domain(envelope)) => envelope.code,
            other => panic!("expected invalid-input domain error, got {other:?}"),
        };

        assert_eq!(start_code, ErrorCode::InvalidInput);
        assert_eq!(complete_code, ErrorCode::InvalidInput);
    }

    #[test]
    fn project_network_rule_uses_canonical_action_and_pattern_fields_only() {
        let rule = NetworkRule {
            id: 7,
            status: rub_core::model::NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::HeaderOverride {
                url_pattern: "https://example.com/*".to_string(),
                headers: std::collections::BTreeMap::from([(
                    "x-rub-env".to_string(),
                    "dev".to_string(),
                )]),
            },
        };

        let value = project_network_rule(&rule);
        assert_eq!(value["action"], "header_override");
        assert_eq!(value["pattern"], "https://example.com/*");
        assert!(value.get("kind").is_none(), "{value}");
        assert!(value.get("url_pattern").is_none(), "{value}");
    }

    #[test]
    fn intercept_payload_uses_subject_result_runtime_envelope() {
        let payload = intercept_payload(
            intercept_registry_subject(),
            serde_json::json!({ "rules": [] }),
            serde_json::json!({ "status": "active" }),
        );
        assert_eq!(payload["subject"]["kind"], "intercept_rule_registry");
        assert_eq!(payload["result"]["rules"], serde_json::json!([]));
        assert_eq!(payload["runtime"]["status"], "active");
    }

    #[tokio::test]
    async fn cookies_set_rejects_unknown_or_mistyped_fields() {
        let router = test_router();
        let error = cmd_cookies(
            &router,
            &serde_json::json!({
                "sub": "set",
                "name": "session",
                "value": "abc",
                "domain": "example.com",
                "secure": "true",
                "same_sitee": "Lax"
            }),
        )
        .await
        .expect_err("invalid cookie set payload must fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("Invalid cookies set payload"));
    }

    #[test]
    fn intercept_subject_helpers_are_machine_facing() {
        let rule = NetworkRule {
            id: 3,
            status: rub_core::model::NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::Rewrite {
                url_pattern: "https://example.com/*".to_string(),
                target_base: "http://localhost:3000/mock".to_string(),
            },
        };
        let subject = intercept_rule_subject(&rule);
        assert_eq!(subject["kind"], "intercept_rule");
        assert_eq!(subject["action"], "rewrite");
        assert_eq!(subject["pattern"], "https://example.com/*");

        let by_id = intercept_rule_id_subject(3);
        assert_eq!(by_id["kind"], "intercept_rule");
        assert_eq!(by_id["id"], 3);
    }

    #[test]
    fn cookie_payload_uses_subject_result_and_artifact() {
        let cookie = Cookie {
            name: "sid".to_string(),
            value: "abc".to_string(),
            domain: "example.test".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: "Lax".to_string(),
            expires: None,
        };
        let payload = cookie_payload(
            cookie_subject(&cookie),
            serde_json::json!({ "cookie": cookie }),
            Some(cookie_artifact("/tmp/cookies.json", "output")),
        );
        assert_eq!(payload["subject"]["kind"], "cookie");
        assert_eq!(payload["result"]["cookie"]["name"], "sid");
        assert_eq!(payload["artifact"]["kind"], "cookies_archive");
        assert_eq!(payload["artifact"]["direction"], "output");
    }

    #[test]
    fn cookies_subject_is_url_scoped_collection_identity() {
        let subject = cookies_subject(Some("https://example.test"));
        assert_eq!(subject["kind"], "cookies");
        assert_eq!(subject["url"], "https://example.test");
    }

    #[test]
    fn continuity_failure_requires_active_tab_and_live_frame_context() {
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        let readiness = ReadinessInfo::default();
        let integration = IntegrationRuntimeInfo::default();

        let no_tab = takeover_continuity_failure(false, &frame_runtime, &readiness, &integration);
        assert_eq!(
            no_tab,
            Some((
                "continuity_no_active_tab",
                "No active tab remained after takeover transition",
            ))
        );

        let with_tab = takeover_continuity_failure(true, &frame_runtime, &readiness, &integration);
        assert_eq!(
            with_tab,
            Some((
                "continuity_frame_unavailable",
                "Frame context became unavailable after takeover transition",
            ))
        );
    }

    #[test]
    fn continuity_failure_degrades_readiness_and_integration_surfaces() {
        let frame_runtime = rub_core::model::FrameRuntimeInfo {
            status: FrameContextStatus::Top,
            current_frame: Some(FrameContextInfo {
                frame_id: "root".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: None,
                url: Some("https://example.com".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            primary_frame: None,
            frame_lineage: vec!["root".to_string()],
            degraded_reason: None,
        };
        let degraded_readiness = ReadinessInfo {
            status: ReadinessStatus::Degraded,
            ..ReadinessInfo::default()
        };
        let degraded_integration = IntegrationRuntimeInfo {
            mode: IntegrationMode::Normal,
            status: IntegrationRuntimeStatus::Degraded,
            degraded_surfaces: vec![IntegrationSurface::RuntimeObservatory],
            ..IntegrationRuntimeInfo::default()
        };
        let degraded_optional_integration = IntegrationRuntimeInfo {
            mode: IntegrationMode::Normal,
            status: IntegrationRuntimeStatus::Degraded,
            degraded_surfaces: vec![IntegrationSurface::StateInspector],
            ..IntegrationRuntimeInfo::default()
        };

        assert_eq!(
            takeover_continuity_failure(
                true,
                &frame_runtime,
                &degraded_readiness,
                &IntegrationRuntimeInfo::default()
            ),
            Some((
                "continuity_readiness_degraded",
                "Readiness surface degraded after takeover transition",
            ))
        );
        assert_eq!(
            takeover_continuity_failure(
                true,
                &frame_runtime,
                &ReadinessInfo::default(),
                &degraded_integration
            ),
            Some((
                "continuity_runtime_degraded",
                "Integration runtime degraded after takeover transition",
            ))
        );
        assert_eq!(
            takeover_continuity_failure(
                true,
                &frame_runtime,
                &ReadinessInfo::default(),
                &degraded_optional_integration
            ),
            None
        );
        assert_eq!(
            takeover_continuity_failure(
                true,
                &frame_runtime,
                &ReadinessInfo::default(),
                &IntegrationRuntimeInfo::default()
            ),
            None
        );
    }

    #[test]
    fn takeover_resume_repaused_error_rejects_reactivated_handoff() {
        let error = takeover_resume_repaused_error(
            &TakeoverRuntimeInfo {
                status: TakeoverRuntimeStatus::Active,
                automation_paused: true,
                ..TakeoverRuntimeInfo::default()
            },
            &HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Active,
                automation_paused: true,
                ..HumanVerificationHandoffInfo::default()
            },
        )
        .expect("repaused runtime should reject resume");

        assert_eq!(error.into_envelope().code, ErrorCode::AutomationPaused);
    }
}
