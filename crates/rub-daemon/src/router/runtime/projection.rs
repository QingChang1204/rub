use super::super::artifacts::{
    annotate_file_artifact_state, annotate_operator_path_reference_state,
};
use super::*;
use rub_core::model::Cookie;

pub(super) fn project_network_rules(rules: &[NetworkRule]) -> Vec<serde_json::Value> {
    rules.iter().map(project_network_rule).collect()
}

pub(super) fn project_network_rule(rule: &NetworkRule) -> serde_json::Value {
    let (action, pattern, extra) = match &rule.spec {
        NetworkRuleSpec::Rewrite {
            url_pattern,
            target_base,
        } => (
            "rewrite",
            url_pattern.as_str(),
            serde_json::json!({ "target_base": target_base }),
        ),
        NetworkRuleSpec::Block { url_pattern } => {
            ("block", url_pattern.as_str(), serde_json::json!({}))
        }
        NetworkRuleSpec::Allow { url_pattern } => {
            ("allow", url_pattern.as_str(), serde_json::json!({}))
        }
        NetworkRuleSpec::HeaderOverride {
            url_pattern,
            headers,
        } => (
            "header_override",
            url_pattern.as_str(),
            serde_json::json!({ "headers": headers }),
        ),
    };

    let mut value = serde_json::json!({
        "id": rule.id,
        "status": rule.status,
        "action": action,
        "pattern": pattern,
    });
    if let Some(object) = value.as_object_mut()
        && let Some(extra_object) = extra.as_object()
    {
        object.extend(extra_object.clone());
    }
    value
}

pub(super) fn intercept_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

pub(super) fn intercept_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "intercept_rule_registry",
    })
}

pub(super) fn intercept_rule_subject(rule: &NetworkRule) -> serde_json::Value {
    let (action, pattern) = match &rule.spec {
        NetworkRuleSpec::Rewrite { url_pattern, .. } => ("rewrite", url_pattern.as_str()),
        NetworkRuleSpec::Block { url_pattern } => ("block", url_pattern.as_str()),
        NetworkRuleSpec::Allow { url_pattern } => ("allow", url_pattern.as_str()),
        NetworkRuleSpec::HeaderOverride { url_pattern, .. } => {
            ("header_override", url_pattern.as_str())
        }
    };
    serde_json::json!({
        "kind": "intercept_rule",
        "action": action,
        "pattern": pattern,
    })
}

pub(super) fn intercept_rule_id_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "intercept_rule",
        "id": id,
    })
}

pub(super) fn cookie_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    artifact: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "subject": subject,
        "result": result,
    });
    if let Some(object) = payload.as_object_mut()
        && let Some(artifact) = artifact
    {
        object.insert("artifact".to_string(), artifact);
    }
    payload
}

pub(super) fn cookies_subject(url: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "cookies",
        "url": url,
    })
}

pub(super) fn cookie_subject(cookie: &Cookie) -> serde_json::Value {
    serde_json::json!({
        "kind": "cookie",
        "name": cookie.name,
        "domain": cookie.domain,
        "path": cookie.path,
    })
}

pub(super) fn cookie_artifact(path: &str, direction: &str, durability: &str) -> serde_json::Value {
    let mut artifact = serde_json::json!({
        "kind": "cookies_archive",
        "format": "json",
        "path": path,
        "direction": direction,
    });
    let (artifact_authority, upstream_truth) = match direction {
        "output" => ("router.cookies_export_artifact", "cookies_export_result"),
        "input" => ("router.cookies_import_artifact", "cookies_import_result"),
        _ => ("router.cookies_artifact", "cookies_result"),
    };
    annotate_file_artifact_state(
        &mut artifact,
        artifact_authority,
        upstream_truth,
        durability,
    );
    artifact
}

pub(super) fn runtime_surface_payload(
    subject: serde_json::Value,
    runtime_projection_state: serde_json::Value,
    runtime: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "runtime_projection_state": runtime_projection_state,
        "runtime": runtime,
    })
}

pub(super) fn runtime_subject(surface: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "runtime_surface",
        "surface": surface,
    })
}

pub(super) fn runtime_projection_state(
    surface: &str,
    projection_authority: &str,
) -> serde_json::Value {
    serde_json::json!({
        "surface": surface,
        "truth_level": "operator_projection",
        "projection_kind": "live_runtime_projection",
        "projection_authority": projection_authority,
        "upstream_truth": "session_live_runtime_state",
        "control_role": "display_only",
        "durability": "best_effort",
    })
}

pub(super) fn annotate_doctor_operator_path_states(result: &mut serde_json::Value) {
    if let Some(browser) = result.get_mut("browser") {
        annotate_operator_path_reference_state(
            browser,
            "path_state",
            "router.doctor.browser_path",
            "doctor_browser_report",
            "browser_binary_reference",
        );
    }
    if let Some(socket) = result.get_mut("socket") {
        annotate_operator_path_reference_state(
            socket,
            "path_state",
            "router.doctor.socket_path",
            "doctor_socket_report",
            "daemon_socket_reference",
        );
    }
    if let Some(disk) = result.get_mut("disk") {
        annotate_operator_path_reference_state(
            disk,
            "rub_home_state",
            "router.doctor.rub_home",
            "doctor_disk_report",
            "daemon_home_directory",
        );
    }
    if let Some(launch_policy) = result.get_mut("launch_policy") {
        if launch_policy
            .get("user_data_dir")
            .is_some_and(|value| !value.is_null())
        {
            annotate_operator_path_reference_state(
                launch_policy,
                "user_data_dir_state",
                "router.doctor.launch_policy.user_data_dir",
                "doctor_launch_policy",
                "managed_user_data_directory",
            );
        }
        if let Some(connection_target) = launch_policy.get_mut("connection_target")
            && connection_target
                .get("source")
                .and_then(|value| value.as_str())
                == Some("profile")
            && connection_target
                .get("resolved_path")
                .is_some_and(|value| !value.is_null())
        {
            annotate_operator_path_reference_state(
                connection_target,
                "resolved_path_state",
                "router.doctor.launch_policy.connection_target.resolved_path",
                "doctor_launch_policy_connection_target",
                "profile_directory_reference",
            );
        }
    }
}
