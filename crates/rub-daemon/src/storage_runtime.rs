use std::collections::VecDeque;

use rub_core::storage::{
    StorageArea, StorageMutationKind, StorageMutationRecord, StorageRuntimeInfo,
    StorageRuntimeStatus, StorageSnapshot,
};

const STORAGE_MUTATION_LIMIT: usize = 64;

/// Session-scoped storage runtime authority.
#[derive(Debug, Default)]
pub struct StorageRuntimeState {
    next_sequence: u64,
    projection: StorageRuntimeInfo,
    recent_mutations: VecDeque<StorageMutationRecord>,
}

impl StorageRuntimeState {
    pub fn projection(&self) -> StorageRuntimeInfo {
        let mut projection = self.projection.clone();
        projection.recent_mutations = self.recent_mutations.iter().cloned().collect();
        projection
    }

    pub fn replace_snapshot(&mut self, snapshot: StorageSnapshot) -> StorageRuntimeInfo {
        self.projection.status = StorageRuntimeStatus::Active;
        self.projection.current_origin = Some(snapshot.origin);
        self.projection.local_storage_keys = snapshot.local_storage.into_keys().collect();
        self.projection.session_storage_keys = snapshot.session_storage.into_keys().collect();
        self.projection.degraded_reason = None;
        self.projection()
    }

    pub fn record_mutation(
        &mut self,
        kind: StorageMutationKind,
        origin: String,
        area: Option<StorageArea>,
        key: Option<String>,
    ) -> StorageRuntimeInfo {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;
        self.recent_mutations.push_back(StorageMutationRecord {
            sequence,
            kind,
            origin,
            area,
            key,
        });
        while self.recent_mutations.len() > STORAGE_MUTATION_LIMIT {
            self.recent_mutations.pop_front();
        }
        self.projection()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) -> StorageRuntimeInfo {
        self.projection.status = StorageRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        self.projection()
    }
}

#[cfg(test)]
mod tests {
    use super::StorageRuntimeState;
    use rub_core::storage::{
        StorageArea, StorageMutationKind, StorageRuntimeStatus, StorageSnapshot,
    };
    use std::collections::BTreeMap;

    #[test]
    fn storage_runtime_state_tracks_snapshot_and_mutations() {
        let mut state = StorageRuntimeState::default();
        let snapshot = state.replace_snapshot(StorageSnapshot {
            origin: "https://example.test".to_string(),
            local_storage: BTreeMap::from([("token".to_string(), "abc".to_string())]),
            session_storage: BTreeMap::from([("csrf".to_string(), "def".to_string())]),
        });

        assert_eq!(snapshot.status, StorageRuntimeStatus::Active);
        assert_eq!(
            snapshot.current_origin.as_deref(),
            Some("https://example.test")
        );
        assert_eq!(snapshot.local_storage_keys, vec!["token"]);
        assert_eq!(snapshot.session_storage_keys, vec!["csrf"]);

        let projection = state.record_mutation(
            StorageMutationKind::Set,
            "https://example.test".to_string(),
            Some(StorageArea::Local),
            Some("token".to_string()),
        );
        assert_eq!(projection.recent_mutations.len(), 1);
        assert_eq!(projection.recent_mutations[0].sequence, 1);
        assert_eq!(
            projection.recent_mutations[0].area,
            Some(StorageArea::Local)
        );
    }

    #[test]
    fn storage_runtime_state_can_mark_degraded() {
        let mut state = StorageRuntimeState::default();
        let projection = state.mark_degraded("storage_probe_failed:opaque_origin");

        assert_eq!(projection.status, StorageRuntimeStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("storage_probe_failed:opaque_origin")
        );
    }
}
