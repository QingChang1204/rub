use super::{
    CORRELATION_LIMIT, CORRELATION_REGISTRY_EVICTED_REASON,
    CORRELATION_REGISTRY_TTL_EXPIRED_REASON, CORRELATION_RELAXED_FALLBACK_REASON,
    CORRELATION_STRICT_FALLBACK_REASON, CORRELATION_TTL, CorrelationFallbackMetrics,
    RequestCorrelation, RequestCorrelationRegistry, headers_include, normalize_header_name,
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
                tab_target_id: Some("tab-1".to_string()),
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
            .take_for_request("req-0", "https://example.com/0", "GET", None, Some("tab-1"))
            .is_none()
    );
    let last = registry.take_for_request(
        &format!("req-{}", CORRELATION_LIMIT + 3),
        &format!("https://example.com/{}", CORRELATION_LIMIT + 3),
        "GET",
        None,
        Some("tab-1"),
    );
    assert!(last.is_some());
    assert!(
        registry
            .take_for_request(
                &format!("req-{}", CORRELATION_LIMIT + 3),
                &format!("https://example.com/{}", CORRELATION_LIMIT + 3),
                "GET",
                None,
                Some("tab-1")
            )
            .is_none()
    );
    assert_eq!(
        registry.take_degraded_reasons(),
        vec![CORRELATION_REGISTRY_EVICTED_REASON]
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
            tab_target_id: Some("tab-1".to_string()),
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
        .peek_for_request(
            "net-1",
            "https://cdn.example.com/1",
            "GET",
            None,
            Some("tab-1"),
        )
        .expect("peek should clone entry");
    assert_eq!(peeked.original_url, "https://example.com/1");
    assert!(
        registry
            .take_for_request(
                "req-1",
                "https://cdn.example.com/1",
                "GET",
                None,
                Some("tab-1")
            )
            .is_some()
    );
}

#[test]
fn network_id_lookup_isolated_from_fetch_request_id_collisions() {
    let mut registry = RequestCorrelationRegistry::default();
    registry.record(
        "shared-id".to_string(),
        None,
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/fetch".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 41,
                kind: NetworkRuleEffectKind::Allow,
            }],
        },
    );
    registry.record(
        "fetch-2".to_string(),
        Some("shared-id".to_string()),
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/network".to_string(),
            rewritten_url: Some("https://cdn.example.com/network".to_string()),
            effective_request_headers: None,
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 42,
                kind: NetworkRuleEffectKind::Rewrite,
            }],
        },
    );

    let resolved = registry
        .take_for_request(
            "shared-id",
            "https://cdn.example.com/network",
            "GET",
            None,
            Some("tab-1"),
        )
        .expect("network-side lookup should not be stolen by fetch request id collisions");
    assert_eq!(resolved.original_url, "https://example.com/network");

    let fetch_entry = registry
        .take_for_request(
            "shared-id",
            "https://example.com/fetch",
            "GET",
            None,
            Some("tab-1"),
        )
        .expect("fetch-side entry should remain available after network-side consumption");
    assert_eq!(fetch_entry.original_url, "https://example.com/fetch");
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
            tab_target_id: Some("tab-1".to_string()),
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
                Some("tab-1"),
                now + CORRELATION_TTL + Duration::from_secs(1)
            )
            .is_none()
    );
    assert_eq!(
        registry.take_degraded_reasons(),
        vec![CORRELATION_REGISTRY_TTL_EXPIRED_REASON]
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
            tab_target_id: Some("tab-1".to_string()),
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
            Some("tab-1"),
        )
        .expect("fallback should bridge correlation");
    assert_eq!(peeked.original_url, "https://example.com/api");
    assert_eq!(
        registry.take_degraded_reasons(),
        vec![CORRELATION_STRICT_FALLBACK_REASON]
    );
    assert!(
        registry
            .take_for_request(
                "network-1",
                "https://cdn.example.com/api",
                "GET",
                Some(&expected_headers),
                Some("tab-1"),
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
            tab_target_id: Some("tab-1".to_string()),
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
            Some("tab-1"),
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
                Some("tab-1"),
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
            tab_target_id: Some("tab-1".to_string()),
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
            Some("tab-1"),
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
                Some("tab-1"),
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
            tab_target_id: Some("tab-1".to_string()),
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
            .peek_for_request(
                "req-1",
                "https://cdn.example.com/api",
                "GET",
                None,
                Some("tab-1")
            )
            .is_none()
    );
    assert!(
        registry
            .take_for_request(
                "req-1",
                "https://cdn.example.com/api",
                "POST",
                None,
                Some("tab-1")
            )
            .is_some()
    );
}

#[test]
fn correlation_registry_falls_back_to_unique_url_method_when_effective_headers_are_not_visible() {
    let mut registry = RequestCorrelationRegistry::default();
    let mut expected_headers = BTreeMap::new();
    expected_headers.insert("x-rub-test".to_string(), "1".to_string());
    registry.record(
        "fetch-1".to_string(),
        None,
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/api".to_string(),
            rewritten_url: Some("https://example.com/api".to_string()),
            effective_request_headers: Some(expected_headers),
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 11,
                kind: NetworkRuleEffectKind::HeaderOverride,
            }],
        },
    );

    let peeked = registry
        .peek_for_request(
            "network-1",
            "https://example.com/api",
            "GET",
            Some(&BTreeMap::new()),
            Some("tab-1"),
        )
        .expect("unique URL+method fallback should bridge correlation");
    assert_eq!(peeked.applied_rule_effects.len(), 1);
    assert_eq!(
        peeked.applied_rule_effects[0].kind,
        NetworkRuleEffectKind::HeaderOverride
    );
    assert_eq!(
        registry.take_degraded_reasons(),
        vec![CORRELATION_RELAXED_FALLBACK_REASON]
    );
}

#[test]
fn correlation_registry_relaxed_fallback_requires_same_tab_target() {
    let mut registry = RequestCorrelationRegistry::default();
    registry.record(
        "fetch-1".to_string(),
        None,
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/api".to_string(),
            rewritten_url: Some("https://example.com/api".to_string()),
            effective_request_headers: Some(BTreeMap::from([(
                "x-rub-test".to_string(),
                "1".to_string(),
            )])),
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 12,
                kind: NetworkRuleEffectKind::HeaderOverride,
            }],
        },
    );

    assert!(
        registry
            .peek_for_request(
                "network-1",
                "https://example.com/api",
                "GET",
                Some(&BTreeMap::new()),
                Some("tab-2"),
            )
            .is_none(),
        "relaxed fallback must stay within the same tab target"
    );
    assert!(
        registry
            .peek_for_request(
                "network-1",
                "https://example.com/api",
                "GET",
                Some(&BTreeMap::new()),
                Some("tab-1"),
            )
            .is_some(),
        "same-tab relaxed fallback should still bridge when the match is unique"
    );
}

#[test]
fn correlation_registry_fails_closed_when_relaxed_url_method_match_is_ambiguous() {
    let mut registry = RequestCorrelationRegistry::default();

    for (index, header_value) in ["1", "2"].into_iter().enumerate() {
        let mut expected_headers = BTreeMap::new();
        expected_headers.insert("x-rub-test".to_string(), header_value.to_string());
        registry.record(
            format!("fetch-{index}"),
            None,
            "GET",
            RequestCorrelation {
                tab_target_id: Some("tab-1".to_string()),
                original_url: "https://example.com/api".to_string(),
                rewritten_url: Some("https://example.com/api".to_string()),
                effective_request_headers: Some(expected_headers),
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: index as u32,
                    kind: NetworkRuleEffectKind::HeaderOverride,
                }],
            },
        );
    }

    assert!(
        registry
            .peek_for_request(
                "network-1",
                "https://example.com/api",
                "GET",
                Some(&BTreeMap::new()),
                Some("tab-1"),
            )
            .is_none(),
        "relaxed fallback must fail closed when multiple unresolved correlations share URL+method",
    );
}

#[test]
fn direct_key_lookup_leaves_fallback_metrics_empty() {
    let mut registry = RequestCorrelationRegistry::default();
    registry.record(
        "fetch-1".to_string(),
        Some("network-1".to_string()),
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/api".to_string(),
            rewritten_url: None,
            effective_request_headers: None,
            applied_rule_effects: vec![],
        },
    );

    let _ = registry.peek_for_request(
        "network-1",
        "https://example.com/api",
        "GET",
        None,
        Some("tab-1"),
    );

    assert_eq!(
        registry.fallback_metrics(),
        CorrelationFallbackMetrics::default()
    );
}

#[test]
fn unresolved_fallback_metrics_capture_scan_baseline() {
    let mut registry = RequestCorrelationRegistry::default();
    let mut headers = BTreeMap::new();
    headers.insert("x-rub-test".to_string(), "1".to_string());
    registry.record(
        "fetch-1".to_string(),
        None,
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/one".to_string(),
            rewritten_url: Some("https://cdn.example.com/one".to_string()),
            effective_request_headers: None,
            applied_rule_effects: vec![],
        },
    );
    registry.record(
        "fetch-2".to_string(),
        None,
        "GET",
        RequestCorrelation {
            tab_target_id: Some("tab-1".to_string()),
            original_url: "https://example.com/two".to_string(),
            rewritten_url: Some("https://cdn.example.com/two".to_string()),
            effective_request_headers: Some(headers.clone()),
            applied_rule_effects: vec![],
        },
    );

    let _ = registry.peek_for_request(
        "request-side-key",
        "https://cdn.example.com/two",
        "GET",
        Some(&headers),
        Some("tab-1"),
    );

    assert_eq!(
        registry.fallback_metrics(),
        CorrelationFallbackMetrics {
            unresolved_fallback_attempts: 1,
            unresolved_entries_scanned: 1,
            relaxed_match_candidates: 1,
            strict_match_candidates: 1,
        }
    );
}
