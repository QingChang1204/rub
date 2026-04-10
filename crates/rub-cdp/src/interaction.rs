mod confirmation;
mod observation;
mod preflight;

pub(crate) use confirmation::{
    confirm_click, confirm_click_xy, confirm_hover, confirm_input, confirm_key_combo,
    confirm_select, confirm_typed_text, confirm_typed_text_in_context, confirm_upload,
};
pub(crate) use observation::{
    capture_active_interaction_baseline, capture_active_interaction_baseline_in_context,
    capture_interaction_baseline, capture_page_baseline, capture_related_page_baseline,
    observe_element,
};
pub(crate) use preflight::{
    clear_text_input, ensure_activation_target_enabled, ensure_active_text_target_editable,
    ensure_active_text_target_editable_in_context, prepare_text_input,
};

#[cfg(test)]
mod tests {
    use super::observation::{
        ElementObservation, PageObservation, confirmation_observation_degraded,
        element_state_changed, is_context_replaced_error, page_changed, page_mutated,
        typed_effect_contradicted, typed_effect_observed,
    };

    #[test]
    fn page_mutated_detects_dom_fingerprint_changes() {
        let before = PageObservation {
            available: true,
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            element_count: Some(3),
            text_hash: Some(10),
            text_length: Some(12),
            markup_hash: Some(99),
            context_replaced: false,
        };
        let after = PageObservation {
            text_hash: Some(11),
            ..before.clone()
        };
        assert!(page_mutated(&before, &after));
    }

    #[test]
    fn page_mutated_detects_markup_only_changes() {
        let before = PageObservation {
            available: true,
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            element_count: Some(3),
            text_hash: Some(10),
            text_length: Some(12),
            markup_hash: Some(99),
            context_replaced: false,
        };
        let after = PageObservation {
            markup_hash: Some(100),
            ..before.clone()
        };
        assert!(page_mutated(&before, &after));
    }

    #[test]
    fn page_probe_loss_is_degraded_not_confirmed_mutation() {
        let before = PageObservation {
            available: true,
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            element_count: Some(3),
            text_hash: Some(10),
            text_length: Some(12),
            markup_hash: Some(99),
            context_replaced: false,
        };
        let after = PageObservation {
            available: false,
            url: None,
            title: None,
            element_count: None,
            text_hash: None,
            text_length: None,
            markup_hash: None,
            context_replaced: false,
        };
        assert!(!page_changed(&before, &after));
        assert!(!page_mutated(&before, &after));
        assert!(confirmation_observation_degraded(
            &before, &after, true, false
        ));
    }

    #[test]
    fn element_state_changed_detects_attribute_and_text_mutations() {
        let before = ElementObservation {
            text: Some("Open".to_string()),
            aria_expanded: Some("false".to_string()),
            ..ElementObservation::default()
        };
        let after = ElementObservation {
            text: Some("Close".to_string()),
            aria_expanded: Some("true".to_string()),
            ..ElementObservation::default()
        };
        assert!(element_state_changed(&before, &after));
    }

    #[test]
    fn context_replaced_error_patterns_are_classified() {
        assert!(is_context_replaced_error(
            "Protocol error (Runtime.callFunctionOn): Cannot find context with specified id"
        ));
        assert!(is_context_replaced_error(
            "Execution context was destroyed, most likely because of a navigation."
        ));
        assert!(!is_context_replaced_error("Some unrelated protocol error"));
    }

    #[test]
    fn typed_effect_observed_detects_value_growth() {
        let before = ElementObservation {
            value: Some("hel".to_string()),
            ..ElementObservation::default()
        };
        let after = ElementObservation {
            value: Some("hello".to_string()),
            ..ElementObservation::default()
        };
        assert!(typed_effect_observed(&before, &after, "lo"));
    }

    #[test]
    fn typed_effect_observed_detects_contenteditable_text_change() {
        let before = ElementObservation {
            text: Some("foo".to_string()),
            ..ElementObservation::default()
        };
        let after = ElementObservation {
            text: Some("foobar".to_string()),
            ..ElementObservation::default()
        };
        assert!(typed_effect_observed(&before, &after, "bar"));
    }

    #[test]
    fn typed_effect_observed_rejects_contradicted_value_replacement() {
        let before = ElementObservation {
            value: Some("".to_string()),
            ..ElementObservation::default()
        };
        let after = ElementObservation {
            value: Some("TEST USER".to_string()),
            ..ElementObservation::default()
        };
        assert!(!typed_effect_observed(&before, &after, "Test User"));
        assert!(typed_effect_contradicted(&before, &after, "Test User"));
    }
}
