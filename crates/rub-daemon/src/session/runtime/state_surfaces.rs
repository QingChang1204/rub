use super::*;

impl SessionState {
    /// Current session-scoped runtime observability projection.
    pub async fn frame_runtime(&self) -> FrameRuntimeInfo {
        self.frame_runtime.read().await.projection()
    }

    /// Current session-scoped storage runtime projection.
    pub async fn storage_runtime(&self) -> StorageRuntimeInfo {
        self.storage_runtime.read().await.projection()
    }

    /// Replace the current storage runtime snapshot from the live browser authority.
    pub async fn set_storage_snapshot(&self, snapshot: StorageSnapshot) -> StorageRuntimeInfo {
        self.storage_runtime
            .write()
            .await
            .replace_snapshot(snapshot)
    }

    /// Record one storage mutation in the session-scoped mutation ledger.
    pub async fn record_storage_mutation(
        &self,
        kind: StorageMutationKind,
        origin: String,
        area: Option<StorageArea>,
        key: Option<String>,
    ) -> StorageRuntimeInfo {
        self.storage_runtime
            .write()
            .await
            .record_mutation(kind, origin, area, key)
    }

    /// Mark the storage runtime surface as degraded when the live probe cannot run reliably.
    pub async fn mark_storage_runtime_degraded(&self, reason: impl Into<String>) {
        self.storage_runtime.write().await.mark_degraded(reason);
    }

    /// Current selected frame authority (`None` = top/primary frame).
    pub async fn selected_frame_id(&self) -> Option<String> {
        self.frame_runtime.read().await.selected_frame_id()
    }

    /// Replace the selected frame authority (`None` = top/primary frame).
    pub async fn select_frame(&self, frame_id: Option<String>) {
        self.frame_runtime.write().await.select_frame(frame_id);
    }

    /// Replace the current frame runtime projection.
    pub async fn set_frame_runtime(&self, runtime: FrameRuntimeInfo) {
        self.frame_runtime.write().await.replace(runtime);
    }

    /// Rebuild the frame runtime projection from the current live inventory.
    pub async fn apply_frame_inventory(&self, inventory: &[FrameInventoryEntry]) {
        self.frame_runtime.write().await.apply_inventory(inventory);
    }

    /// Overlay session-scoped current/primary markers onto the live frame inventory.
    pub async fn project_frame_inventory(
        &self,
        inventory: &[FrameInventoryEntry],
    ) -> Vec<FrameInventoryEntry> {
        self.frame_runtime.read().await.project_inventory(inventory)
    }

    /// Mark the frame runtime surface as degraded when the live frame probe fails.
    pub async fn mark_frame_runtime_degraded(&self, reason: impl Into<String>) {
        self.frame_runtime.write().await.mark_degraded(reason);
    }

    pub fn allocate_runtime_state_sequence(&self) -> u64 {
        self.next_runtime_state_sequence
            .fetch_add(1, Ordering::SeqCst)
    }

    pub async fn runtime_state_snapshot(&self) -> RuntimeStateSnapshot {
        self.runtime_state.read().await.snapshot()
    }

    /// Current session-scoped auth/storage observability projection.
    pub async fn state_inspector(&self) -> StateInspectorInfo {
        self.runtime_state.read().await.state_inspector()
    }

    /// Current session-scoped readiness heuristics projection.
    pub async fn readiness_state(&self) -> ReadinessInfo {
        self.runtime_state.read().await.readiness()
    }

    /// Replace the current runtime-state projection atomically.
    pub async fn publish_runtime_state_snapshot(
        &self,
        sequence: u64,
        snapshot: RuntimeStateSnapshot,
    ) {
        self.runtime_state.write().await.replace(sequence, snapshot);
    }

    /// Mark both runtime-state surfaces as degraded from a shared live-probe failure.
    pub async fn mark_runtime_state_probe_degraded(&self, sequence: u64, reason: impl AsRef<str>) {
        let reason = format!("live_probe_failed:{}", reason.as_ref());
        self.runtime_state
            .write()
            .await
            .mark_degraded(sequence, reason);
    }
}
