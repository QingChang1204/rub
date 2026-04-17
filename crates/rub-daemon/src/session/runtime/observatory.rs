use std::sync::Arc;

use super::*;
use rub_core::model::ObservedNetworkRequestRecord;

struct ObservatoryDropCounts {
    total: u64,
    request: u64,
}

impl SessionState {
    fn observatory_drop_counts(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
    ) -> ObservatoryDropCounts {
        let timeline = self
            .observatory_ingress_drop_count()
            .saturating_add(observatory.dropped_timeline_event_count());
        let request = self
            .network_request_ingress_drop_count()
            .saturating_add(observatory.dropped_request_record_count());
        ObservatoryDropCounts {
            total: timeline.saturating_add(request),
            request,
        }
    }

    pub(super) fn projected_observatory(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
    ) -> RuntimeObservatoryInfo {
        observatory.projection_with_drop_count(self.observatory_drop_counts(observatory).total)
    }

    fn observatory_event_window_between_from_state(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
        cursor: u64,
        end_cursor: u64,
        last_observed_drop_count: u64,
        observed_drop_count: u64,
    ) -> crate::observatory::ObservatoryEventWindow {
        observatory.event_window_between(
            cursor,
            end_cursor,
            observed_drop_count,
            last_observed_drop_count,
        )
    }

    fn observatory_request_window_from_state(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        observatory.request_window_after(
            cursor,
            self.observatory_drop_counts(observatory).request,
            last_observed_drop_count,
        )
    }

    fn observatory_request_window_between_from_state(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
        cursor: u64,
        end_cursor: u64,
        last_observed_drop_count: u64,
        observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        observatory.request_window_between(
            cursor,
            end_cursor,
            observed_drop_count,
            last_observed_drop_count,
        )
    }

    /// Current session-scoped runtime observability projection.
    pub async fn observatory(&self) -> RuntimeObservatoryInfo {
        let observatory = self.observatory.read().await;
        self.projected_observatory(&observatory)
    }

    /// Return the current observatory cursor for later event-window correlation.
    pub async fn observatory_cursor(&self) -> u64 {
        self.observatory.read().await.cursor()
    }

    /// Return the current network-request cursor for later request-window correlation.
    pub async fn network_request_cursor(&self) -> u64 {
        self.observatory.read().await.request_cursor()
    }

    /// Return sequenced observatory events recorded after the given cursor.
    pub async fn observatory_events_after(&self, cursor: u64) -> Vec<RuntimeObservatoryEvent> {
        self.observatory.read().await.events_after(cursor)
    }

    pub(crate) async fn observatory_event_window_between(
        &self,
        cursor: u64,
        end_cursor: u64,
        last_observed_drop_count: u64,
        observed_drop_count: u64,
    ) -> crate::observatory::ObservatoryEventWindow {
        let observatory = self.observatory.read().await;
        self.observatory_event_window_between_from_state(
            &observatory,
            cursor,
            end_cursor,
            last_observed_drop_count,
            observed_drop_count,
        )
    }

    /// Record a browser console error into the observability ring buffer.
    pub async fn record_console_error(&self, event: ConsoleErrorEvent) {
        self.observatory.write().await.push_console_error(event);
    }

    /// Record a page-level error into the observability ring buffer.
    pub async fn record_page_error(&self, event: PageErrorEvent) {
        self.observatory.write().await.push_page_error(event);
    }

    /// Record a failed network request into the observability ring buffer.
    pub async fn record_network_failure(&self, event: NetworkFailureEvent) {
        self.observatory.write().await.push_network_failure(event);
    }

    /// Record a request summary into the observability ring buffer.
    pub async fn record_request_summary(&self, event: RequestSummaryEvent) {
        self.observatory.write().await.push_request(event);
    }

    /// Upsert a detailed request lifecycle record into the network inspection registry.
    #[cfg(test)]
    pub async fn upsert_network_request_record(&self, record: NetworkRequestRecord) {
        self.observatory.write().await.upsert_request_record(record);
        self.network_request_notify.notify_waiters();
    }

    /// Upsert a pre-authority request observation and let the observatory assign sequence truth.
    pub async fn upsert_observed_network_request_record(
        &self,
        record: ObservedNetworkRequestRecord,
    ) {
        self.observatory
            .write()
            .await
            .upsert_observed_request_record(record);
        self.network_request_notify.notify_waiters();
    }

    /// Return a bounded view of recent network request records.
    pub async fn network_request_records(
        &self,
        last: Option<usize>,
        url_match: Option<&str>,
        method: Option<&str>,
        status: Option<u16>,
        lifecycle: Option<NetworkRequestLifecycle>,
    ) -> Vec<NetworkRequestRecord> {
        self.observatory
            .read()
            .await
            .request_records(last, url_match, method, status, lifecycle)
    }

    /// Return a single network request record by request identifier.
    pub async fn network_request_record(&self, request_id: &str) -> Option<NetworkRequestRecord> {
        self.observatory.read().await.request_record(request_id)
    }

    /// Return request lifecycle records first observed after the given cursor.
    pub async fn network_request_records_after(&self, cursor: u64) -> Vec<NetworkRequestRecord> {
        self.observatory.read().await.request_records_after(cursor)
    }

    pub async fn network_request_drop_count(&self) -> u64 {
        let observatory = self.observatory.read().await;
        self.observatory_drop_counts(&observatory).request
    }

    pub(crate) async fn network_request_window_after(
        &self,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        let observatory = self.observatory.read().await;
        self.observatory_request_window_from_state(&observatory, cursor, last_observed_drop_count)
    }

    pub(crate) async fn network_request_window_between(
        &self,
        cursor: u64,
        end_cursor: u64,
        last_observed_drop_count: u64,
        observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        let observatory = self.observatory.read().await;
        self.observatory_request_window_between_from_state(
            &observatory,
            cursor,
            end_cursor,
            last_observed_drop_count,
            observed_drop_count,
        )
    }

    /// Mark the runtime observability surface as degraded.
    pub async fn mark_observatory_degraded(&self, reason: impl Into<String>) {
        self.observatory.write().await.mark_degraded(reason);
        self.network_request_notify.notify_waiters();
    }

    pub async fn mark_observatory_ready(&self) {
        self.observatory.write().await.mark_ready();
    }

    /// Shared notification channel for new network-request record commits.
    pub fn network_request_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.network_request_notify.clone()
    }
}
