use super::intercept_args::{
    InterceptAction, InterceptHeaderArgs, InterceptRemoveArgs, InterceptRewriteArgs,
    InterceptUrlPatternArgs,
};
use super::*;
use crate::router::request_args::parse_json_args;
use std::collections::BTreeMap;

pub(super) async fn cmd_intercept(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match InterceptAction::parse(args)? {
        InterceptAction::List => {
            let rules = state.network_rules().await;
            Ok(intercept_payload(
                intercept_registry_subject(),
                serde_json::json!({
                    "rules": project_network_rules(&rules),
                }),
                serde_json::json!(state.integration_runtime().await),
            ))
        }
        InterceptAction::Rewrite => {
            let parsed = parse_json_args::<InterceptRewriteArgs>(args, "intercept rewrite")?;
            validate_rewrite_pattern(&parsed.source_pattern)?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Rewrite {
                    url_pattern: parsed.source_pattern,
                    target_base: parsed.target_base,
                },
            )
            .await
        }
        InterceptAction::Block => {
            let parsed = parse_json_args::<InterceptUrlPatternArgs>(args, "intercept block")?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Block {
                    url_pattern: parsed.url_pattern,
                },
            )
            .await
        }
        InterceptAction::Allow => {
            let parsed = parse_json_args::<InterceptUrlPatternArgs>(args, "intercept allow")?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::Allow {
                    url_pattern: parsed.url_pattern,
                },
            )
            .await
        }
        InterceptAction::Header => {
            let parsed = parse_json_args::<InterceptHeaderArgs>(args, "intercept header")?;
            let headers = parse_header_overrides(&parsed.headers)?;
            create_intercept_rule(
                router,
                state,
                NetworkRuleSpec::HeaderOverride {
                    url_pattern: parsed.url_pattern,
                    headers,
                },
            )
            .await
        }
        InterceptAction::Remove => {
            let parsed = parse_json_args::<InterceptRemoveArgs>(args, "intercept remove")?;
            remove_intercept_rule(router, state, parsed.id).await
        }
        InterceptAction::Clear => clear_intercept_rules(router, state).await,
    }
}

pub(super) async fn create_intercept_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    spec: NetworkRuleSpec,
) -> Result<serde_json::Value, RubError> {
    let mut rules = state.network_rules().await;
    let rule = NetworkRule {
        id: state.next_network_rule_id(),
        status: NetworkRuleStatus::Active,
        spec,
    };
    rules.push(rule.clone());
    commit_intercept_registry_change(
        router,
        state,
        intercept_rule_subject(&rule),
        serde_json::json!({
            "rule": project_network_rule(&rule),
            "rules": project_network_rules(&rules),
        }),
        rules,
    )
    .await
}

pub(super) async fn remove_intercept_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    id: u32,
) -> Result<serde_json::Value, RubError> {
    let current = state.network_rules().await;
    if !current.iter().any(|rule| rule.id == id) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Intercept rule {id} does not exist"),
        ));
    }

    let rules = current
        .into_iter()
        .filter(|rule| rule.id != id)
        .collect::<Vec<_>>();
    commit_intercept_registry_change(
        router,
        state,
        intercept_rule_id_subject(id),
        serde_json::json!({
            "removed_id": id,
            "rules": project_network_rules(&rules),
        }),
        rules,
    )
    .await
}

pub(super) async fn clear_intercept_rules(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    commit_intercept_registry_change(
        router,
        state,
        intercept_registry_subject(),
        serde_json::json!({
            "cleared": true,
            "rules": [],
        }),
        Vec::new(),
    )
    .await
}

async fn commit_intercept_registry_change(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    subject: serde_json::Value,
    result: serde_json::Value,
    rules: Vec<NetworkRule>,
) -> Result<serde_json::Value, RubError> {
    router.browser.sync_network_rules(&rules).await?;
    state.replace_network_rules(rules).await;
    Ok(intercept_payload(
        subject,
        result,
        serde_json::json!(state.integration_runtime().await),
    ))
}

pub(super) fn validate_rewrite_pattern(pattern: &str) -> Result<(), RubError> {
    let wildcard_count = pattern.matches('*').count();
    if wildcard_count > 1 || (wildcard_count == 1 && !pattern.ends_with('*')) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Rewrite patterns must be exact URLs or a single trailing-* prefix pattern",
        ));
    }
    Ok(())
}

pub(super) fn parse_header_overrides(
    raw_headers: &[String],
) -> Result<BTreeMap<String, String>, RubError> {
    if raw_headers.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Intercept header requires at least one --header NAME=VALUE entry",
        ));
    }

    let mut headers = BTreeMap::new();
    for entry in raw_headers {
        let Some((name, value)) = entry.split_once('=') else {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid header override '{entry}'. Use NAME=VALUE"),
            ));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid header override '{entry}'. Header name cannot be empty"),
            ));
        }
        headers.insert(name.to_string(), value.to_string());
    }

    Ok(headers)
}
