use rub_core::model::{Element, ElementTag};

pub(super) fn fallback_role(element: &Element) -> &'static str {
    match element.tag {
        ElementTag::Button => "button",
        ElementTag::Link => "link",
        ElementTag::Input | ElementTag::TextArea => "textbox",
        ElementTag::Select => "combobox",
        ElementTag::Checkbox => "checkbox",
        ElementTag::Radio => "radio",
        ElementTag::Option => "option",
        ElementTag::Other => {
            if is_content_editable(element) {
                "textbox"
            } else {
                "generic"
            }
        }
    }
}

pub(super) fn semantic_role(element: &Element) -> String {
    element
        .ax_info
        .as_ref()
        .and_then(|info| info.role.as_deref())
        .or_else(|| attr(element, "role"))
        .unwrap_or_else(|| fallback_role(element))
        .trim()
        .to_string()
}

pub(super) fn accessible_label(element: &Element) -> String {
    element
        .ax_info
        .as_ref()
        .and_then(|info| info.accessible_name.as_deref())
        .or_else(|| non_empty(&element.text))
        .or_else(|| attr(element, "aria-label"))
        .or_else(|| attr(element, "placeholder"))
        .or_else(|| attr(element, "name"))
        .or_else(|| attr(element, "value"))
        .or_else(|| attr(element, "title"))
        .or_else(|| attr(element, "alt"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub(super) fn test_id(element: &Element) -> Option<&str> {
    for key in ["data-testid", "data-test-id", "data-test"] {
        if let Some(value) = attr(element, key) {
            return Some(value);
        }
    }
    None
}

pub(super) fn is_content_editable(element: &Element) -> bool {
    element
        .attributes
        .get("contenteditable")
        .map(|value| !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub(super) fn editor_safe_text_target_kind(element: &Element) -> Option<&'static str> {
    match element.tag {
        ElementTag::Input | ElementTag::TextArea => Some("plain_text_control"),
        ElementTag::Other if is_content_editable(element) => {
            if attr_is(element, "role", "textbox") {
                Some("semantic_textbox")
            } else {
                Some("contenteditable")
            }
        }
        _ => None,
    }
}

pub(super) fn has_snapshot_visible_bbox(element: &Element) -> bool {
    element
        .bounding_box
        .is_some_and(|bbox| bbox.width > 0.0 && bbox.height > 0.0)
}

pub(super) fn is_disabled_in_snapshot(element: &Element) -> bool {
    element.attributes.contains_key("disabled") || attr_is(element, "aria-disabled", "true")
}

pub(super) fn is_readonly_in_snapshot(element: &Element) -> bool {
    editor_safe_text_target_kind(element).is_some()
        && (element.attributes.contains_key("readonly")
            || attr_is(element, "aria-readonly", "true"))
}

pub(super) fn is_prefer_enabled_blocked_in_snapshot(element: &Element) -> bool {
    is_disabled_in_snapshot(element) || is_readonly_in_snapshot(element)
}

pub(super) fn attr<'a>(element: &'a Element, key: &str) -> Option<&'a str> {
    element
        .attributes
        .get(key)
        .and_then(|value| non_empty(value))
}

pub(super) fn attr_is(element: &Element, key: &str, expected: &str) -> bool {
    element
        .attributes
        .get(key)
        .map(|value| value.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::{
        accessible_label, editor_safe_text_target_kind, has_snapshot_visible_bbox,
        is_content_editable, is_disabled_in_snapshot, is_prefer_enabled_blocked_in_snapshot,
        is_readonly_in_snapshot, semantic_role, test_id,
    };
    use rub_core::model::{AXInfo, BoundingBox, Element, ElementTag};
    use std::collections::HashMap;

    #[test]
    fn accessible_label_prefers_a11y_then_textish_fallbacks() {
        let mut element = sample_element();
        element.text = "Visible Text".to_string();
        element
            .attributes
            .insert("aria-label".to_string(), "Aria Label".to_string());
        element.ax_info = Some(AXInfo {
            role: Some("button".to_string()),
            accessible_name: Some("Accessible Name".to_string()),
            accessible_description: None,
        });
        assert_eq!(accessible_label(&element), "Accessible Name");
    }

    #[test]
    fn semantic_role_prefers_explicit_role() {
        let mut element = sample_element();
        element
            .attributes
            .insert("role".to_string(), "tab".to_string());
        assert_eq!(semantic_role(&element), "tab");
    }

    #[test]
    fn test_id_reads_common_testing_attributes() {
        let mut element = sample_element();
        element
            .attributes
            .insert("data-testid".to_string(), "hero-cta".to_string());
        assert_eq!(test_id(&element), Some("hero-cta"));
    }

    #[test]
    fn contenteditable_other_falls_back_to_textbox_role() {
        let mut element = sample_element();
        element.tag = ElementTag::Other;
        element
            .attributes
            .insert("contenteditable".to_string(), "true".to_string());
        assert!(is_content_editable(&element));
        assert_eq!(semantic_role(&element), "textbox");
        assert_eq!(
            editor_safe_text_target_kind(&element),
            Some("contenteditable")
        );
    }

    #[test]
    fn semantic_textbox_is_distinguished_for_editor_safe_writes() {
        let mut element = sample_element();
        element.tag = ElementTag::Other;
        element
            .attributes
            .insert("contenteditable".to_string(), "plaintext-only".to_string());
        element
            .attributes
            .insert("role".to_string(), "textbox".to_string());
        assert_eq!(semantic_role(&element), "textbox");
        assert_eq!(
            editor_safe_text_target_kind(&element),
            Some("semantic_textbox")
        );
    }

    #[test]
    fn visible_bbox_requires_non_zero_area() {
        let mut element = sample_element();
        element.bounding_box = Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 0.0,
            height: 15.0,
        });
        assert!(!has_snapshot_visible_bbox(&element));
        element.bounding_box = Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 40.0,
            height: 15.0,
        });
        assert!(has_snapshot_visible_bbox(&element));
    }

    #[test]
    fn disabled_state_honors_disabled_and_aria_disabled() {
        let mut element = sample_element();
        assert!(!is_disabled_in_snapshot(&element));
        element
            .attributes
            .insert("aria-disabled".to_string(), "true".to_string());
        assert!(is_disabled_in_snapshot(&element));
    }

    #[test]
    fn readonly_state_only_applies_to_writable_targets() {
        let mut input = sample_element();
        input.tag = ElementTag::Input;
        assert!(!is_readonly_in_snapshot(&input));
        input
            .attributes
            .insert("readonly".to_string(), String::new());
        assert!(is_readonly_in_snapshot(&input));
        assert!(is_prefer_enabled_blocked_in_snapshot(&input));

        let mut editor = sample_element();
        editor.tag = ElementTag::Other;
        editor
            .attributes
            .insert("contenteditable".to_string(), "true".to_string());
        editor
            .attributes
            .insert("aria-readonly".to_string(), "true".to_string());
        assert!(is_readonly_in_snapshot(&editor));
        assert!(is_prefer_enabled_blocked_in_snapshot(&editor));

        let mut button = sample_element();
        button
            .attributes
            .insert("aria-readonly".to_string(), "true".to_string());
        assert!(!is_readonly_in_snapshot(&button));
        assert!(!is_prefer_enabled_blocked_in_snapshot(&button));
    }

    fn sample_element() -> Element {
        Element {
            index: 0,
            tag: ElementTag::Button,
            text: String::new(),
            attributes: HashMap::new(),
            element_ref: Some("main:1".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: Some(0),
        }
    }
}
