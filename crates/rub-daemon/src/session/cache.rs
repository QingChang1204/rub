use super::journal::redacted_post_commit_request;
use super::*;
use tracing::trace;

struct PostCommitProjectionDrainGuard<'a> {
    state: &'a SessionState,
}

impl<'a> PostCommitProjectionDrainGuard<'a> {
    fn new(state: &'a SessionState) -> Self {
        state
            .post_commit_followup_count
            .fetch_add(1, Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for PostCommitProjectionDrainGuard<'_> {
    fn drop(&mut self) {
        self.state
            .post_commit_followup_count
            .fetch_sub(1, Ordering::SeqCst);
    }
}

fn serialized_json_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(rub_ipc::codec::MAX_FRAME_BYTES)
}

fn post_commit_projection_bytes(
    request: &rub_ipc::protocol::IpcRequest,
    response: &rub_ipc::protocol::IpcResponse,
    workflow_capture_response: &rub_ipc::protocol::IpcResponse,
) -> usize {
    serialized_json_len(request)
        .saturating_add(serialized_json_len(response))
        .saturating_add(serialized_json_len(workflow_capture_response))
}

fn trim_post_commit_projection_queue_with_limits(
    queue: &mut PostCommitProjectionQueue,
    max_entries: usize,
    max_bytes: usize,
) {
    while queue.entries.len() > max_entries
        || (queue.total_bytes > max_bytes && queue.entries.len() > 1)
    {
        let Some(evicted) = queue.entries.pop_front() else {
            break;
        };
        queue.total_bytes = queue.total_bytes.saturating_sub(evicted.approx_bytes);
        queue.dropped_before_projection = queue.dropped_before_projection.saturating_add(1);
    }
}

fn trim_post_commit_projection_queue(queue: &mut PostCommitProjectionQueue) {
    trim_post_commit_projection_queue_with_limits(
        queue,
        POST_COMMIT_PROJECTION_LIMIT,
        POST_COMMIT_PROJECTION_LIMIT_BYTES,
    );
}

fn trim_replay_cache_with_limits(
    replay: &mut ReplayProtocolState,
    max_entries: usize,
    max_bytes: usize,
) {
    while replay.cache.len() > max_entries
        || (replay.total_bytes > max_bytes && replay.order.len() > 1)
    {
        let Some(oldest) = replay.order.pop_front() else {
            break;
        };
        if let Some(evicted) = replay.cache.remove(&oldest) {
            replay.total_bytes = replay.total_bytes.saturating_sub(evicted.approx_bytes);
        }
    }
}

fn trim_replay_cache(replay: &mut ReplayProtocolState) {
    trim_replay_cache_with_limits(replay, REPLAY_CACHE_LIMIT, REPLAY_CACHE_LIMIT_BYTES);
}

fn trim_replay_spent_with_limit(replay: &mut ReplayProtocolState, max_entries: usize) {
    if max_entries == usize::MAX {
        return;
    }
    while replay.spent.len() > max_entries {
        let Some(oldest) = replay.spent_order.pop_front() else {
            break;
        };
        replay.spent.remove(&oldest);
    }
}

fn trim_replay_spent(replay: &mut ReplayProtocolState) {
    // A spent command_id is the session-lifetime fallback authority once the
    // cached response has been evicted. Dropping the tombstone would let the
    // same command_id become Owner again and violate at-most-once execution.
    trim_replay_spent_with_limit(replay, usize::MAX);
}

impl SessionState {
    /// Queue a post-commit projection without extending the request authority fence.
    pub fn submit_post_commit_projection(
        &self,
        request: &rub_ipc::protocol::IpcRequest,
        response: &rub_ipc::protocol::IpcResponse,
    ) {
        self.submit_post_commit_projection_with_capture(
            request,
            response,
            response,
            crate::workflow_capture::WorkflowCaptureDeliveryState::Delivered,
        );
    }

    pub fn submit_post_commit_projection_with_capture(
        &self,
        request: &rub_ipc::protocol::IpcRequest,
        response: &rub_ipc::protocol::IpcResponse,
        workflow_capture_response: &rub_ipc::protocol::IpcResponse,
        workflow_capture_delivery_state: crate::workflow_capture::WorkflowCaptureDeliveryState,
    ) {
        let mut queue = self
            .post_commit_projections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let projection = PostCommitProjection {
            request: request.clone(),
            response: response.clone(),
            workflow_capture_response: workflow_capture_response.clone(),
            workflow_capture_delivery_state,
            approx_bytes: post_commit_projection_bytes(
                request,
                response,
                workflow_capture_response,
            ),
        };
        queue.total_bytes = queue.total_bytes.saturating_add(projection.approx_bytes);
        queue.entries.push_back(projection);
        trim_post_commit_projection_queue(&mut queue);
    }

    pub fn pending_post_commit_projection_count(&self) -> usize {
        self.post_commit_projections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }

    pub fn post_commit_projection_drop_count(&self) -> u64 {
        self.post_commit_projections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .dropped_before_projection
    }

    pub fn spawn_post_commit_projection_drain(self: &Arc<Self>) {
        if self
            .post_commit_projection_drain_scheduled
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        #[cfg(test)]
        self.post_commit_projection_drain_spawn_count
            .fetch_add(1, Ordering::SeqCst);
        let state = Arc::clone(self);
        tokio::spawn(async move {
            state.drain_post_commit_projections().await;
        });
    }

    /// Flush pending projections into the bounded history/workflow views.
    pub async fn drain_post_commit_projections(&self) {
        let _drain_guard = PostCommitProjectionDrainGuard::new(self);
        loop {
            let _drain = self.post_commit_projection_drain.lock().await;

            loop {
                let projection = {
                    let mut queue = self
                        .post_commit_projections
                        .lock()
                        .expect("post-commit projection mutex should not be poisoned");
                    let Some(projection) = queue.entries.pop_front() else {
                        break;
                    };
                    queue.total_bytes = queue.total_bytes.saturating_sub(projection.approx_bytes);
                    projection
                };

                self.record_command_history(&projection.request, &projection.response)
                    .await;
                self.record_workflow_capture_with_state(
                    &projection.request,
                    &projection.workflow_capture_response,
                    projection.workflow_capture_delivery_state,
                )
                .await;
            }

            self.post_commit_projection_drain_scheduled
                .store(false, Ordering::SeqCst);

            let queue_empty = self
                .post_commit_projections
                .lock()
                .expect("post-commit projection mutex should not be poisoned")
                .entries
                .is_empty();
            if queue_empty
                || self
                    .post_commit_projection_drain_scheduled
                    .swap(true, Ordering::SeqCst)
            {
                break;
            }
        }
    }

    pub fn claim_replay_command(
        &self,
        command_id: &str,
        fingerprint: String,
    ) -> ReplayCommandClaim {
        let mut replay = self
            .replay
            .lock()
            .expect("replay mutex should not be poisoned");
        if let Some(entry) = replay.cache.get(command_id) {
            if entry.fingerprint == fingerprint {
                return ReplayCommandClaim::Cached(Box::new(entry.response.clone()));
            }
            return ReplayCommandClaim::Conflict;
        }

        if let Some(existing) = replay.in_flight.get(command_id) {
            if existing.fingerprint != fingerprint {
                return ReplayCommandClaim::Conflict;
            }
            return ReplayCommandClaim::Wait(existing.sender.subscribe());
        }

        if let Some(existing) = replay.spent.get(command_id) {
            if existing.fingerprint != fingerprint {
                return ReplayCommandClaim::Conflict;
            }
            return ReplayCommandClaim::SpentWithoutCachedResponse;
        }

        let (sender, _receiver) = tokio::sync::watch::channel(ReplayFenceState::InFlight);
        replay.in_flight.insert(
            command_id.to_string(),
            ReplayInFlightEntry {
                fingerprint,
                sender,
            },
        );
        ReplayCommandClaim::Owner
    }

    pub fn release_replay_command(&self, command_id: &str) {
        let entry = self
            .replay
            .lock()
            .expect("replay mutex should not be poisoned")
            .in_flight
            .remove(command_id);
        if let Some(entry) = entry {
            let _ = entry.sender.send(ReplayFenceState::Released);
        }
    }

    pub fn mark_replay_command_spent(&self, command_id: &str, fingerprint: &str) {
        let mut replay = self
            .replay
            .lock()
            .expect("replay mutex should not be poisoned");
        match replay.spent.get(command_id) {
            Some(existing) => {
                debug_assert_eq!(
                    existing.fingerprint, fingerprint,
                    "spent replay command identity must remain stable for one command_id"
                );
            }
            None => {
                replay.spent.insert(
                    command_id.to_string(),
                    ReplaySpentEntry {
                        fingerprint: fingerprint.to_string(),
                    },
                );
                replay.spent_order.push_back(command_id.to_string());
                trim_replay_spent(&mut replay);
            }
        }
    }

    /// Store a response in the replay cache.
    pub async fn cache_response(
        &self,
        command_id: String,
        fingerprint: String,
        response: rub_ipc::protocol::IpcResponse,
    ) {
        let mut replay = self
            .replay
            .lock()
            .expect("replay mutex should not be poisoned");
        let approx_bytes = serialized_json_len(&response);
        if let Some(previous) = replay.cache.remove(&command_id) {
            replay.total_bytes = replay.total_bytes.saturating_sub(previous.approx_bytes);
        }
        replay.cache.insert(
            command_id.clone(),
            ReplayCacheEntry {
                fingerprint,
                response,
                approx_bytes,
            },
        );
        replay.total_bytes = replay.total_bytes.saturating_add(approx_bytes);

        replay.order.retain(|existing| existing != &command_id);
        replay.order.push_back(command_id);

        trim_replay_cache(&mut replay);
    }

    #[cfg(test)]
    pub(crate) fn evict_cached_replay_response_for_test(&self, command_id: &str) {
        let mut replay = self
            .replay
            .lock()
            .expect("replay mutex should not be poisoned");
        if let Some(previous) = replay.cache.remove(command_id) {
            replay.total_bytes = replay.total_bytes.saturating_sub(previous.approx_bytes);
        }
        replay.order.retain(|existing| existing != command_id);
    }

    pub async fn record_command_history(
        &self,
        request: &rub_ipc::protocol::IpcRequest,
        response: &rub_ipc::protocol::IpcResponse,
    ) {
        self.history.write().await.record(request, response);
    }

    pub async fn command_history(&self, last: usize) -> CommandHistoryProjection {
        self.drain_post_commit_projections().await;
        self.history
            .read()
            .await
            .projection(last, self.post_commit_projection_drop_count())
    }

    pub async fn command_history_range(
        &self,
        from: Option<u64>,
        to: Option<u64>,
    ) -> CommandHistoryProjection {
        self.drain_post_commit_projections().await;
        self.history.read().await.projection_range(
            from,
            to,
            self.post_commit_projection_drop_count(),
        )
    }

    pub fn record_observatory_ingress_overflow(&self) -> u64 {
        self.observatory_drop_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn observatory_ingress_drop_count(&self) -> u64 {
        self.observatory_drop_count.load(Ordering::SeqCst)
    }

    pub fn record_network_request_ingress_overflow(&self) -> u64 {
        self.network_request_ingress_drop_count
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    pub fn network_request_ingress_drop_count(&self) -> u64 {
        self.network_request_ingress_drop_count
            .load(Ordering::SeqCst)
    }

    pub fn record_browser_event_ingress_overflow(&self) -> u64 {
        self.browser_event_ingress_drop_count
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    pub fn browser_event_ingress_drop_count(&self) -> u64 {
        self.browser_event_ingress_drop_count.load(Ordering::SeqCst)
    }

    pub async fn record_workflow_capture(
        &self,
        request: &rub_ipc::protocol::IpcRequest,
        response: &rub_ipc::protocol::IpcResponse,
    ) {
        self.record_workflow_capture_with_state(
            request,
            response,
            crate::workflow_capture::WorkflowCaptureDeliveryState::Delivered,
        )
        .await;
    }

    pub async fn record_workflow_capture_with_state(
        &self,
        request: &rub_ipc::protocol::IpcRequest,
        response: &rub_ipc::protocol::IpcResponse,
        delivery_state: crate::workflow_capture::WorkflowCaptureDeliveryState,
    ) {
        let captured_request = redacted_post_commit_request(request, &self.rub_home);
        self.workflow_capture
            .write()
            .await
            .record(&captured_request, response, delivery_state);
    }

    pub async fn workflow_capture(&self, last: usize) -> WorkflowCaptureProjection {
        self.drain_post_commit_projections().await;
        self.workflow_capture
            .read()
            .await
            .projection(last, self.post_commit_projection_drop_count())
    }

    pub async fn lookup_locator_memo(&self, key: &str) -> Option<Vec<LocatorMemoTarget>> {
        self.locator_memo.read().await.get(key)
    }

    pub async fn record_locator_memo(&self, key: String, targets: Vec<LocatorMemoTarget>) {
        self.locator_memo.write().await.insert(key, targets);
    }

    /// Store a snapshot for later interaction validation.
    pub async fn cache_snapshot(&self, snapshot: Snapshot) -> Arc<Snapshot> {
        let snapshot = Arc::new(snapshot);
        let snapshot_id = snapshot.snapshot_id.clone();
        let mut sc = self.snapshot_cache.write().await;
        // Deduplicate in order (same ID re-inserted = move to back)
        sc.order.retain(|existing| existing != &snapshot_id);
        sc.map.insert(snapshot_id.clone(), Arc::clone(&snapshot));
        sc.order.push_back(snapshot_id);
        // Evict oldest when over limit
        while sc.order.len() > SNAPSHOT_CACHE_LIMIT {
            if let Some(oldest) = sc.order.pop_front() {
                sc.map.remove(&oldest);
            }
        }
        snapshot
    }

    /// Fetch a previously published snapshot.
    pub async fn get_snapshot(&self, snapshot_id: &str) -> Option<Arc<Snapshot>> {
        let result = self
            .snapshot_cache
            .read()
            .await
            .map
            .get(snapshot_id)
            .cloned();
        trace!(
            snapshot_id,
            cache_hit = result.is_some(),
            "snapshot_cache.get"
        );
        result
    }

    /// Drop all cached snapshots after an authority fence such as tab switch/close.
    pub async fn clear_all_snapshots(&self) {
        let mut sc = self.snapshot_cache.write().await;
        sc.map.clear();
        sc.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PostCommitProjection, PostCommitProjectionQueue, ReplayCacheEntry, ReplayProtocolState,
        ReplaySpentEntry, trim_post_commit_projection_queue_with_limits,
        trim_replay_cache_with_limits, trim_replay_spent_with_limit,
    };
    use crate::workflow_capture::WorkflowCaptureDeliveryState;
    use rub_ipc::protocol::{IpcRequest, IpcResponse};
    use std::collections::{HashMap, VecDeque};

    fn test_projection(
        command: &str,
        request_id: &str,
        approx_bytes: usize,
    ) -> PostCommitProjection {
        PostCommitProjection {
            request: IpcRequest::new(command, serde_json::json!({}), 1_000),
            response: IpcResponse::success(request_id, serde_json::json!({})),
            workflow_capture_response: IpcResponse::success(request_id, serde_json::json!({})),
            workflow_capture_delivery_state: WorkflowCaptureDeliveryState::Delivered,
            approx_bytes,
        }
    }

    #[test]
    fn post_commit_projection_queue_enforces_byte_limit() {
        let mut queue = PostCommitProjectionQueue::default();
        queue.entries.push_back(test_projection("open", "req-1", 6));
        queue.total_bytes += 6;
        queue
            .entries
            .push_back(test_projection("click", "req-2", 6));
        queue.total_bytes += 6;

        trim_post_commit_projection_queue_with_limits(&mut queue, 10, 8);

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.total_bytes, 6);
        assert_eq!(queue.entries[0].response.request_id, "req-2");
    }

    #[test]
    fn replay_cache_enforces_byte_limit() {
        let mut replay = ReplayProtocolState {
            cache: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashMap::new(),
            spent: HashMap::new(),
            spent_order: VecDeque::new(),
            total_bytes: 0,
        };
        replay.cache.insert(
            "cmd-1".to_string(),
            ReplayCacheEntry {
                fingerprint: "fp-1".to_string(),
                response: IpcResponse::success("req-1", serde_json::json!({})),
                approx_bytes: 6,
            },
        );
        replay.order.push_back("cmd-1".to_string());
        replay.total_bytes += 6;

        replay.cache.insert(
            "cmd-2".to_string(),
            ReplayCacheEntry {
                fingerprint: "fp-2".to_string(),
                response: IpcResponse::success("req-2", serde_json::json!({})),
                approx_bytes: 6,
            },
        );
        replay.order.push_back("cmd-2".to_string());
        replay.total_bytes += 6;

        trim_replay_cache_with_limits(&mut replay, 10, 8);

        assert_eq!(replay.cache.len(), 1);
        assert_eq!(replay.total_bytes, 6);
        assert!(replay.cache.contains_key("cmd-2"));
        assert!(!replay.cache.contains_key("cmd-1"));
    }

    #[test]
    fn replay_cache_eviction_does_not_clear_spent_command_authority() {
        let mut replay = ReplayProtocolState {
            cache: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashMap::new(),
            spent: HashMap::from([(
                "cmd-1".to_string(),
                ReplaySpentEntry {
                    fingerprint: "fp-1".to_string(),
                },
            )]),
            spent_order: VecDeque::from(["cmd-1".to_string()]),
            total_bytes: 0,
        };
        replay.cache.insert(
            "cmd-1".to_string(),
            ReplayCacheEntry {
                fingerprint: "fp-1".to_string(),
                response: IpcResponse::success("req-1", serde_json::json!({})),
                approx_bytes: 6,
            },
        );
        replay.order.push_back("cmd-1".to_string());
        replay.total_bytes += 6;
        replay.cache.insert(
            "cmd-2".to_string(),
            ReplayCacheEntry {
                fingerprint: "fp-2".to_string(),
                response: IpcResponse::success("req-2", serde_json::json!({})),
                approx_bytes: 6,
            },
        );
        replay.order.push_back("cmd-2".to_string());
        replay.total_bytes += 6;

        trim_replay_cache_with_limits(&mut replay, 10, 8);

        assert!(replay.spent.contains_key("cmd-1"));
        assert!(!replay.cache.contains_key("cmd-1"));
    }

    #[test]
    fn replay_spent_budget_evicts_oldest_spent_authority() {
        let mut replay = ReplayProtocolState {
            cache: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashMap::new(),
            spent: HashMap::from([
                (
                    "cmd-1".to_string(),
                    ReplaySpentEntry {
                        fingerprint: "fp-1".to_string(),
                    },
                ),
                (
                    "cmd-2".to_string(),
                    ReplaySpentEntry {
                        fingerprint: "fp-2".to_string(),
                    },
                ),
                (
                    "cmd-3".to_string(),
                    ReplaySpentEntry {
                        fingerprint: "fp-3".to_string(),
                    },
                ),
            ]),
            spent_order: VecDeque::from([
                "cmd-1".to_string(),
                "cmd-2".to_string(),
                "cmd-3".to_string(),
            ]),
            total_bytes: 0,
        };

        trim_replay_spent_with_limit(&mut replay, 2);

        assert!(!replay.spent.contains_key("cmd-1"));
        assert!(replay.spent.contains_key("cmd-2"));
        assert!(replay.spent.contains_key("cmd-3"));
        assert_eq!(
            replay.spent_order,
            VecDeque::from(["cmd-2".to_string(), "cmd-3".to_string()])
        );
    }
}
