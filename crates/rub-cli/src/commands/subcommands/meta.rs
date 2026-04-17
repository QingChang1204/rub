use rub_core::command::CommandName;

use super::{Commands, ExplainSubcommand, InspectSubcommand, WaitAfterArgs};

impl Commands {
    pub(crate) fn canonical_name(&self) -> &'static str {
        match self {
            Self::Open { .. } => CommandName::Open.as_str(),
            Self::State { .. } => CommandName::State.as_str(),
            Self::Observe { .. } => CommandName::Observe.as_str(),
            Self::Find { .. } => CommandName::Find.as_str(),
            Self::Click { .. } => CommandName::Click.as_str(),
            Self::Exec { .. } => CommandName::Exec.as_str(),
            Self::Explain { .. } => "explain",
            Self::Scroll { .. } => CommandName::Scroll.as_str(),
            Self::Back { .. } => CommandName::Back.as_str(),
            Self::Forward { .. } => CommandName::Forward.as_str(),
            Self::Reload { .. } => CommandName::Reload.as_str(),
            Self::Screenshot { .. } => CommandName::Screenshot.as_str(),
            Self::Close { .. } => CommandName::Close.as_str(),
            Self::Sessions => "sessions",
            Self::Binding { .. } => "binding",
            Self::Secret { .. } => CommandName::Secret.as_str(),
            Self::Doctor => CommandName::Doctor.as_str(),
            Self::Runtime { .. } => CommandName::Runtime.as_str(),
            Self::Trigger { .. } => CommandName::Trigger.as_str(),
            Self::Orchestration { .. } => CommandName::Orchestration.as_str(),
            Self::Frames => CommandName::Frames.as_str(),
            Self::Frame { .. } => CommandName::Frame.as_str(),
            Self::Cleanup => "cleanup",
            Self::Teardown => "teardown",
            Self::History { .. } => CommandName::History.as_str(),
            Self::Downloads => CommandName::Downloads.as_str(),
            Self::Download { .. } => CommandName::Download.as_str(),
            Self::Storage(_) => CommandName::Storage.as_str(),
            Self::Handoff { .. } => CommandName::Handoff.as_str(),
            Self::Takeover { .. } => CommandName::Takeover.as_str(),
            Self::Dialog { .. } => CommandName::Dialog.as_str(),
            Self::Intercept { .. } => CommandName::Intercept.as_str(),
            Self::Interference { .. } => CommandName::Interference.as_str(),
            Self::Keys { .. } => CommandName::Keys.as_str(),
            Self::Type { .. } => CommandName::Type.as_str(),
            Self::Wait { .. } => CommandName::Wait.as_str(),
            Self::Tabs => CommandName::Tabs.as_str(),
            Self::Switch { .. } => CommandName::Switch.as_str(),
            Self::CloseTab { .. } => CommandName::CloseTab.as_str(),
            Self::Get(_) => CommandName::Get.as_str(),
            Self::Inspect(_) => CommandName::Inspect.as_str(),
            Self::Hover { .. } => CommandName::Hover.as_str(),
            Self::Cookies(_) => CommandName::Cookies.as_str(),
            Self::Upload { .. } => CommandName::Upload.as_str(),
            Self::Select { .. } => CommandName::Select.as_str(),
            Self::Fill { .. } => CommandName::Fill.as_str(),
            Self::Extract { .. } => CommandName::Extract.as_str(),
            Self::Pipe { .. } => CommandName::Pipe.as_str(),
            Self::InternalDaemon => "__daemon",
        }
    }

    pub(crate) fn wait_after_args(&self) -> Option<&WaitAfterArgs> {
        match self {
            Self::Open { wait_after, .. }
            | Self::Back { wait_after }
            | Self::Forward { wait_after }
            | Self::Reload { wait_after, .. }
            | Self::Keys { wait_after, .. }
            | Self::Type { wait_after, .. }
            | Self::Switch { wait_after, .. }
            | Self::Hover { wait_after, .. }
            | Self::Upload { wait_after, .. }
            | Self::Select { wait_after, .. }
            | Self::Fill { wait_after, .. }
            | Self::Pipe { wait_after, .. }
            | Self::Click { wait_after, .. } => Some(wait_after),
            _ => None,
        }
    }

    pub(crate) fn local_projection_surface(&self) -> Option<&'static str> {
        match self {
            Self::Close { all: true } => Some("close --all"),
            Self::Cleanup => Some("cleanup"),
            Self::Teardown => Some("teardown"),
            Self::Pipe {
                list_workflows: true,
                ..
            } => Some("pipe list-workflows"),
            Self::Orchestration {
                subcommand: super::OrchestrationSubcommand::ListAssets,
            } => Some("orchestration list-assets"),
            Self::Inspect(InspectSubcommand::Harvest { .. }) => Some("inspect harvest"),
            Self::Inspect(InspectSubcommand::List {
                builder_help: true, ..
            }) => Some("inspect list built-in help"),
            Self::Explain {
                subcommand: ExplainSubcommand::Extract { .. },
            } => Some("explain extract"),
            Self::Extract { schema: true, .. } => Some("extract built-in help"),
            Self::Extract {
                examples: Some(_), ..
            } => Some("extract built-in help"),
            Self::Sessions => Some("sessions"),
            Self::Binding { .. } => Some("binding"),
            Self::Secret { .. } => Some("secret"),
            Self::InternalDaemon => Some("internal daemon"),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn render_nested_subcommand_long_help(parent: &str, child: &str) -> String {
    use clap::CommandFactory;

    let mut root = super::super::Cli::command();
    let mut parent_command = root
        .find_subcommand_mut(parent)
        .unwrap_or_else(|| panic!("missing subcommand {parent}"))
        .clone();
    let mut child_command = parent_command
        .find_subcommand_mut(child)
        .unwrap_or_else(|| panic!("missing subcommand {parent} {child}"))
        .clone();
    let mut buffer = Vec::new();
    child_command
        .write_long_help(&mut buffer)
        .expect("help should render");
    String::from_utf8(buffer).expect("help should be valid utf-8")
}
