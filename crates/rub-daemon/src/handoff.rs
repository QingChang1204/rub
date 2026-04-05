use rub_core::model::{HumanVerificationHandoffInfo, HumanVerificationHandoffStatus};

/// Session-scoped handoff state machine for human verification takeover.
#[derive(Debug, Default)]
pub struct HumanVerificationHandoffState {
    projection: HumanVerificationHandoffInfo,
}

impl HumanVerificationHandoffState {
    pub fn projection(&self) -> HumanVerificationHandoffInfo {
        self.projection.clone()
    }

    pub fn is_ready(&self) -> bool {
        !matches!(
            self.projection.status,
            HumanVerificationHandoffStatus::Unavailable
        )
    }

    pub fn replace(&mut self, projection: HumanVerificationHandoffInfo) {
        self.projection = projection;
    }

    pub fn set_available(&mut self, resume_supported: bool) {
        self.projection = HumanVerificationHandoffInfo {
            status: HumanVerificationHandoffStatus::Available,
            automation_paused: false,
            resume_supported,
            unavailable_reason: None,
        };
    }

    pub fn activate(&mut self) {
        self.projection.status = HumanVerificationHandoffStatus::Active;
        self.projection.automation_paused = true;
        self.projection.unavailable_reason = None;
    }

    pub fn complete(&mut self) {
        self.projection.status = HumanVerificationHandoffStatus::Completed;
        self.projection.automation_paused = false;
        self.projection.resume_supported = true;
        self.projection.unavailable_reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::HumanVerificationHandoffState;
    use rub_core::model::HumanVerificationHandoffStatus;

    #[test]
    fn handoff_state_machine_projects_explicit_transitions() {
        let mut state = HumanVerificationHandoffState::default();
        assert_eq!(
            state.projection().status,
            HumanVerificationHandoffStatus::Unavailable
        );
        assert!(!state.is_ready());

        state.set_available(true);
        assert_eq!(
            state.projection().status,
            HumanVerificationHandoffStatus::Available
        );
        assert!(state.is_ready());

        state.activate();
        let active = state.projection();
        assert_eq!(active.status, HumanVerificationHandoffStatus::Active);
        assert!(active.automation_paused);

        state.complete();
        let completed = state.projection();
        assert_eq!(completed.status, HumanVerificationHandoffStatus::Completed);
        assert!(!completed.automation_paused);
        assert!(completed.resume_supported);
    }
}
