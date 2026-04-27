use std::sync::Arc;

mod install;
mod runtime;

pub(crate) use self::runtime::{
    ProjectionContext, ensure_page_hooks, replay_runtime_state_for_committed_active_page,
    sync_tabs_projection_with,
};

/// Callback type for CDP event-driven epoch increments (INV-001 Source B).
pub(crate) type EpochCallback = Arc<dyn Fn(Option<&str>) + Send + Sync>;

pub(super) enum PageHookResult<T, E> {
    Completed(Result<T, E>),
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageHookFlag {
    EnvironmentMetrics,
    TouchEmulation,
    StealthNewDocument,
    StealthLive,
    UserAgent,
    SelfProbe,
    DomEnable,
    RuntimeProbe,
    FrameListener,
    DocumentListener,
    Observatory,
    Dialogs,
    NetworkRules,
}

impl PageHookFlag {
    const fn bit(self) -> u16 {
        1 << match self {
            Self::EnvironmentMetrics => 0,
            Self::TouchEmulation => 1,
            Self::StealthNewDocument => 2,
            Self::StealthLive => 3,
            Self::UserAgent => 4,
            Self::SelfProbe => 5,
            Self::DomEnable => 6,
            Self::RuntimeProbe => 7,
            Self::FrameListener => 8,
            Self::DocumentListener => 9,
            Self::Observatory => 10,
            Self::Dialogs => 11,
            Self::NetworkRules => 12,
        }
    }
}

const IDENTITY_HOOKS_MASK: u16 = PageHookFlag::EnvironmentMetrics.bit()
    | PageHookFlag::TouchEmulation.bit()
    | PageHookFlag::StealthNewDocument.bit()
    | PageHookFlag::StealthLive.bit()
    | PageHookFlag::UserAgent.bit();
pub(super) const CRITICAL_RUNTIME_HOOKS_MASK: u16 = PageHookFlag::SelfProbe.bit()
    | PageHookFlag::DomEnable.bit()
    | PageHookFlag::RuntimeProbe.bit()
    | PageHookFlag::FrameListener.bit()
    | PageHookFlag::DocumentListener.bit()
    | PageHookFlag::Observatory.bit()
    | PageHookFlag::Dialogs.bit()
    | PageHookFlag::NetworkRules.bit();
pub(super) const BACKGROUND_DIALOG_HOOK_MASK: u16 = PageHookFlag::Dialogs.bit();
pub(super) const BACKGROUND_OBSERVATORY_HOOK_MASK: u16 = PageHookFlag::Observatory.bit();
pub(super) const BACKGROUND_NETWORK_RULES_HOOK_MASK: u16 = PageHookFlag::NetworkRules.bit();
pub(super) const RUNTIME_CALLBACK_HOOKS_MASK: u16 = PageHookFlag::RuntimeProbe.bit()
    | PageHookFlag::FrameListener.bit()
    | PageHookFlag::DocumentListener.bit()
    | PageHookFlag::Observatory.bit()
    | PageHookFlag::Dialogs.bit()
    | PageHookFlag::NetworkRules.bit();
pub(super) const ALL_PAGE_HOOKS_MASK: u16 = IDENTITY_HOOKS_MASK | CRITICAL_RUNTIME_HOOKS_MASK;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PageHookInstallState {
    pub(super) installing: bool,
    pub(super) installation_recorded: bool,
    pub(super) hook_bits: u16,
}

impl PageHookInstallState {
    pub(super) fn complete(&self) -> bool {
        self.contains_all(ALL_PAGE_HOOKS_MASK)
    }

    pub(super) fn contains(&self, flag: PageHookFlag) -> bool {
        self.hook_bits & flag.bit() != 0
    }

    pub(super) fn contains_all(&self, mask: u16) -> bool {
        self.hook_bits & mask == mask
    }

    pub(super) fn mark(&mut self, flag: PageHookFlag) {
        self.hook_bits |= flag.bit();
    }

    pub(super) fn mark_all(&mut self, mask: u16) {
        self.hook_bits |= mask;
    }

    pub(super) fn clear_all(&mut self, mask: u16) {
        self.hook_bits &= !mask;
    }

    pub(crate) fn invalidate_runtime_callback_hooks(&mut self) {
        self.installing = false;
        self.clear_all(RUNTIME_CALLBACK_HOOKS_MASK);
    }

    #[cfg(test)]
    pub(crate) fn completed_runtime_callback_hooks_for_test() -> Self {
        let mut state = Self::default();
        state.mark_all(RUNTIME_CALLBACK_HOOKS_MASK);
        state
    }

    #[cfg(test)]
    pub(crate) fn runtime_callback_hooks_cleared_for_test(&self) -> bool {
        self.hook_bits & RUNTIME_CALLBACK_HOOKS_MASK == 0
    }
}

pub(super) fn required_runtime_hooks_commit_ready(
    state: &PageHookInstallState,
    required_mask: u16,
    required_failure_mask: u16,
) -> bool {
    state.contains_all(required_mask) && required_failure_mask & required_mask == 0
}

pub(super) fn user_agent_protocol_override_succeeded<T, E>(result: &PageHookResult<T, E>) -> bool {
    matches!(result, PageHookResult::Completed(Ok(_)))
}
