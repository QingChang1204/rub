pub(crate) use rub_daemon::workflow_params::resolve_workflow_parameters;

#[cfg(test)]
mod tests {
    use super::resolve_workflow_parameters;
    use rub_core::error::ErrorCode;

    const OPEN_URL_SPEC: &str = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}"}}]}"#;

    fn vars(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|value| (*value).to_string()).collect()
    }

    fn assert_invalid_input(error: rub_core::error::RubError) {
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_workflow_parameters_replaces_exact_leaf_placeholders() {
        let raw = r#"{"steps":[{"command":"open","args":{"url":"{{target_url}}","label":"prefix {{target_url}}"}}]}"#;
        let resolved = resolve_workflow_parameters(raw, &vars(&["target_url=https://example.com"]))
            .expect("parameters should resolve");

        let value: serde_json::Value =
            serde_json::from_str(&resolved.resolved_spec).expect("resolved json");
        assert_eq!(value["steps"][0]["args"]["url"], "https://example.com");
        assert_eq!(value["steps"][0]["args"]["label"], "prefix {{target_url}}");
        assert_eq!(resolved.parameter_keys, vec!["target_url"]);
    }

    #[test]
    fn resolve_workflow_parameters_preserves_secret_references_inside_values() {
        let raw = r#"{"steps":[{"command":"type","args":{"text":"{{password_ref}}"}}]}"#;
        let resolved = resolve_workflow_parameters(raw, &vars(&["password_ref=$RUB_PASSWORD"]))
            .expect("parameters should resolve");
        let value: serde_json::Value =
            serde_json::from_str(&resolved.resolved_spec).expect("resolved json");
        assert_eq!(value["steps"][0]["args"]["text"], "$RUB_PASSWORD");
    }

    #[test]
    fn resolve_workflow_parameters_reports_missing_placeholders() {
        let error = resolve_workflow_parameters(OPEN_URL_SPEC, &vars(&[]))
            .expect_err("missing vars should fail");
        assert_invalid_input(error);
    }

    #[test]
    fn resolve_workflow_parameters_rejects_duplicate_variables() {
        let error = resolve_workflow_parameters(
            OPEN_URL_SPEC,
            &vars(&[
                "target_url=https://a.example",
                "target_url=https://b.example",
            ]),
        )
        .expect_err("duplicate vars should fail");
        assert_invalid_input(error);
    }
}
