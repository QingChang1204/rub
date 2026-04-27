use rub_daemon::session::{RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompatibilityDegradedOwnedReason {
    ProtocolIncompatible,
    HardCutReleasePending,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) struct CompatibilityDegradedOwnedSession {
    pub(crate) session: String,
    pub(crate) daemon_session_id: String,
    pub(crate) reason: CompatibilityDegradedOwnedReason,
}

pub(crate) fn compatibility_degraded_owned_reason(
    liveness: RegistryEntryLiveness,
) -> Option<CompatibilityDegradedOwnedReason> {
    match liveness {
        RegistryEntryLiveness::ProtocolIncompatible => {
            Some(CompatibilityDegradedOwnedReason::ProtocolIncompatible)
        }
        RegistryEntryLiveness::HardCutReleasePending => {
            Some(CompatibilityDegradedOwnedReason::HardCutReleasePending)
        }
        _ => None,
    }
}

pub(crate) fn compatibility_degraded_owned_from_entry(
    entry: &RegistryEntry,
    liveness: RegistryEntryLiveness,
) -> Option<CompatibilityDegradedOwnedSession> {
    Some(CompatibilityDegradedOwnedSession {
        session: entry.session_name.clone(),
        daemon_session_id: entry.session_id.clone(),
        reason: compatibility_degraded_owned_reason(liveness)?,
    })
}

pub(crate) fn compatibility_degraded_owned_from_snapshot(
    snapshot: &RegistryEntrySnapshot,
) -> Option<CompatibilityDegradedOwnedSession> {
    compatibility_degraded_owned_from_entry(&snapshot.entry, snapshot.liveness)
}
