#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandMetadata {
    pub internal: bool,
    pub supports_post_wait: bool,
    pub in_process_only: bool,
    pub transport_protocol_compat_exempt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandName {
    Handshake,
    UpgradeCheck,
    BlockerDiagnose,
    InteractabilityProbe,
    FillValidate,
    OrchestrationProbe,
    OrchestrationTabFrames,
    OrchestrationTargetDispatch,
    OrchestrationWorkflowSourceVars,
    TriggerFill,
    TriggerPipe,
    Open,
    State,
    Observe,
    Orchestration,
    Inspect,
    Find,
    Click,
    Exec,
    Scroll,
    Back,
    Forward,
    Reload,
    Screenshot,
    Doctor,
    Runtime,
    Frames,
    Frame,
    History,
    Downloads,
    Download,
    Storage,
    Handoff,
    Takeover,
    Dialog,
    Intercept,
    Interference,
    Close,
    Secret,
    Keys,
    Type,
    Wait,
    Tabs,
    Trigger,
    Switch,
    CloseTab,
    Get,
    Hover,
    Cookies,
    Upload,
    Select,
    Fill,
    Extract,
    Pipe,
}

impl CommandName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handshake => "_handshake",
            Self::UpgradeCheck => "_upgrade_check",
            Self::BlockerDiagnose => "_blocker_diagnose",
            Self::InteractabilityProbe => "_interactability_probe",
            Self::FillValidate => "_fill_validate",
            Self::OrchestrationProbe => "_orchestration_probe",
            Self::OrchestrationTabFrames => "_orchestration_tab_frames",
            Self::OrchestrationTargetDispatch => "_orchestration_target_dispatch",
            Self::OrchestrationWorkflowSourceVars => "_orchestration_workflow_source_vars",
            Self::TriggerFill => "_trigger_fill",
            Self::TriggerPipe => "_trigger_pipe",
            Self::Open => "open",
            Self::State => "state",
            Self::Observe => "observe",
            Self::Orchestration => "orchestration",
            Self::Inspect => "inspect",
            Self::Find => "find",
            Self::Click => "click",
            Self::Exec => "exec",
            Self::Scroll => "scroll",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Reload => "reload",
            Self::Screenshot => "screenshot",
            Self::Doctor => "doctor",
            Self::Runtime => "runtime",
            Self::Frames => "frames",
            Self::Frame => "frame",
            Self::History => "history",
            Self::Downloads => "downloads",
            Self::Download => "download",
            Self::Storage => "storage",
            Self::Handoff => "handoff",
            Self::Takeover => "takeover",
            Self::Dialog => "dialog",
            Self::Intercept => "intercept",
            Self::Interference => "interference",
            Self::Close => "close",
            Self::Secret => "secret",
            Self::Keys => "keys",
            Self::Type => "type",
            Self::Wait => "wait",
            Self::Tabs => "tabs",
            Self::Trigger => "trigger",
            Self::Switch => "switch",
            Self::CloseTab => "close-tab",
            Self::Get => "get",
            Self::Hover => "hover",
            Self::Cookies => "cookies",
            Self::Upload => "upload",
            Self::Select => "select",
            Self::Fill => "fill",
            Self::Extract => "extract",
            Self::Pipe => "pipe",
        }
    }

    pub const fn metadata(self) -> CommandMetadata {
        match self {
            Self::Handshake | Self::UpgradeCheck | Self::BlockerDiagnose => CommandMetadata {
                internal: true,
                supports_post_wait: false,
                in_process_only: false,
                transport_protocol_compat_exempt: true,
            },
            Self::InteractabilityProbe
            | Self::FillValidate
            | Self::OrchestrationProbe
            | Self::OrchestrationTabFrames
            | Self::OrchestrationTargetDispatch
            | Self::OrchestrationWorkflowSourceVars => CommandMetadata {
                internal: true,
                supports_post_wait: false,
                in_process_only: false,
                transport_protocol_compat_exempt: false,
            },
            Self::TriggerFill | Self::TriggerPipe => CommandMetadata {
                internal: true,
                supports_post_wait: true,
                in_process_only: true,
                transport_protocol_compat_exempt: false,
            },
            Self::Open
            | Self::Exec
            | Self::Back
            | Self::Forward
            | Self::Reload
            | Self::Switch
            | Self::Click
            | Self::Keys
            | Self::Type
            | Self::Hover
            | Self::Upload
            | Self::Select
            | Self::Fill
            | Self::Pipe => CommandMetadata {
                internal: false,
                supports_post_wait: true,
                in_process_only: false,
                transport_protocol_compat_exempt: false,
            },
            _ => CommandMetadata {
                internal: false,
                supports_post_wait: false,
                in_process_only: false,
                transport_protocol_compat_exempt: false,
            },
        }
    }

    pub fn parse(command: &str) -> Option<Self> {
        match command {
            "_handshake" => Some(Self::Handshake),
            "_upgrade_check" => Some(Self::UpgradeCheck),
            "_blocker_diagnose" => Some(Self::BlockerDiagnose),
            "_interactability_probe" => Some(Self::InteractabilityProbe),
            "_fill_validate" => Some(Self::FillValidate),
            "_orchestration_probe" => Some(Self::OrchestrationProbe),
            "_orchestration_tab_frames" => Some(Self::OrchestrationTabFrames),
            "_orchestration_target_dispatch" => Some(Self::OrchestrationTargetDispatch),
            "_orchestration_workflow_source_vars" => Some(Self::OrchestrationWorkflowSourceVars),
            "_trigger_fill" => Some(Self::TriggerFill),
            "_trigger_pipe" => Some(Self::TriggerPipe),
            "open" => Some(Self::Open),
            "state" => Some(Self::State),
            "observe" => Some(Self::Observe),
            "orchestration" => Some(Self::Orchestration),
            "inspect" => Some(Self::Inspect),
            "find" => Some(Self::Find),
            "click" => Some(Self::Click),
            "exec" => Some(Self::Exec),
            "scroll" => Some(Self::Scroll),
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            "reload" => Some(Self::Reload),
            "screenshot" => Some(Self::Screenshot),
            "doctor" => Some(Self::Doctor),
            "runtime" => Some(Self::Runtime),
            "frames" => Some(Self::Frames),
            "frame" => Some(Self::Frame),
            "history" => Some(Self::History),
            "downloads" => Some(Self::Downloads),
            "download" => Some(Self::Download),
            "storage" => Some(Self::Storage),
            "handoff" => Some(Self::Handoff),
            "takeover" => Some(Self::Takeover),
            "dialog" => Some(Self::Dialog),
            "intercept" => Some(Self::Intercept),
            "interference" => Some(Self::Interference),
            "close" => Some(Self::Close),
            "secret" => Some(Self::Secret),
            "keys" => Some(Self::Keys),
            "type" => Some(Self::Type),
            "wait" => Some(Self::Wait),
            "tabs" => Some(Self::Tabs),
            "trigger" => Some(Self::Trigger),
            "switch" => Some(Self::Switch),
            "close-tab" => Some(Self::CloseTab),
            "get" => Some(Self::Get),
            "hover" => Some(Self::Hover),
            "cookies" => Some(Self::Cookies),
            "upload" => Some(Self::Upload),
            "select" => Some(Self::Select),
            "fill" => Some(Self::Fill),
            "extract" => Some(Self::Extract),
            "pipe" => Some(Self::Pipe),
            _ => None,
        }
    }
}

pub fn command_metadata(command: &str) -> CommandMetadata {
    CommandName::parse(command)
        .map(CommandName::metadata)
        .unwrap_or_default()
}

pub fn is_transport_exposed_internal_command(command: &str) -> bool {
    let metadata = command_metadata(command);
    metadata.internal && !metadata.in_process_only
}

pub fn allows_transport_protocol_compat_exemption(command: &str) -> bool {
    let metadata = command_metadata(command);
    metadata.internal && !metadata.in_process_only && metadata.transport_protocol_compat_exempt
}

pub fn allows_missing_request_command_id(command: &str) -> bool {
    matches!(
        CommandName::parse(command),
        Some(CommandName::Handshake | CommandName::UpgradeCheck | CommandName::BlockerDiagnose)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CommandName, allows_missing_request_command_id, allows_transport_protocol_compat_exemption,
        command_metadata, is_transport_exposed_internal_command,
    };

    /// Every CommandName variant must round-trip through parse(as_str()).
    /// This is a regression guard: adding a new command requires updating
    /// both as_str() and parse() — this test enforces that invariant.
    #[test]
    fn all_command_names_round_trip_through_parse_and_as_str() {
        let all_commands = [
            CommandName::Handshake,
            CommandName::UpgradeCheck,
            CommandName::BlockerDiagnose,
            CommandName::InteractabilityProbe,
            CommandName::OrchestrationProbe,
            CommandName::OrchestrationTabFrames,
            CommandName::OrchestrationTargetDispatch,
            CommandName::OrchestrationWorkflowSourceVars,
            CommandName::TriggerFill,
            CommandName::TriggerPipe,
            CommandName::Open,
            CommandName::State,
            CommandName::Observe,
            CommandName::Orchestration,
            CommandName::Inspect,
            CommandName::Find,
            CommandName::Click,
            CommandName::Exec,
            CommandName::Scroll,
            CommandName::Back,
            CommandName::Forward,
            CommandName::Reload,
            CommandName::Screenshot,
            CommandName::Doctor,
            CommandName::Runtime,
            CommandName::Frames,
            CommandName::Frame,
            CommandName::History,
            CommandName::Downloads,
            CommandName::Download,
            CommandName::Storage,
            CommandName::Handoff,
            CommandName::Takeover,
            CommandName::Dialog,
            CommandName::Intercept,
            CommandName::Interference,
            CommandName::Close,
            CommandName::Secret,
            CommandName::Keys,
            CommandName::Type,
            CommandName::Wait,
            CommandName::Tabs,
            CommandName::Trigger,
            CommandName::Switch,
            CommandName::CloseTab,
            CommandName::Get,
            CommandName::Hover,
            CommandName::Cookies,
            CommandName::Upload,
            CommandName::Select,
            CommandName::Fill,
            CommandName::Extract,
            CommandName::Pipe,
        ];

        for command in all_commands {
            assert_eq!(
                CommandName::parse(command.as_str()),
                Some(command),
                "CommandName::{command:?} failed round-trip: as_str()=\"{}\" did not parse back",
                command.as_str()
            );
        }
    }

    #[test]
    fn parse_returns_none_for_unknown_command_strings() {
        assert_eq!(CommandName::parse(""), None);
        assert_eq!(CommandName::parse("unknown"), None);
        assert_eq!(CommandName::parse("Open"), None); // case-sensitive
        assert_eq!(CommandName::parse("CLICK"), None);
        assert_eq!(CommandName::parse("_unknown_internal"), None);
    }

    #[test]
    fn command_metadata_sources_internal_and_post_wait_flags() {
        let handshake = command_metadata(CommandName::Handshake.as_str());
        assert!(handshake.internal);
        assert!(!handshake.supports_post_wait);
        assert!(!handshake.in_process_only);
        assert!(handshake.transport_protocol_compat_exempt);

        let open = command_metadata(CommandName::Open.as_str());
        assert!(!open.internal);
        assert!(open.supports_post_wait);
        assert!(!open.in_process_only);
        assert!(!open.transport_protocol_compat_exempt);

        let click = command_metadata(CommandName::Click.as_str());
        assert!(!click.internal);
        assert!(click.supports_post_wait);
        assert!(!click.in_process_only);
        assert!(!click.transport_protocol_compat_exempt);

        let exec = command_metadata(CommandName::Exec.as_str());
        assert!(!exec.internal);
        assert!(exec.supports_post_wait);
        assert!(!exec.in_process_only);
        assert!(!exec.transport_protocol_compat_exempt);

        let history = command_metadata(CommandName::History.as_str());
        assert!(!history.internal);
        assert!(!history.supports_post_wait);
        assert!(!history.in_process_only);
        assert!(!history.transport_protocol_compat_exempt);

        // Management-plane internal commands are never post-wait capable.
        for cmd in [
            CommandName::Handshake,
            CommandName::UpgradeCheck,
            CommandName::BlockerDiagnose,
        ] {
            let meta = cmd.metadata();
            assert!(meta.internal, "{cmd:?} should be internal");
            assert!(
                !meta.supports_post_wait,
                "{cmd:?} should not support post-wait"
            );
            assert!(
                !meta.in_process_only,
                "{cmd:?} should remain transport-exposed internal"
            );
            assert!(
                meta.transport_protocol_compat_exempt,
                "{cmd:?} should remain transport protocol compatibility exempt"
            );
        }

        for cmd in [
            CommandName::InteractabilityProbe,
            CommandName::FillValidate,
            CommandName::OrchestrationProbe,
            CommandName::OrchestrationTabFrames,
            CommandName::OrchestrationTargetDispatch,
            CommandName::OrchestrationWorkflowSourceVars,
        ] {
            let meta = cmd.metadata();
            assert!(meta.internal, "{cmd:?} should be internal");
            assert!(
                !meta.supports_post_wait,
                "{cmd:?} should not support post-wait"
            );
            assert!(
                !meta.in_process_only,
                "{cmd:?} should remain transport-exposed internal"
            );
            assert!(
                !meta.transport_protocol_compat_exempt,
                "{cmd:?} must not inherit control-plane protocol compatibility exemption"
            );
        }

        for cmd in [CommandName::TriggerFill, CommandName::TriggerPipe] {
            let meta = cmd.metadata();
            assert!(meta.internal, "{cmd:?} should be internal");
            assert!(meta.supports_post_wait, "{cmd:?} should support post-wait");
            assert!(meta.in_process_only, "{cmd:?} should be in-process only");
        }
    }

    #[test]
    fn transport_exposed_internal_command_helper_matches_metadata_contract() {
        assert!(is_transport_exposed_internal_command(
            CommandName::Handshake.as_str()
        ));
        assert!(is_transport_exposed_internal_command(
            CommandName::UpgradeCheck.as_str()
        ));
        assert!(!is_transport_exposed_internal_command(
            CommandName::TriggerFill.as_str()
        ));
        assert!(!is_transport_exposed_internal_command(
            CommandName::Open.as_str()
        ));
    }

    #[test]
    fn transport_protocol_compat_exemption_is_narrowed_to_control_plane_internal_commands() {
        assert!(allows_transport_protocol_compat_exemption(
            CommandName::Handshake.as_str()
        ));
        assert!(allows_transport_protocol_compat_exemption(
            CommandName::UpgradeCheck.as_str()
        ));
        assert!(allows_transport_protocol_compat_exemption(
            CommandName::BlockerDiagnose.as_str()
        ));
        assert!(!allows_transport_protocol_compat_exemption(
            CommandName::FillValidate.as_str()
        ));
        assert!(!allows_transport_protocol_compat_exemption(
            CommandName::OrchestrationTargetDispatch.as_str()
        ));
        assert!(!allows_transport_protocol_compat_exemption(
            CommandName::TriggerFill.as_str()
        ));
    }

    #[test]
    fn missing_request_command_id_is_allowed_only_for_control_plane_compat_commands() {
        assert!(allows_missing_request_command_id(
            CommandName::Handshake.as_str()
        ));
        assert!(allows_missing_request_command_id(
            CommandName::UpgradeCheck.as_str()
        ));
        assert!(allows_missing_request_command_id(
            CommandName::BlockerDiagnose.as_str()
        ));
        assert!(!allows_missing_request_command_id(
            CommandName::FillValidate.as_str()
        ));
        assert!(!allows_missing_request_command_id(
            CommandName::OrchestrationTabFrames.as_str()
        ));
        assert!(!allows_missing_request_command_id(
            CommandName::OrchestrationTargetDispatch.as_str()
        ));
        assert!(!allows_missing_request_command_id(
            CommandName::Open.as_str()
        ));
    }

    #[test]
    fn missing_request_command_id_allowlist_matches_transport_protocol_compat_metadata() {
        let all_commands = [
            CommandName::Handshake,
            CommandName::UpgradeCheck,
            CommandName::BlockerDiagnose,
            CommandName::InteractabilityProbe,
            CommandName::OrchestrationProbe,
            CommandName::OrchestrationTabFrames,
            CommandName::OrchestrationTargetDispatch,
            CommandName::OrchestrationWorkflowSourceVars,
            CommandName::TriggerFill,
            CommandName::TriggerPipe,
            CommandName::Open,
            CommandName::State,
            CommandName::Observe,
            CommandName::Orchestration,
            CommandName::Inspect,
            CommandName::Find,
            CommandName::Click,
            CommandName::Exec,
            CommandName::Scroll,
            CommandName::Back,
            CommandName::Forward,
            CommandName::Reload,
            CommandName::Screenshot,
            CommandName::Doctor,
            CommandName::Runtime,
            CommandName::Frames,
            CommandName::Frame,
            CommandName::History,
            CommandName::Downloads,
            CommandName::Download,
            CommandName::Storage,
            CommandName::Handoff,
            CommandName::Takeover,
            CommandName::Dialog,
            CommandName::Intercept,
            CommandName::Interference,
            CommandName::Close,
            CommandName::Secret,
            CommandName::Keys,
            CommandName::Type,
            CommandName::Wait,
            CommandName::Tabs,
            CommandName::Trigger,
            CommandName::Switch,
            CommandName::CloseTab,
            CommandName::Get,
            CommandName::Hover,
            CommandName::Cookies,
            CommandName::Upload,
            CommandName::Select,
            CommandName::Fill,
            CommandName::Extract,
            CommandName::Pipe,
        ];

        for command in all_commands {
            let metadata = command.metadata();
            let should_allow_missing_command_id = metadata.internal
                && !metadata.in_process_only
                && metadata.transport_protocol_compat_exempt;
            assert_eq!(
                allows_missing_request_command_id(command.as_str()),
                should_allow_missing_command_id,
                "missing request command_id allowlist drifted from transport compat metadata for {:?}",
                command
            );
        }
    }
}
