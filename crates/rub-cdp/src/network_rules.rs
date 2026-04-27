use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, DisableParams, EnableParams, EventRequestPaused, HeaderEntry,
    RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, Headers};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    NetworkRule, NetworkRuleEffect, NetworkRuleEffectKind, NetworkRuleSpec, NetworkRuleStatus,
};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};
use crate::request_correlation::{
    RequestCorrelation, RequestCorrelationRegistry, normalize_header_name,
};

#[derive(Debug, Default, Clone)]
pub struct NetworkRuleRuntime {
    rules: Vec<NetworkRule>,
    fetch_enabled_targets: HashSet<String>,
}

impl NetworkRuleRuntime {
    pub fn replace_rules(&mut self, rules: Vec<NetworkRule>) {
        self.rules = rules;
    }

    pub fn clear_browser_installation_state(&mut self) {
        self.fetch_enabled_targets.clear();
    }

    pub fn rules_snapshot(&self) -> Vec<NetworkRule> {
        self.rules.clone()
    }

    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    pub fn has_active_rules(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| matches!(rule.status, NetworkRuleStatus::Active))
    }

    pub fn is_fetch_enabled_for(&self, target_id: &str) -> bool {
        self.fetch_enabled_targets.contains(target_id)
    }

    pub fn mark_fetch_enabled(&mut self, target_id: String) {
        self.fetch_enabled_targets.insert(target_id);
    }

    pub fn mark_fetch_disabled(&mut self, target_id: &str) {
        self.fetch_enabled_targets.remove(target_id);
    }
}

pub async fn ensure_page_request_interception(
    page: Arc<Page>,
    runtime: Arc<RwLock<NetworkRuleRuntime>>,
    request_correlation: Arc<Mutex<RequestCorrelationRegistry>>,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
) -> Result<(), RubError> {
    let mut listener = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|error| {
            RubError::domain(
                ErrorCode::BrowserCrashed,
                format!("Failed to subscribe to request interception events: {error}"),
            )
        })?;

    sync_fetch_domain_for_page(page.clone(), runtime.clone()).await?;

    tokio::spawn(async move {
        let mut generation_rx = listener_generation_rx;
        while let Some(event) =
            next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
        {
            if let Err(error) = handle_request_paused(
                &page,
                event.as_ref(),
                runtime.clone(),
                request_correlation.clone(),
            )
            .await
            {
                warn!(%error, "Failed to apply session-scoped network rule");
            }
        }
    });

    Ok(())
}

pub async fn sync_fetch_domain_for_pages(
    pages: &[Arc<Page>],
    runtime: Arc<RwLock<NetworkRuleRuntime>>,
) -> Result<(), RubError> {
    for page in pages {
        sync_fetch_domain_for_page(page.clone(), runtime.clone()).await?;
    }
    Ok(())
}

async fn sync_fetch_domain_for_page(
    page: Arc<Page>,
    runtime: Arc<RwLock<NetworkRuleRuntime>>,
) -> Result<(), RubError> {
    let target_id = page.target_id().as_ref().to_string();
    let (has_rules, already_enabled) = {
        let runtime = runtime.read().await;
        (
            runtime.has_active_rules(),
            runtime.is_fetch_enabled_for(&target_id),
        )
    };

    if has_rules && !already_enabled {
        page.execute(
            EnableParams::builder()
                .pattern(RequestPattern::builder().url_pattern("*").build())
                .build(),
        )
        .await
        .map_err(|e| {
            RubError::domain(
                ErrorCode::BrowserCrashed,
                format!("Failed to enable request interception for target {target_id}: {e}"),
            )
        })?;
        runtime.write().await.mark_fetch_enabled(target_id);
    } else if !has_rules && already_enabled {
        page.execute(DisableParams::default()).await.map_err(|e| {
            RubError::domain(
                ErrorCode::BrowserCrashed,
                format!("Failed to disable request interception for target {target_id}: {e}"),
            )
        })?;
        runtime.write().await.mark_fetch_disabled(&target_id);
    }

    Ok(())
}

async fn handle_request_paused(
    page: &Page,
    event: &EventRequestPaused,
    runtime: Arc<RwLock<NetworkRuleRuntime>>,
    request_correlation: Arc<Mutex<RequestCorrelationRegistry>>,
) -> Result<(), RubError> {
    if event.response_status_code.is_some() {
        page.execute(ContinueRequestParams::new(event.request_id.clone()))
            .await
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserCrashed,
                    format!("Failed to continue paused response request: {e}"),
                )
            })?;
        return Ok(());
    }

    let rules = runtime.read().await.rules_snapshot();
    let Some(plan) = build_request_plan(&event.request.url, &event.request.headers, &rules) else {
        page.execute(ContinueRequestParams::new(event.request_id.clone()))
            .await
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserCrashed,
                    format!("Failed to continue paused request: {e}"),
                )
            })?;
        return Ok(());
    };

    let correlation = (!plan.applied_rule_effects.is_empty()).then(|| {
        let effective_request_headers = match &plan.action {
            RequestPlanAction::Continue { headers, .. } => headers
                .as_ref()
                .map(|headers| header_entries_to_map(headers.as_slice())),
            RequestPlanAction::Block => None,
        };
        let rewritten_url = match &plan.action {
            RequestPlanAction::Continue { url, .. } => url.clone(),
            RequestPlanAction::Block => None,
        };
        (
            event
                .network_id
                .as_ref()
                .map(|network_id| network_id.as_ref().to_string()),
            RequestCorrelation {
                tab_target_id: Some(page.target_id().as_ref().to_string()),
                original_url: event.request.url.clone(),
                rewritten_url,
                effective_request_headers,
                applied_rule_effects: plan.applied_rule_effects.clone(),
            },
        )
    });

    publish_request_correlation_before_actuation(
        &request_correlation,
        event.request_id.as_ref(),
        event.request.method.as_str(),
        correlation.as_ref(),
    )
    .await;

    match plan.action {
        RequestPlanAction::Continue { url, headers } => {
            let mut builder = ContinueRequestParams::builder().request_id(event.request_id.clone());
            if let Some(url) = url {
                builder = builder.url(url);
            }
            if let Some(headers) = headers {
                builder = builder.headers(headers);
            }
            run_request_actuation_with_prerecorded_correlation(
                &request_correlation,
                event.request_id.as_ref(),
                move || {
                    let command = builder.build().map_err(RubError::Internal)?;
                    Ok(async move {
                        page.execute(command).await.map_err(|e| {
                            RubError::domain(
                                ErrorCode::BrowserCrashed,
                                format!("Failed to continue intercepted request: {e}"),
                            )
                        })
                    })
                },
            )
            .await?;
        }
        RequestPlanAction::Block => {
            run_request_actuation_with_prerecorded_correlation(
                &request_correlation,
                event.request_id.as_ref(),
                move || {
                    Ok(async move {
                        page.execute(
                            chromiumoxide::cdp::browser_protocol::fetch::FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::BlockedByClient,
                            ),
                        )
                        .await
                        .map_err(|e| {
                            RubError::domain(
                                ErrorCode::BrowserCrashed,
                                format!("Failed to block intercepted request: {e}"),
                            )
                        })
                    })
                },
            )
            .await?;
        }
    }

    Ok(())
}

async fn publish_request_correlation_before_actuation(
    request_correlation: &Arc<Mutex<RequestCorrelationRegistry>>,
    fetch_request_id: &str,
    method: &str,
    correlation: Option<&(Option<String>, RequestCorrelation)>,
) {
    let Some((network_id, correlation)) = correlation else {
        return;
    };
    request_correlation.lock().await.record(
        fetch_request_id.to_string(),
        network_id.clone(),
        method.to_string(),
        correlation.clone(),
    );
}

async fn rollback_request_correlation_after_failed_actuation(
    request_correlation: &Arc<Mutex<RequestCorrelationRegistry>>,
    fetch_request_id: &str,
) {
    request_correlation
        .lock()
        .await
        .discard_for_fetch_request_id(fetch_request_id);
}

async fn run_request_actuation_with_prerecorded_correlation<T, Prepare, Fut>(
    request_correlation: &Arc<Mutex<RequestCorrelationRegistry>>,
    fetch_request_id: &str,
    prepare: Prepare,
) -> Result<T, RubError>
where
    Prepare: FnOnce() -> Result<Fut, RubError>,
    Fut: Future<Output = Result<T, RubError>>,
{
    let future = match prepare() {
        Ok(future) => future,
        Err(error) => {
            rollback_request_correlation_after_failed_actuation(
                request_correlation,
                fetch_request_id,
            )
            .await;
            return Err(error);
        }
    };
    match future.await {
        Ok(value) => Ok(value),
        Err(error) => {
            rollback_request_correlation_after_failed_actuation(
                request_correlation,
                fetch_request_id,
            )
            .await;
            Err(error)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedRequestPlan {
    action: RequestPlanAction,
    applied_rule_effects: Vec<NetworkRuleEffect>,
}

#[derive(Debug, Clone, PartialEq)]
enum RequestPlanAction {
    Continue {
        url: Option<String>,
        headers: Option<Vec<HeaderEntry>>,
    },
    Block,
}

fn build_request_plan(
    url: &str,
    headers: &Headers,
    rules: &[NetworkRule],
) -> Option<ResolvedRequestPlan> {
    let mut matched_any = false;
    let mut terminal_action = None;
    let mut merged_headers = headers_to_map(headers);
    let mut headers_modified = false;
    let mut applied_rule_effects = Vec::new();

    for rule in rules {
        if !matches!(rule.status, NetworkRuleStatus::Active) {
            continue;
        }

        let matches = match &rule.spec {
            NetworkRuleSpec::Rewrite { url_pattern, .. }
            | NetworkRuleSpec::Block { url_pattern }
            | NetworkRuleSpec::Allow { url_pattern }
            | NetworkRuleSpec::HeaderOverride { url_pattern, .. } => {
                wildcard_matches(url_pattern, url)
            }
        };

        if !matches {
            continue;
        }
        matched_any = true;

        match &rule.spec {
            NetworkRuleSpec::Rewrite {
                url_pattern,
                target_base,
            } => {
                applied_rule_effects.push(NetworkRuleEffect {
                    rule_id: rule.id,
                    kind: NetworkRuleEffectKind::Rewrite,
                });
                if terminal_action.is_none() {
                    terminal_action = Some(TerminalAction::Rewrite(rewrite_url(
                        url,
                        url_pattern,
                        target_base,
                    )));
                }
            }
            NetworkRuleSpec::Block { .. } => {
                applied_rule_effects.push(NetworkRuleEffect {
                    rule_id: rule.id,
                    kind: NetworkRuleEffectKind::Block,
                });
                if terminal_action.is_none() {
                    terminal_action = Some(TerminalAction::Block);
                }
            }
            NetworkRuleSpec::Allow { .. } => {
                applied_rule_effects.push(NetworkRuleEffect {
                    rule_id: rule.id,
                    kind: NetworkRuleEffectKind::Allow,
                });
                if terminal_action.is_none() {
                    terminal_action = Some(TerminalAction::Allow);
                }
            }
            NetworkRuleSpec::HeaderOverride { headers, .. } => {
                applied_rule_effects.push(NetworkRuleEffect {
                    rule_id: rule.id,
                    kind: NetworkRuleEffectKind::HeaderOverride,
                });
                headers_modified = true;
                for (name, value) in headers {
                    merged_headers.insert(normalize_header_name(name), value.clone());
                }
            }
        }
    }

    if !matched_any {
        return None;
    }

    match terminal_action.unwrap_or(TerminalAction::Allow) {
        TerminalAction::Block => Some(ResolvedRequestPlan {
            action: RequestPlanAction::Block,
            applied_rule_effects,
        }),
        TerminalAction::Allow => Some(ResolvedRequestPlan {
            action: RequestPlanAction::Continue {
                url: None,
                headers: headers_modified.then(|| map_to_header_entries(&merged_headers)),
            },
            applied_rule_effects,
        }),
        TerminalAction::Rewrite(url) => Some(ResolvedRequestPlan {
            action: RequestPlanAction::Continue {
                url: Some(url),
                headers: headers_modified.then(|| map_to_header_entries(&merged_headers)),
            },
            applied_rule_effects,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalAction {
    Allow,
    Block,
    Rewrite(String),
}

fn headers_to_map(headers: &Headers) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(obj) = headers.inner().as_object() {
        for (name, value) in obj {
            let value = value
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| value.to_string());
            map.insert(normalize_header_name(name), value);
        }
    }
    map
}

fn map_to_header_entries(headers: &BTreeMap<String, String>) -> Vec<HeaderEntry> {
    headers
        .iter()
        .map(|(name, value)| HeaderEntry::new(name.clone(), value.clone()))
        .collect()
}

fn header_entries_to_map(headers: &[HeaderEntry]) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|header| (normalize_header_name(&header.name), header.value.clone()))
        .collect()
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == text;
    }

    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');
    let mut remainder = text;

    for (idx, part) in parts.iter().filter(|part| !part.is_empty()).enumerate() {
        if idx == 0 && !starts_with_wildcard {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            continue;
        }

        if let Some(position) = remainder.find(part) {
            remainder = &remainder[position + part.len()..];
        } else {
            return false;
        }
    }

    if !ends_with_wildcard && let Some(last_part) = parts.iter().rev().find(|part| !part.is_empty())
    {
        return text.ends_with(last_part);
    }

    true
}

fn rewrite_url(url: &str, pattern: &str, target_base: &str) -> String {
    if let Some(prefix) = pattern.strip_suffix('*')
        && url.starts_with(prefix)
    {
        let suffix = &url[prefix.len()..];
        return join_rewrite_target(target_base, suffix);
    }

    target_base.to_string()
}

fn join_rewrite_target(target_base: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        return target_base.to_string();
    }
    let target_trimmed = target_base.trim_end_matches('/');
    let suffix_trimmed = suffix.trim_start_matches('/');
    format!("{target_trimmed}/{suffix_trimmed}")
}

#[cfg(test)]
mod tests {
    use super::{
        build_request_plan, headers_to_map, join_rewrite_target,
        publish_request_correlation_before_actuation,
        rollback_request_correlation_after_failed_actuation,
        run_request_actuation_with_prerecorded_correlation, wildcard_matches,
    };
    use chromiumoxide::cdp::browser_protocol::network::Headers;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{
        NetworkRule, NetworkRuleEffect, NetworkRuleEffectKind, NetworkRuleSpec, NetworkRuleStatus,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use crate::request_correlation::{RequestCorrelation, RequestCorrelationRegistry};

    #[test]
    fn wildcard_matching_supports_exact_and_glob_patterns() {
        assert!(wildcard_matches(
            "https://example.com/api/*",
            "https://example.com/api/users"
        ));
        assert!(wildcard_matches(
            "*example.com*",
            "https://example.com/api/users"
        ));
        assert!(wildcard_matches(
            "https://example.com/api/users",
            "https://example.com/api/users"
        ));
        assert!(!wildcard_matches(
            "https://example.com/api/*",
            "https://other.com/api/users"
        ));
    }

    #[test]
    fn rewrite_join_preserves_path_suffix() {
        assert_eq!(
            join_rewrite_target("http://localhost:3000/mock", "/users?id=1"),
            "http://localhost:3000/mock/users?id=1"
        );
    }

    #[test]
    fn request_plan_merges_headers_and_rewrite() {
        let mut override_headers = BTreeMap::new();
        override_headers.insert("x-rub-env".to_string(), "dev".to_string());

        let rules = vec![
            NetworkRule {
                id: 1,
                status: NetworkRuleStatus::Active,
                spec: NetworkRuleSpec::HeaderOverride {
                    url_pattern: "https://example.com/api/*".to_string(),
                    headers: override_headers,
                },
            },
            NetworkRule {
                id: 2,
                status: NetworkRuleStatus::Active,
                spec: NetworkRuleSpec::Rewrite {
                    url_pattern: "https://example.com/api/*".to_string(),
                    target_base: "http://localhost:3000/mock".to_string(),
                },
            },
        ];

        let plan = build_request_plan(
            "https://example.com/api/users",
            &Headers::new(serde_json::json!({ "accept": "application/json" })),
            &rules,
        )
        .expect("request should match");

        assert_eq!(
            plan.applied_rule_effects,
            vec![
                NetworkRuleEffect {
                    rule_id: 1,
                    kind: NetworkRuleEffectKind::HeaderOverride,
                },
                NetworkRuleEffect {
                    rule_id: 2,
                    kind: NetworkRuleEffectKind::Rewrite,
                },
            ]
        );

        match plan.action {
            super::RequestPlanAction::Continue { url, headers } => {
                assert_eq!(url.as_deref(), Some("http://localhost:3000/mock/users"));
                let projected = headers
                    .expect("header overrides should be applied")
                    .into_iter()
                    .map(|entry| (entry.name, entry.value))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(
                    projected.get("accept").map(String::as_str),
                    Some("application/json")
                );
                assert_eq!(projected.get("x-rub-env").map(String::as_str), Some("dev"));
            }
            super::RequestPlanAction::Block => panic!("expected continue plan"),
        }
    }

    #[test]
    fn request_plan_ignores_non_active_rules() {
        let rules = vec![NetworkRule {
            id: 7,
            status: NetworkRuleStatus::Configured,
            spec: NetworkRuleSpec::Block {
                url_pattern: "https://example.com/api/*".to_string(),
            },
        }];

        let plan = build_request_plan(
            "https://example.com/api/users",
            &Headers::new(serde_json::json!({})),
            &rules,
        );

        assert!(
            plan.is_none(),
            "configured rules must not execute in browser runtime"
        );
    }

    #[test]
    fn headers_map_parses_string_values_from_network_headers() {
        let map = headers_to_map(&Headers::new(serde_json::json!({
            "accept": "application/json",
            "x-number": 1,
        })));
        assert_eq!(
            map.get("accept").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(map.get("x-number").map(String::as_str), Some("1"));
    }

    #[tokio::test]
    async fn prerecord_correlation_is_visible_before_actuation() {
        let registry = Arc::new(Mutex::new(RequestCorrelationRegistry::default()));
        let correlation = (
            Some("net-1".to_string()),
            RequestCorrelation {
                tab_target_id: Some("tab-1".to_string()),
                original_url: "https://example.com/original".to_string(),
                rewritten_url: Some("https://example.com/final".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 1,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        publish_request_correlation_before_actuation(&registry, "req-1", "GET", Some(&correlation))
            .await;

        let resolved = registry.lock().await.peek_for_request(
            "req-1",
            "https://example.com/final",
            "GET",
            None,
            Some("tab-1"),
        );
        assert!(
            resolved.is_some(),
            "pre-recorded correlation must be visible before request observatory callbacks run"
        );
    }

    #[tokio::test]
    async fn failed_actuation_rolls_back_prerecorded_correlation() {
        let registry = Arc::new(Mutex::new(RequestCorrelationRegistry::default()));
        let correlation = (
            Some("net-2".to_string()),
            RequestCorrelation {
                tab_target_id: Some("tab-2".to_string()),
                original_url: "https://example.com/original".to_string(),
                rewritten_url: Some("https://example.com/final".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 2,
                    kind: NetworkRuleEffectKind::Block,
                }],
            },
        );

        publish_request_correlation_before_actuation(&registry, "req-2", "GET", Some(&correlation))
            .await;
        rollback_request_correlation_after_failed_actuation(&registry, "req-2").await;

        let resolved = registry.lock().await.take_for_request(
            "req-2",
            "https://example.com/final",
            "GET",
            None,
            Some("tab-2"),
        );
        assert!(
            resolved.is_none(),
            "failed actuation must not leave pre-recorded correlation authority behind"
        );
    }

    #[tokio::test]
    async fn prerecorded_correlation_rolls_back_when_actuation_prepare_fails() {
        let registry = Arc::new(Mutex::new(RequestCorrelationRegistry::default()));
        let correlation = (
            Some("net-3".to_string()),
            RequestCorrelation {
                tab_target_id: Some("tab-3".to_string()),
                original_url: "https://example.com/original".to_string(),
                rewritten_url: Some("https://example.com/final".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 3,
                    kind: NetworkRuleEffectKind::Rewrite,
                }],
            },
        );

        publish_request_correlation_before_actuation(&registry, "req-3", "GET", Some(&correlation))
            .await;

        let result: Result<(), RubError> = run_request_actuation_with_prerecorded_correlation(
            &registry,
            "req-3",
            || -> Result<std::future::Ready<Result<(), RubError>>, RubError> {
                Err(RubError::Internal("prepare failed".to_string()))
            },
        )
        .await;

        assert!(result.is_err(), "prepare failure should propagate");
        let resolved = registry.lock().await.take_for_request(
            "req-3",
            "https://example.com/final",
            "GET",
            None,
            Some("tab-3"),
        );
        assert!(
            resolved.is_none(),
            "prepare failure must roll back pre-recorded correlation authority"
        );
    }

    #[tokio::test]
    async fn prerecorded_correlation_rolls_back_when_actuation_future_fails() {
        let registry = Arc::new(Mutex::new(RequestCorrelationRegistry::default()));
        let correlation = (
            Some("net-4".to_string()),
            RequestCorrelation {
                tab_target_id: Some("tab-4".to_string()),
                original_url: "https://example.com/original".to_string(),
                rewritten_url: Some("https://example.com/final".to_string()),
                effective_request_headers: None,
                applied_rule_effects: vec![NetworkRuleEffect {
                    rule_id: 4,
                    kind: NetworkRuleEffectKind::Block,
                }],
            },
        );

        publish_request_correlation_before_actuation(&registry, "req-4", "GET", Some(&correlation))
            .await;

        let result: Result<(), RubError> =
            run_request_actuation_with_prerecorded_correlation(&registry, "req-4", || {
                Ok(async {
                    Err(RubError::domain(
                        ErrorCode::BrowserCrashed,
                        "actuation failed",
                    ))
                })
            })
            .await;

        assert!(result.is_err(), "actuation failure should propagate");
        let resolved = registry.lock().await.take_for_request(
            "req-4",
            "https://example.com/final",
            "GET",
            None,
            Some("tab-4"),
        );
        assert!(
            resolved.is_none(),
            "future failure must roll back pre-recorded correlation authority"
        );
    }

    #[test]
    fn header_override_replaces_case_insensitive_names() {
        let mut override_headers = BTreeMap::new();
        override_headers.insert("Authorization".to_string(), "Bearer override".to_string());
        let rules = vec![NetworkRule {
            id: 1,
            status: NetworkRuleStatus::Active,
            spec: NetworkRuleSpec::HeaderOverride {
                url_pattern: "https://example.com/*".to_string(),
                headers: override_headers,
            },
        }];

        let plan = build_request_plan(
            "https://example.com/api",
            &Headers::new(serde_json::json!({ "authorization": "Bearer original" })),
            &rules,
        )
        .expect("request should match");

        match plan.action {
            super::RequestPlanAction::Continue { headers, .. } => {
                let projected = headers
                    .expect("header override should project")
                    .into_iter()
                    .map(|entry| (entry.name, entry.value))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(projected.len(), 1);
                assert_eq!(
                    projected.get("authorization").map(String::as_str),
                    Some("Bearer override")
                );
            }
            super::RequestPlanAction::Block => panic!("expected continue plan"),
        }
    }
}
