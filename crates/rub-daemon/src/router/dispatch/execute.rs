use std::sync::Arc;

use super::super::timeout::execute_wait_command;
use super::super::*;
use rub_core::command::CommandName;
use rub_core::error::ErrorCode;

pub(super) async fn dispatch_named_command(
    router: &DaemonRouter,
    command: &str,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    match CommandName::parse(command) {
        Some(CommandName::Handshake) => runtime::cmd_handshake(router, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::UpgradeCheck) => runtime::cmd_upgrade_check(state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::BlockerDiagnose) => runtime::cmd_blocker_diagnose(router, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::InteractabilityProbe) => {
            interaction::cmd_interactability_probe(router, args, deadline, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::FillValidate) => {
            workflow::cmd_fill_validate(router, args, deadline, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::OrchestrationProbe) => {
            orchestration::cmd_orchestration_probe(router, args, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::OrchestrationTabFrames) => {
            orchestration::cmd_orchestration_tab_frames(router, args, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::OrchestrationTargetDispatch) => {
            orchestration::cmd_orchestration_target_dispatch(router, args, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::OrchestrationWorkflowSourceVars) => {
            orchestration::cmd_orchestration_workflow_source_vars(router, args, state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::TriggerFill) => workflow::cmd_trigger_fill(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::TriggerPipe) => workflow::cmd_trigger_pipe(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Open) => navigation::cmd_open(router, args, deadline, state).await,
        Some(CommandName::State) => navigation::cmd_state(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Observe) => observe::cmd_observe(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Orchestration) => orchestration::cmd_orchestration(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Inspect) => inspect::cmd_inspect(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Find) => find::cmd_find(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Click) => interaction::cmd_click(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Exec) => query::cmd_exec(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Scroll) => navigation::cmd_scroll(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Back) => navigation::cmd_back(router, deadline, state).await,
        Some(CommandName::Forward) => navigation::cmd_forward(router, deadline, state).await,
        Some(CommandName::Reload) => navigation::cmd_reload(router, args, deadline, state).await,
        Some(CommandName::Screenshot) => navigation::cmd_screenshot(router, args)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Doctor) => runtime::cmd_doctor(router, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Runtime) => runtime::cmd_runtime(router, state, args)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Frames) => frames::cmd_frames(router, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Frame) => frames::cmd_frame(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::History) => history::cmd_history(args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Downloads) => downloads::cmd_downloads(state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Download) => downloads::cmd_download(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Storage) => storage::cmd_storage(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Handoff) => runtime::cmd_handoff(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Takeover) => runtime::cmd_takeover(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Dialog) => dialogs::cmd_dialog(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Intercept) => runtime::cmd_intercept(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Interference) => interference::cmd_interference(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Close) => runtime::cmd_close(router)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Keys) => interaction::cmd_keys(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Type) => interaction::cmd_type(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Wait) => {
            execute_wait_command(router, router.browser.clone(), args.clone(), state)
                .await
                .map(CommandDispatchOutcome::new)
        }
        Some(CommandName::Tabs) => navigation::cmd_tabs(router)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Trigger) => triggers::cmd_trigger(router, args, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Switch) => navigation::cmd_switch(router, args, deadline, state).await,
        Some(CommandName::CloseTab) => {
            navigation::cmd_close_tab(router, args, deadline, state).await
        }
        Some(CommandName::Get) => query::cmd_get(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Hover) => interaction::cmd_hover(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Cookies) => runtime::cmd_cookies(router, args)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Upload) => interaction::cmd_upload(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Select) => interaction::cmd_select(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Fill) => workflow::cmd_fill(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Extract) => extract::cmd_extract(router, args, None, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        Some(CommandName::Pipe) => workflow::cmd_pipe(router, args, deadline, state)
            .await
            .map(CommandDispatchOutcome::new),
        None => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown command: {command}"),
        )),
    }
}

pub(super) fn execute_named_command_with_fence<'a>(
    router: &'a DaemonRouter,
    command: &'a str,
    args: &'a serde_json::Value,
    deadline: TransactionDeadline,
    state: &'a Arc<SessionState>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<serde_json::Value, RubError>> + Send + 'a>,
> {
    Box::pin(async move {
        let outcome = dispatch_named_command(router, command, args, deadline, state).await?;
        let (data, pending_external_dom_commit) = outcome.into_parts();
        let data = apply_post_wait_if_requested(
            router,
            router.browser.clone(),
            command,
            args,
            data,
            state,
        )
        .await?;
        let response_epoch = response_dom_epoch(command, args, state, pending_external_dom_commit);
        Ok(if let Some(epoch) = response_epoch {
            attach_response_metadata(data, Some(epoch))
        } else {
            data
        })
    })
}

#[cfg_attr(not(test), allow(dead_code))]
fn response_dom_epoch(
    command: &str,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    pending_external_dom_commit: PendingExternalDomCommit,
) -> Option<u64> {
    super::super::policy::response_dom_epoch(command, args, state, pending_external_dom_commit)
}
