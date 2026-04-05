use rub_core::model::InterferenceMode;

pub(crate) fn active_policies_for_mode(mode: InterferenceMode) -> Vec<String> {
    match mode {
        InterferenceMode::Normal => Vec::new(),
        InterferenceMode::PublicWebStable => vec![
            "safe_recovery".to_string(),
            "handoff_escalation".to_string(),
        ],
        InterferenceMode::Strict => vec![
            "safe_recovery".to_string(),
            "handoff_escalation".to_string(),
            "strict_containment".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::active_policies_for_mode;
    use rub_core::model::InterferenceMode;

    #[test]
    fn active_policies_are_canonical_per_mode() {
        assert!(active_policies_for_mode(InterferenceMode::Normal).is_empty());
        assert_eq!(
            active_policies_for_mode(InterferenceMode::PublicWebStable),
            vec!["safe_recovery", "handoff_escalation"]
        );
        assert_eq!(
            active_policies_for_mode(InterferenceMode::Strict),
            vec!["safe_recovery", "handoff_escalation", "strict_containment"]
        );
    }
}
