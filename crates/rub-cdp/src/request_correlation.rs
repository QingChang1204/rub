use rub_core::model::NetworkRuleEffect;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

const CORRELATION_LIMIT: usize = 2_048;
const CORRELATION_TTL: Duration = Duration::from_secs(900);

pub(crate) const CORRELATION_REGISTRY_EVICTED_REASON: &str = "request_correlation_registry_evicted";
pub(crate) const CORRELATION_REGISTRY_TTL_EXPIRED_REASON: &str =
    "request_correlation_registry_ttl_expired";
pub(crate) const CORRELATION_STRICT_FALLBACK_REASON: &str =
    "request_correlation_unresolved_fallback";
pub(crate) const CORRELATION_RELAXED_FALLBACK_REASON: &str = "request_correlation_relaxed_fallback";
pub(crate) const CORRELATION_AMBIGUOUS_FALLBACK_REASON: &str =
    "request_correlation_ambiguous_fallback";
pub(crate) const CORRELATION_BROWSER_AUTHORITY_REBUILD_FAILED_REASON: &str =
    "request_correlation_browser_authority_rebuild_failed";

/// Correlated request metadata produced by network rule execution and consumed
/// by the runtime observatory when the request later resolves or fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCorrelation {
    pub tab_target_id: Option<String>,
    pub original_url: String,
    pub rewritten_url: Option<String>,
    pub effective_request_headers: Option<BTreeMap<String, String>>,
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
}

/// Bounded bridge between Fetch interception and Network observability.
#[derive(Debug, Default, Clone)]
pub struct RequestCorrelationRegistry {
    next_id: u64,
    by_id: HashMap<u64, TimedCorrelation>,
    by_fetch_request_id: HashMap<String, u64>,
    by_network_request_id: HashMap<String, u64>,
    order: VecDeque<u64>,
    unresolved_by_lookup_key: HashMap<UnresolvedLookupKey, Vec<u64>>,
    fallback_metrics: CorrelationFallbackMetrics,
    degraded_reasons: VecDeque<&'static str>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CorrelationFallbackMetrics {
    pub unresolved_fallback_attempts: u64,
    pub unresolved_entries_scanned: u64,
    pub relaxed_match_candidates: u64,
    pub strict_match_candidates: u64,
}

pub(crate) fn normalize_header_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl RequestCorrelationRegistry {
    pub fn record(
        &mut self,
        fetch_request_id: String,
        network_id: Option<String>,
        method: impl Into<String>,
        correlation: RequestCorrelation,
    ) {
        self.record_at(
            fetch_request_id,
            network_id,
            method.into(),
            correlation,
            Instant::now(),
        );
    }

    pub fn peek_for_request(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        tab_target_id: Option<&str>,
    ) -> Option<RequestCorrelation> {
        self.peek_at(
            request_id,
            url,
            method,
            request_headers,
            tab_target_id,
            Instant::now(),
        )
    }

    pub fn take_for_request(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        tab_target_id: Option<&str>,
    ) -> Option<RequestCorrelation> {
        self.take_at(
            request_id,
            url,
            method,
            request_headers,
            tab_target_id,
            Instant::now(),
        )
    }

    pub fn discard_for_fetch_request_id(&mut self, fetch_request_id: &str) -> bool {
        let Some(entry_id) = self.by_fetch_request_id.get(fetch_request_id).copied() else {
            return false;
        };
        self.remove_entry(entry_id).is_some()
    }

    fn record_at(
        &mut self,
        fetch_request_id: String,
        network_id: Option<String>,
        method: String,
        correlation: RequestCorrelation,
        now: Instant,
    ) {
        self.prune(now);
        self.insert_timed(fetch_request_id, network_id, method, correlation, now);
        let mut bounded_evicted = false;
        while self.order.len() > CORRELATION_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.remove_entry(oldest);
                bounded_evicted = true;
            }
        }
        if bounded_evicted {
            self.emit_degraded(CORRELATION_REGISTRY_EVICTED_REASON);
        }
    }

    fn take_at(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        tab_target_id: Option<&str>,
        now: Instant,
    ) -> Option<RequestCorrelation> {
        self.prune(now);
        let entry_id = if let Some(entry_id) = self.direct_entry_id(request_id) {
            Some(entry_id)
        } else {
            self.find_unique_unresolved(url, method, request_headers, tab_target_id)
        };
        entry_id.and_then(|entry_id| self.remove_entry(entry_id))
    }

    fn peek_at(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        tab_target_id: Option<&str>,
        now: Instant,
    ) -> Option<RequestCorrelation> {
        self.prune(now);
        if let Some(entry_id) = self.direct_entry_id(request_id) {
            self.retire_unresolved(entry_id);
            return self
                .by_id
                .get(&entry_id)
                .map(|entry| entry.correlation.clone());
        }

        self.find_unique_unresolved(url, method, request_headers, tab_target_id)
            .and_then(|entry_id| self.by_id.get(&entry_id))
            .map(|entry| entry.correlation.clone())
    }

    fn prune(&mut self, now: Instant) {
        let mut expired_pruned = false;
        let mut bounded_evicted = false;
        while let Some(oldest) = self.order.front().copied() {
            let expired = self
                .by_id
                .get(&oldest)
                .is_none_or(|entry| now.duration_since(entry.recorded_at) > CORRELATION_TTL);
            if !expired && self.order.len() <= CORRELATION_LIMIT {
                break;
            }
            self.order.pop_front();
            self.remove_entry(oldest);
            if expired {
                expired_pruned = true;
            } else {
                bounded_evicted = true;
            }
        }
        if expired_pruned {
            self.emit_degraded(CORRELATION_REGISTRY_TTL_EXPIRED_REASON);
        }
        if bounded_evicted {
            self.emit_degraded(CORRELATION_REGISTRY_EVICTED_REASON);
        }
    }
}

#[derive(Debug, Clone)]
struct TimedCorrelation {
    recorded_at: Instant,
    fetch_request_id: String,
    network_id: Option<String>,
    unresolved_match: Option<UnresolvedCorrelationMatch>,
    unresolved_lookup_key: Option<UnresolvedLookupKey>,
    correlation: RequestCorrelation,
}

impl RequestCorrelationRegistry {
    fn direct_entry_id(&self, request_id: &str) -> Option<u64> {
        self.by_network_request_id
            .get(request_id)
            .copied()
            .or_else(|| self.by_fetch_request_id.get(request_id).copied())
    }

    fn insert_timed(
        &mut self,
        fetch_request_id: String,
        network_id: Option<String>,
        method: String,
        correlation: RequestCorrelation,
        now: Instant,
    ) {
        self.retire_replaced_direct_authority(fetch_request_id.as_str(), network_id.as_deref());

        let entry_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);

        self.by_fetch_request_id
            .insert(fetch_request_id.clone(), entry_id);
        if let Some(network_id) = network_id.as_ref() {
            self.by_network_request_id
                .insert(network_id.clone(), entry_id);
        }

        let unresolved_match = Some(UnresolvedCorrelationMatch {
            expected_url: correlation
                .rewritten_url
                .clone()
                .unwrap_or_else(|| correlation.original_url.clone()),
            expected_method: method,
            effective_request_headers: correlation.effective_request_headers.clone(),
            expected_tab_target_id: correlation.tab_target_id.clone(),
        });
        let unresolved_lookup_key = unresolved_match
            .as_ref()
            .map(UnresolvedLookupKey::from_match);
        if let Some(lookup_key) = unresolved_lookup_key.as_ref() {
            self.unresolved_by_lookup_key
                .entry(lookup_key.clone())
                .or_default()
                .push(entry_id);
        }

        self.by_id.insert(
            entry_id,
            TimedCorrelation {
                recorded_at: now,
                fetch_request_id,
                network_id,
                unresolved_match,
                unresolved_lookup_key,
                correlation,
            },
        );
        self.order.push_back(entry_id);
    }

    fn retire_replaced_direct_authority(
        &mut self,
        fetch_request_id: &str,
        network_id: Option<&str>,
    ) {
        let mut replaced_entry_ids = Vec::new();
        if let Some(entry_id) = self.by_fetch_request_id.get(fetch_request_id).copied() {
            replaced_entry_ids.push(entry_id);
        }
        if let Some(network_id) = network_id
            && let Some(entry_id) = self.by_network_request_id.get(network_id).copied()
            && !replaced_entry_ids.contains(&entry_id)
        {
            replaced_entry_ids.push(entry_id);
        }
        for entry_id in replaced_entry_ids {
            let _ = self.remove_entry(entry_id);
        }
    }

    fn find_unique_unresolved(
        &mut self,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        tab_target_id: Option<&str>,
    ) -> Option<u64> {
        self.fallback_metrics.unresolved_fallback_attempts = self
            .fallback_metrics
            .unresolved_fallback_attempts
            .saturating_add(1);
        let lookup_key = UnresolvedLookupKey::for_request(url, method, tab_target_id);
        let mut strict_match = None;
        let mut strict_ambiguous = false;
        let mut relaxed_match = None;
        let mut relaxed_ambiguous = false;
        let candidate_entry_ids = self
            .unresolved_by_lookup_key
            .get(&lookup_key)
            .cloned()
            .unwrap_or_default();
        for entry_id in candidate_entry_ids {
            let Some(entry) = self.by_id.get(&entry_id) else {
                continue;
            };
            let Some(unresolved_match) = entry.unresolved_match.as_ref() else {
                continue;
            };
            self.fallback_metrics.unresolved_entries_scanned = self
                .fallback_metrics
                .unresolved_entries_scanned
                .saturating_add(1);
            if entry.unresolved_lookup_key.as_ref() != Some(&lookup_key) {
                continue;
            }

            if relaxed_match.is_some() {
                relaxed_ambiguous = true;
            } else {
                relaxed_match = Some(entry_id);
            }
            self.fallback_metrics.relaxed_match_candidates = self
                .fallback_metrics
                .relaxed_match_candidates
                .saturating_add(1);

            let strict_headers_match = match unresolved_match.effective_request_headers.as_ref() {
                Some(expected_headers) => request_headers.is_some_and(|actual_headers| {
                    headers_include(actual_headers, expected_headers)
                }),
                None => true,
            };
            if strict_headers_match {
                self.fallback_metrics.strict_match_candidates = self
                    .fallback_metrics
                    .strict_match_candidates
                    .saturating_add(1);
                if strict_match.is_some() {
                    strict_ambiguous = true;
                } else {
                    strict_match = Some(entry_id);
                }
            }
        }

        if strict_ambiguous {
            self.emit_degraded(CORRELATION_AMBIGUOUS_FALLBACK_REASON);
            return None;
        }
        if let Some(entry_id) = strict_match {
            self.emit_degraded(CORRELATION_STRICT_FALLBACK_REASON);
            return Some(entry_id);
        }
        if relaxed_ambiguous {
            self.emit_degraded(CORRELATION_AMBIGUOUS_FALLBACK_REASON);
            return None;
        }
        if relaxed_match.is_some() {
            self.emit_degraded(CORRELATION_RELAXED_FALLBACK_REASON);
        }
        relaxed_match
    }

    fn remove_entry(&mut self, entry_id: u64) -> Option<RequestCorrelation> {
        let entry = self.by_id.remove(&entry_id)?;
        self.order.retain(|existing| *existing != entry_id);
        if let Some(lookup_key) = entry.unresolved_lookup_key.as_ref() {
            self.remove_from_unresolved_index(lookup_key, entry_id);
        }
        if self
            .by_fetch_request_id
            .get(&entry.fetch_request_id)
            .copied()
            == Some(entry_id)
        {
            self.by_fetch_request_id.remove(&entry.fetch_request_id);
        }
        if let Some(network_id) = entry.network_id.as_ref()
            && self.by_network_request_id.get(network_id).copied() == Some(entry_id)
        {
            self.by_network_request_id.remove(network_id);
        }
        Some(entry.correlation)
    }

    fn retire_unresolved(&mut self, entry_id: u64) {
        let lookup_key = self
            .by_id
            .get(&entry_id)
            .and_then(|entry| entry.unresolved_lookup_key.clone());
        if let Some(lookup_key) = lookup_key.as_ref() {
            self.remove_from_unresolved_index(lookup_key, entry_id);
        }
        if let Some(entry) = self.by_id.get_mut(&entry_id) {
            entry.unresolved_match = None;
            entry.unresolved_lookup_key = None;
        }
    }

    #[cfg(test)]
    fn fallback_metrics(&self) -> CorrelationFallbackMetrics {
        self.fallback_metrics
    }

    pub(crate) fn take_degraded_reasons(&mut self) -> Vec<&'static str> {
        self.degraded_reasons.drain(..).collect()
    }

    pub(crate) fn mark_runtime_degraded(&mut self, reason: &'static str) {
        self.emit_degraded(reason);
    }

    fn emit_degraded(&mut self, reason: &'static str) {
        if self.degraded_reasons.back().copied() != Some(reason) {
            self.degraded_reasons.push_back(reason);
        }
    }

    fn remove_from_unresolved_index(&mut self, lookup_key: &UnresolvedLookupKey, entry_id: u64) {
        let remove_bucket = if let Some(entries) = self.unresolved_by_lookup_key.get_mut(lookup_key)
        {
            entries.retain(|existing| *existing != entry_id);
            entries.is_empty()
        } else {
            false
        };
        if remove_bucket {
            self.unresolved_by_lookup_key.remove(lookup_key);
        }
    }
}

#[derive(Debug, Clone)]
struct UnresolvedCorrelationMatch {
    expected_url: String,
    expected_method: String,
    effective_request_headers: Option<BTreeMap<String, String>>,
    expected_tab_target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UnresolvedLookupKey {
    expected_tab_target_id: Option<String>,
    expected_url: String,
    expected_method: String,
}

impl UnresolvedLookupKey {
    fn from_match(unresolved_match: &UnresolvedCorrelationMatch) -> Self {
        Self {
            expected_tab_target_id: unresolved_match.expected_tab_target_id.clone(),
            expected_url: unresolved_match.expected_url.clone(),
            expected_method: unresolved_match.expected_method.clone(),
        }
    }

    fn for_request(url: &str, method: &str, tab_target_id: Option<&str>) -> Self {
        Self {
            expected_tab_target_id: tab_target_id.map(str::to_string),
            expected_url: url.to_string(),
            expected_method: method.to_string(),
        }
    }
}

fn headers_include(actual: &BTreeMap<String, String>, expected: &BTreeMap<String, String>) -> bool {
    expected.iter().all(|(name, value)| {
        actual
            .get(&normalize_header_name(name))
            .or_else(|| actual.get(name))
            == Some(value)
    })
}

#[cfg(test)]
mod tests;
