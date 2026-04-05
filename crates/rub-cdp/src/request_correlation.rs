use rub_core::model::NetworkRuleEffect;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

const CORRELATION_LIMIT: usize = 2_048;
const CORRELATION_TTL: Duration = Duration::from_secs(900);

/// Correlated request metadata produced by network rule execution and consumed
/// by the runtime observatory when the request later resolves or fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCorrelation {
    pub original_url: String,
    pub rewritten_url: Option<String>,
    pub effective_request_headers: Option<BTreeMap<String, String>>,
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
}

/// Bounded bridge between Fetch interception and Network observability.
#[derive(Debug, Default)]
pub struct RequestCorrelationRegistry {
    next_id: u64,
    by_id: HashMap<u64, TimedCorrelation>,
    by_key: HashMap<String, u64>,
    order: VecDeque<u64>,
    unresolved: VecDeque<u64>,
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
    ) -> Option<RequestCorrelation> {
        self.peek_at(request_id, url, method, request_headers, Instant::now())
    }

    pub fn take_for_request(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
    ) -> Option<RequestCorrelation> {
        self.take_at(request_id, url, method, request_headers, Instant::now())
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
        while self.order.len() > CORRELATION_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.remove_entry(oldest);
            }
        }
    }

    fn take_at(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        now: Instant,
    ) -> Option<RequestCorrelation> {
        self.prune(now);
        let entry_id = if let Some(entry_id) = self.by_key.get(request_id).copied() {
            Some(entry_id)
        } else {
            self.find_unique_unresolved(url, method, request_headers)
        };
        entry_id.and_then(|entry_id| self.remove_entry(entry_id))
    }

    fn peek_at(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
        now: Instant,
    ) -> Option<RequestCorrelation> {
        self.prune(now);
        if let Some(entry_id) = self.by_key.get(request_id).copied() {
            self.retire_unresolved(entry_id);
            return self
                .by_id
                .get(&entry_id)
                .map(|entry| entry.correlation.clone());
        }

        self.find_unique_unresolved(url, method, request_headers)
            .and_then(|entry_id| self.by_id.get(&entry_id))
            .map(|entry| entry.correlation.clone())
    }

    fn prune(&mut self, now: Instant) {
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
        }
        self.unresolved
            .retain(|entry_id| self.by_id.contains_key(entry_id));
    }
}

#[derive(Debug)]
struct TimedCorrelation {
    recorded_at: Instant,
    keys: Vec<String>,
    unresolved_match: Option<UnresolvedCorrelationMatch>,
    correlation: RequestCorrelation,
}

impl RequestCorrelationRegistry {
    fn insert_timed(
        &mut self,
        fetch_request_id: String,
        network_id: Option<String>,
        method: String,
        correlation: RequestCorrelation,
        now: Instant,
    ) {
        let entry_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);

        let mut keys = vec![fetch_request_id];
        if let Some(network_id) = network_id
            && keys.iter().all(|existing| existing != &network_id)
        {
            keys.push(network_id);
        }

        for key in &keys {
            self.by_key.insert(key.clone(), entry_id);
        }

        let unresolved_match = Some(UnresolvedCorrelationMatch {
            expected_url: correlation
                .rewritten_url
                .clone()
                .unwrap_or_else(|| correlation.original_url.clone()),
            expected_method: method,
            effective_request_headers: correlation.effective_request_headers.clone(),
        });
        self.unresolved.push_back(entry_id);

        self.by_id.insert(
            entry_id,
            TimedCorrelation {
                recorded_at: now,
                keys,
                unresolved_match,
                correlation,
            },
        );
        self.order.push_back(entry_id);
    }

    fn find_unique_unresolved(
        &self,
        url: &str,
        method: &str,
        request_headers: Option<&BTreeMap<String, String>>,
    ) -> Option<u64> {
        let mut matched = None;
        for entry_id in &self.unresolved {
            let Some(entry) = self.by_id.get(entry_id) else {
                continue;
            };
            let Some(unresolved_match) = entry.unresolved_match.as_ref() else {
                continue;
            };
            if unresolved_match.expected_url != url {
                continue;
            }
            if unresolved_match.expected_method != method {
                continue;
            }
            if let Some(expected_headers) = unresolved_match.effective_request_headers.as_ref() {
                let Some(actual_headers) = request_headers else {
                    continue;
                };
                if !headers_include(actual_headers, expected_headers) {
                    continue;
                }
            }
            if matched.is_some() {
                return None;
            }
            matched = Some(*entry_id);
        }
        matched
    }

    fn remove_entry(&mut self, entry_id: u64) -> Option<RequestCorrelation> {
        let entry = self.by_id.remove(&entry_id)?;
        self.order.retain(|existing| *existing != entry_id);
        self.unresolved.retain(|existing| *existing != entry_id);
        for key in &entry.keys {
            if self.by_key.get(key).copied() == Some(entry_id) {
                self.by_key.remove(key);
            }
        }
        Some(entry.correlation)
    }

    fn retire_unresolved(&mut self, entry_id: u64) {
        if let Some(entry) = self.by_id.get_mut(&entry_id) {
            entry.unresolved_match = None;
        }
        self.unresolved.retain(|existing| *existing != entry_id);
    }
}

#[derive(Debug)]
struct UnresolvedCorrelationMatch {
    expected_url: String,
    expected_method: String,
    effective_request_headers: Option<BTreeMap<String, String>>,
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
mod tests {
    use super::{
        CORRELATION_LIMIT, CORRELATION_TTL, RequestCorrelation, RequestCorrelationRegistry,
        headers_include, normalize_header_name,
    };
    use rub_core::model::{NetworkRuleEffect, NetworkRuleEffectKind};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    #[test]
    fn correlation_registry_is_bounded_and_consuming() {
        let mut registry = RequestCorrelationRegistry::default();
        for index in 0..(CORRELATION_LIMIT + 4) {
            registry.record(
                format!("req-{index}"),
                None,
                "GET",
                RequestCorrelation {
                    original_url: format!("https://example.com/{index}"),
                    rewritten_url: None,
                    effective_request_headers: None,
                    applied_rule_effects: vec![NetworkRuleEffect {
                        rule_id: index as u32,
                        kind: NetworkRuleEffectKind::Allow,
                    }],
                },
            );
        }

        assert!(
            registry
                .take_for_request("req-0", "https://example.com/0", "GET", None)
                .is_none()
        );
        let last = registry.take_for_request(
            &format!("req-{}", CORRELATION_LIMIT + 3),
            &format!("https://example.com/{}", CORRELATION_LIMIT + 3),
            "GET",
            None,
        );
        assert!(last.is_some());
        assert!(
            registry
                .take_for_request(
                    &format!("req-{}", CORRELATION_LIMIT + 3),
                    &format!("https://example.com/{}", CORRELATION_LIMIT + 3),
                    "GET",
                    None
                )
                .is_none()
        );
    }

    #[test]
    fn correlation_registry_peek_does_not_consume_entry() {
        let mut registry = RequestCorrelationRegistry::default();
        registry.record(
            "req-1".to_string(),
            Some("net-1".to_string()),
            "GET",
            RequestCorrelation {
                original_url: "https://example.com/1".to_string(),
                rewritten_url: Some("https://cdn.example.com/1".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 1,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        let peeked = registry
            .peek_for_request("net-1", "https://cdn.example.com/1", "GET", None)
            .expect("peek should clone entry");
        assert_eq!(peeked.original_url, "https://example.com/1");
        assert!(
            registry
                .take_for_request("req-1", "https://cdn.example.com/1", "GET", None)
                .is_some()
        );
    }

    #[test]
    fn correlation_registry_expires_stale_entries() {
        let mut registry = RequestCorrelationRegistry::default();
        let now = Instant::now();
        registry.record_at(
            "req-1".to_string(),
            None,
            "GET".to_string(),
            RequestCorrelation {
                original_url: "https://example.com/1".to_string(),
                rewritten_url: None,
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 1,
                    kind: NetworkRuleEffectKind::Block,
                }],
            },
            now,
        );

        assert!(
            registry
                .take_at(
                    "req-1",
                    "https://example.com/1",
                    "GET",
                    None,
                    now + CORRELATION_TTL + Duration::from_secs(1)
                )
                .is_none()
        );
    }

    #[test]
    fn correlation_registry_falls_back_to_unique_url_and_header_match_without_network_id() {
        let mut registry = RequestCorrelationRegistry::default();
        let mut expected_headers = BTreeMap::new();
        expected_headers.insert("x-rub-test".to_string(), "1".to_string());
        registry.record(
            "fetch-1".to_string(),
            None,
            "GET",
            RequestCorrelation {
                original_url: "https://example.com/api".to_string(),
                rewritten_url: Some("https://cdn.example.com/api".to_string()),
                effective_request_headers: Some(expected_headers.clone()),
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 1,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        let peeked = registry
            .peek_for_request(
                "network-1",
                "https://cdn.example.com/api",
                "GET",
                Some(&expected_headers),
            )
            .expect("fallback should bridge correlation");
        assert_eq!(peeked.original_url, "https://example.com/api");
        assert!(
            registry
                .take_for_request(
                    "network-1",
                    "https://cdn.example.com/api",
                    "GET",
                    Some(&expected_headers)
                )
                .is_some()
        );
    }

    #[test]
    fn correlation_registry_falls_back_when_network_key_space_does_not_match_request_id() {
        let mut registry = RequestCorrelationRegistry::default();
        let mut expected_headers = BTreeMap::new();
        expected_headers.insert("x-rub-test".to_string(), "1".to_string());
        registry.record(
            "fetch-1".to_string(),
            Some("network-side-key".to_string()),
            "GET",
            RequestCorrelation {
                original_url: "https://example.com/api".to_string(),
                rewritten_url: Some("https://cdn.example.com/api".to_string()),
                effective_request_headers: Some(expected_headers.clone()),
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 2,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        let peeked = registry
            .peek_for_request(
                "request-side-key",
                "https://cdn.example.com/api",
                "GET",
                Some(&expected_headers),
            )
            .expect("fallback should bridge mismatched key spaces");
        assert_eq!(peeked.original_url, "https://example.com/api");
        assert!(
            registry
                .take_for_request(
                    "request-side-key",
                    "https://cdn.example.com/api",
                    "GET",
                    Some(&expected_headers),
                )
                .is_some()
        );
    }

    #[test]
    fn header_match_is_case_insensitive() {
        let mut actual = BTreeMap::new();
        actual.insert(
            normalize_header_name("Authorization"),
            "Bearer live".to_string(),
        );
        let mut expected = BTreeMap::new();
        expected.insert("authorization".to_string(), "Bearer live".to_string());
        assert!(headers_include(&actual, &expected));
    }

    #[test]
    fn exact_binding_retires_unresolved_fallback_pool() {
        let mut registry = RequestCorrelationRegistry::default();
        let mut headers = BTreeMap::new();
        headers.insert("x-rub-test".to_string(), "1".to_string());
        registry.record(
            "fetch-1".to_string(),
            Some("net-1".to_string()),
            "GET",
            RequestCorrelation {
                original_url: "https://example.com/api".to_string(),
                rewritten_url: Some("https://cdn.example.com/api".to_string()),
                effective_request_headers: Some(headers.clone()),
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 9,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        let exact = registry
            .peek_for_request(
                "net-1",
                "https://cdn.example.com/api",
                "GET",
                Some(&headers),
            )
            .expect("exact binding should still succeed");
        assert_eq!(exact.original_url, "https://example.com/api");
        assert!(
            registry
                .peek_for_request(
                    "unrelated-request",
                    "https://cdn.example.com/api",
                    "GET",
                    Some(&headers),
                )
                .is_none(),
            "exactly bound correlations must leave the unresolved fallback pool"
        );
    }

    #[test]
    fn unresolved_fallback_requires_matching_http_method() {
        let mut registry = RequestCorrelationRegistry::default();
        registry.record(
            "fetch-1".to_string(),
            None,
            "POST",
            RequestCorrelation {
                original_url: "https://example.com/api".to_string(),
                rewritten_url: Some("https://cdn.example.com/api".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 10,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        assert!(
            registry
                .peek_for_request("req-1", "https://cdn.example.com/api", "GET", None)
                .is_none()
        );
        assert!(
            registry
                .take_for_request("req-1", "https://cdn.example.com/api", "POST", None)
                .is_some()
        );
    }
}
