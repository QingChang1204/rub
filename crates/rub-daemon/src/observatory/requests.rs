use rub_core::model::{
    NetworkRequestLifecycle, NetworkRequestRecord, ObservedNetworkRequestRecord,
};

use super::{NetworkRequestWindow, OBSERVATORY_REQUEST_RECORD_LIMIT, RuntimeObservatoryState};

impl RuntimeObservatoryState {
    pub fn upsert_observed_request_record(&mut self, record: ObservedNetworkRequestRecord) {
        self.mark_active();
        let next_sequence = self.next_request_sequence();
        if let Some(existing) = self.request_records.get_mut(&record.request_id) {
            let request_id = record.request_id.clone();
            merge_observed_request_record(existing, record);
            existing.sequence = next_sequence;
            if let Some(position) = self.request_order.iter().position(|id| id == &request_id) {
                self.request_order.remove(position);
            }
            self.request_order.push_back(request_id);
            return;
        }

        let request_id = record.request_id.clone();
        let record = authoritative_request_record(record, next_sequence);
        self.request_records.insert(request_id.clone(), record);
        self.request_order.push_back(request_id);
        while self.request_order.len() > OBSERVATORY_REQUEST_RECORD_LIMIT {
            if let Some(oldest) = self.request_order.pop_front() {
                if let Some(evicted) = self.request_records.remove(&oldest) {
                    self.last_evicted_request_sequence =
                        self.last_evicted_request_sequence.max(evicted.sequence);
                }
                self.dropped_request_record_count =
                    self.dropped_request_record_count.saturating_add(1);
            }
        }
    }

    #[cfg(test)]
    pub fn upsert_request_record(&mut self, mut record: NetworkRequestRecord) {
        self.mark_active();
        let next_sequence = self.next_request_sequence();
        if let Some(existing) = self.request_records.get_mut(&record.request_id) {
            let request_id = record.request_id.clone();
            merge_request_record(existing, record);
            existing.sequence = next_sequence;
            if let Some(position) = self.request_order.iter().position(|id| id == &request_id) {
                self.request_order.remove(position);
            }
            self.request_order.push_back(request_id);
            return;
        }

        let request_id = record.request_id.clone();
        record.sequence = next_sequence;
        self.request_records.insert(request_id.clone(), record);
        self.request_order.push_back(request_id);
        while self.request_order.len() > OBSERVATORY_REQUEST_RECORD_LIMIT {
            if let Some(oldest) = self.request_order.pop_front() {
                if let Some(evicted) = self.request_records.remove(&oldest) {
                    self.last_evicted_request_sequence =
                        self.last_evicted_request_sequence.max(evicted.sequence);
                }
                self.dropped_request_record_count =
                    self.dropped_request_record_count.saturating_add(1);
            }
        }
    }

    pub fn request_record(&self, request_id: &str) -> Option<NetworkRequestRecord> {
        self.request_records.get(request_id).cloned()
    }

    pub fn request_records(
        &self,
        last: Option<usize>,
        url_match: Option<&str>,
        method: Option<&str>,
        status: Option<u16>,
        lifecycle: Option<NetworkRequestLifecycle>,
    ) -> Vec<NetworkRequestRecord> {
        let method = method.map(|value| value.to_ascii_uppercase());
        let mut records = self
            .request_order
            .iter()
            .rev()
            .filter_map(|request_id| self.request_records.get(request_id))
            .filter(|record| {
                url_match
                    .map(|needle| record.url.contains(needle))
                    .unwrap_or(true)
                    && method
                        .as_deref()
                        .map(|needle| record.method.eq_ignore_ascii_case(needle))
                        .unwrap_or(true)
                    && status
                        .map(|value| record.status == Some(value))
                        .unwrap_or(true)
                    && lifecycle
                        .map(|value| record.lifecycle == value)
                        .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(last) = last {
            records.truncate(last);
        }
        records
    }

    pub fn request_records_after(&self, cursor: u64) -> Vec<NetworkRequestRecord> {
        self.request_order
            .iter()
            .filter_map(|request_id| self.request_records.get(request_id))
            .filter(|record| record.sequence > cursor)
            .cloned()
            .collect()
    }

    pub fn request_records_between(
        &self,
        cursor: u64,
        end_cursor: u64,
    ) -> Vec<NetworkRequestRecord> {
        self.request_order
            .iter()
            .filter_map(|request_id| self.request_records.get(request_id))
            .filter(|record| record.sequence > cursor && record.sequence <= end_cursor)
            .cloned()
            .collect()
    }

    pub(crate) fn request_window_after(
        &self,
        cursor: u64,
        ingress_drop_count: u64,
        last_observed_ingress_drop_count: u64,
    ) -> NetworkRequestWindow {
        let records = self.request_records_after(cursor);
        let next_cursor = records
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or_else(|| self.request_cursor());
        let cursor_lost_to_eviction = cursor < self.last_evicted_request_sequence;
        let ingress_loss_moved = ingress_drop_count > last_observed_ingress_drop_count;
        let degraded_reason = if cursor_lost_to_eviction {
            Some("network_request_ring_overflow".to_string())
        } else if ingress_loss_moved {
            Some("network_request_ingress_overflow".to_string())
        } else if self
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason != "network_request_ring_overflow")
        {
            self.degraded_reason.clone()
        } else {
            None
        };
        NetworkRequestWindow {
            records,
            next_cursor,
            authoritative: degraded_reason.is_none(),
            degraded_reason,
        }
    }

    pub(crate) fn request_window_between(
        &self,
        cursor: u64,
        end_cursor: u64,
        ingress_drop_count: u64,
        last_observed_ingress_drop_count: u64,
    ) -> NetworkRequestWindow {
        let records = self.request_records_between(cursor, end_cursor);
        let next_cursor = records
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or(end_cursor);
        let cursor_lost_to_eviction = cursor < self.last_evicted_request_sequence;
        let ingress_loss_moved = ingress_drop_count > last_observed_ingress_drop_count;
        let degraded_reason = if cursor_lost_to_eviction {
            Some("network_request_ring_overflow".to_string())
        } else if ingress_loss_moved {
            Some("network_request_ingress_overflow".to_string())
        } else if self
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason != "network_request_ring_overflow")
        {
            self.degraded_reason.clone()
        } else {
            None
        };
        NetworkRequestWindow {
            records,
            next_cursor,
            authoritative: degraded_reason.is_none(),
            degraded_reason,
        }
    }
}

fn authoritative_request_record(
    record: ObservedNetworkRequestRecord,
    sequence: u64,
) -> NetworkRequestRecord {
    NetworkRequestRecord {
        request_id: record.request_id,
        sequence,
        lifecycle: record.lifecycle,
        url: record.url,
        method: record.method,
        tab_target_id: record.tab_target_id,
        status: record.status,
        request_headers: record.request_headers,
        response_headers: record.response_headers,
        request_body: record.request_body,
        response_body: record.response_body,
        original_url: record.original_url,
        rewritten_url: record.rewritten_url,
        applied_rule_effects: record.applied_rule_effects,
        error_text: record.error_text,
        frame_id: record.frame_id,
        resource_type: record.resource_type,
        mime_type: record.mime_type,
    }
}

fn merge_observed_request_record(
    existing: &mut NetworkRequestRecord,
    incoming: ObservedNetworkRequestRecord,
) {
    if request_lifecycle_rank(incoming.lifecycle) >= request_lifecycle_rank(existing.lifecycle) {
        existing.lifecycle = incoming.lifecycle;
    }
    if !incoming.url.is_empty() {
        existing.url = incoming.url;
    }
    if !incoming.method.is_empty() {
        existing.method = incoming.method;
    }
    if incoming.tab_target_id.is_some() {
        existing.tab_target_id = incoming.tab_target_id;
    }
    if incoming.status.is_some() {
        existing.status = incoming.status;
    }
    if !incoming.request_headers.is_empty() {
        existing.request_headers = incoming.request_headers;
    }
    if !incoming.response_headers.is_empty() {
        existing.response_headers = incoming.response_headers;
    }
    if incoming.request_body.is_some() {
        existing.request_body = incoming.request_body;
    }
    if incoming.response_body.is_some() {
        existing.response_body = incoming.response_body;
    }
    if incoming.original_url.is_some() {
        existing.original_url = incoming.original_url;
    }
    if incoming.rewritten_url.is_some() {
        existing.rewritten_url = incoming.rewritten_url;
    }
    if !incoming.applied_rule_effects.is_empty() {
        existing.applied_rule_effects = incoming.applied_rule_effects;
    }
    if incoming.error_text.is_some() {
        existing.error_text = incoming.error_text;
    }
    if incoming.frame_id.is_some() {
        existing.frame_id = incoming.frame_id;
    }
    if incoming.resource_type.is_some() {
        existing.resource_type = incoming.resource_type;
    }
    if incoming.mime_type.is_some() {
        existing.mime_type = incoming.mime_type;
    }
}

#[cfg(test)]
fn merge_request_record(existing: &mut NetworkRequestRecord, incoming: NetworkRequestRecord) {
    if request_lifecycle_rank(incoming.lifecycle) >= request_lifecycle_rank(existing.lifecycle) {
        existing.lifecycle = incoming.lifecycle;
    }
    if !incoming.url.is_empty() {
        existing.url = incoming.url;
    }
    if !incoming.method.is_empty() {
        existing.method = incoming.method;
    }
    if incoming.tab_target_id.is_some() {
        existing.tab_target_id = incoming.tab_target_id;
    }
    if incoming.status.is_some() {
        existing.status = incoming.status;
    }
    if !incoming.request_headers.is_empty() {
        existing.request_headers = incoming.request_headers;
    }
    if !incoming.response_headers.is_empty() {
        existing.response_headers = incoming.response_headers;
    }
    if incoming.request_body.is_some() {
        existing.request_body = incoming.request_body;
    }
    if incoming.response_body.is_some() {
        existing.response_body = incoming.response_body;
    }
    if incoming.original_url.is_some() {
        existing.original_url = incoming.original_url;
    }
    if incoming.rewritten_url.is_some() {
        existing.rewritten_url = incoming.rewritten_url;
    }
    if !incoming.applied_rule_effects.is_empty() {
        existing.applied_rule_effects = incoming.applied_rule_effects;
    }
    if incoming.error_text.is_some() {
        existing.error_text = incoming.error_text;
    }
    if incoming.frame_id.is_some() {
        existing.frame_id = incoming.frame_id;
    }
    if incoming.resource_type.is_some() {
        existing.resource_type = incoming.resource_type;
    }
    if incoming.mime_type.is_some() {
        existing.mime_type = incoming.mime_type;
    }
}

fn request_lifecycle_rank(lifecycle: NetworkRequestLifecycle) -> u8 {
    match lifecycle {
        NetworkRequestLifecycle::Pending => 0,
        NetworkRequestLifecycle::Responded => 1,
        NetworkRequestLifecycle::Completed => 2,
        NetworkRequestLifecycle::Failed => 2,
    }
}
