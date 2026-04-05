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
        ElementTag::Other => "generic",
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
    use super::{accessible_label, semantic_role, test_id};
    use rub_core::model::{AXInfo, Element, ElementTag};
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
