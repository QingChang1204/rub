use super::timeout::execute_wait_command;
use super::*;
use rub_core::command::{
    CommandMetadata, CommandName, command_metadata as shared_command_metadata,
};
use rub_core::error::ErrorCode;

pub(super) fn command_metadata(command: &str) -> CommandMetadata {
    shared_command_metadata(command)
}

pub(super) fn is_internal_command(command: &str) -> bool {
    command_metadata(command).internal
}

pub(super) fn command_supports_post_wait(command: &str) -> bool {
    command_metadata(command).supports_post_wait
}

pub(super) async fn dispatch_named_command(
    router: &DaemonRouter,
    command: &str,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match CommandName::parse(command) {
        Some(CommandName::Handshake) => runtime::cmd_handshake(router, state).await,
        Some(CommandName::UpgradeCheck) => runtime::cmd_upgrade_check(state).await,
        Some(CommandName::OrchestrationProbe) => {
            orchestration::cmd_orchestration_probe(router, args, state).await
        }
        Some(CommandName::OrchestrationTargetDispatch) => {
            orchestration::cmd_orchestration_target_dispatch(router, args, state).await
        }
        Some(CommandName::OrchestrationWorkflowSourceVars) => {
            orchestration::cmd_orchestration_workflow_source_vars(router, args, state).await
        }
        Some(CommandName::Open) => navigation::cmd_open(router, args, deadline, state).await,
        Some(CommandName::State) => navigation::cmd_state(router, args, state).await,
        Some(CommandName::Observe) => observe::cmd_observe(router, args, state).await,
        Some(CommandName::Orchestration) => {
            orchestration::cmd_orchestration(router, args, state).await
        }
        Some(CommandName::Inspect) => inspect::cmd_inspect(router, args, state).await,
        Some(CommandName::Find) => find::cmd_find(router, args, state).await,
        Some(CommandName::Click) => interaction::cmd_click(router, args, state).await,
        Some(CommandName::Exec) => query::cmd_exec(router, args, state).await,
        Some(CommandName::Scroll) => navigation::cmd_scroll(router, args, state).await,
        Some(CommandName::Back) => navigation::cmd_back(router, deadline, state).await,
        Some(CommandName::Forward) => navigation::cmd_forward(router, deadline, state).await,
        Some(CommandName::Reload) => navigation::cmd_reload(router, args, deadline, state).await,
        Some(CommandName::Screenshot) => navigation::cmd_screenshot(router, args).await,
        Some(CommandName::Doctor) => runtime::cmd_doctor(router, state).await,
        Some(CommandName::Runtime) => runtime::cmd_runtime(router, state, args).await,
        Some(CommandName::Frames) => frames::cmd_frames(router, state).await,
        Some(CommandName::Frame) => frames::cmd_frame(router, args, state).await,
        Some(CommandName::History) => history::cmd_history(args, state).await,
        Some(CommandName::Downloads) => downloads::cmd_downloads(state).await,
        Some(CommandName::Download) => downloads::cmd_download(router, args, state).await,
        Some(CommandName::Storage) => storage::cmd_storage(router, args, state).await,
        Some(CommandName::Handoff) => runtime::cmd_handoff(router, args, state).await,
        Some(CommandName::Takeover) => runtime::cmd_takeover(router, args, state).await,
        Some(CommandName::Dialog) => dialogs::cmd_dialog(router, args, state).await,
        Some(CommandName::Intercept) => runtime::cmd_intercept(router, args, state).await,
        Some(CommandName::Interference) => {
            interference::cmd_interference(router, args, state).await
        }
        Some(CommandName::Close) => runtime::cmd_close(router).await,
        Some(CommandName::Keys) => interaction::cmd_keys(router, args, state).await,
        Some(CommandName::Type) => interaction::cmd_type(router, args, state).await,
        Some(CommandName::Wait) => {
            execute_wait_command(router, router.browser.clone(), args.clone(), state).await
        }
        Some(CommandName::Tabs) => navigation::cmd_tabs(router).await,
        Some(CommandName::Trigger) => triggers::cmd_trigger(router, args, state).await,
        Some(CommandName::Switch) => navigation::cmd_switch(router, args, state).await,
        Some(CommandName::CloseTab) => navigation::cmd_close_tab(router, args, state).await,
        Some(CommandName::Get) => query::cmd_get(router, args, state).await,
        Some(CommandName::Hover) => interaction::cmd_hover(router, args, state).await,
        Some(CommandName::Cookies) => runtime::cmd_cookies(router, args).await,
        Some(CommandName::Upload) => interaction::cmd_upload(router, args, state).await,
        Some(CommandName::Select) => interaction::cmd_select(router, args, state).await,
        Some(CommandName::Fill) => workflow::cmd_fill(router, args, deadline, state).await,
        Some(CommandName::Extract) => extract::cmd_extract(router, args, state).await,
        Some(CommandName::Pipe) => workflow::cmd_pipe(router, args, deadline, state).await,
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
        let data = dispatch_named_command(router, command, args, deadline, state).await?;
        let data = apply_post_wait_if_requested(
            router,
            router.browser.clone(),
            command,
            args,
            data,
            state,
        )
        .await?;
        let response_epoch = response_dom_epoch(command, args, state);
        Ok(if let Some(epoch) = response_epoch {
            attach_response_metadata(data, Some(epoch))
        } else {
            data
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{command_metadata, command_supports_post_wait, is_internal_command};

    #[test]
    fn command_metadata_single_sources_internal_and_post_wait_flags() {
        let handshake = command_metadata("_handshake");
        assert!(handshake.internal);
        assert!(!handshake.supports_post_wait);
        assert!(is_internal_command("_handshake"));

        let open = command_metadata("open");
        assert!(!open.internal);
        assert!(open.supports_post_wait);
        assert!(command_supports_post_wait("open"));

        let history = command_metadata("history");
        assert!(!history.internal);
        assert!(!history.supports_post_wait);
    }
}
