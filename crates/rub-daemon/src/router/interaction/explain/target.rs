pub(super) fn interactability_target_summary(
    element: &rub_core::model::Element,
) -> serde_json::Value {
    serde_json::json!({
        "index": element.index,
        "tag": element.tag,
        "text": element.text,
        "role": interactability_role(element),
        "label": interactability_label(element),
        "element_ref": element.element_ref,
        "bbox": element.bounding_box,
        "flags": interactability_target_flags(element),
    })
}

pub(super) fn interactability_target_flags(
    element: &rub_core::model::Element,
) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if element.attributes.contains_key("disabled") {
        flags.push("disabled");
    }
    if element
        .attributes
        .get("aria-disabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        flags.push("aria_disabled");
    }
    if element.bounding_box.is_none() {
        flags.push("bbox_unavailable");
    }
    flags
}

fn interactability_role(element: &rub_core::model::Element) -> String {
    element
        .ax_info
        .as_ref()
        .and_then(|info| info.role.as_deref())
        .or_else(|| attribute_value(element, "role"))
        .unwrap_or(match element.tag {
            rub_core::model::ElementTag::Button => "button",
            rub_core::model::ElementTag::Link => "link",
            rub_core::model::ElementTag::Input | rub_core::model::ElementTag::TextArea => "textbox",
            rub_core::model::ElementTag::Select => "combobox",
            rub_core::model::ElementTag::Checkbox => "checkbox",
            rub_core::model::ElementTag::Radio => "radio",
            rub_core::model::ElementTag::Option => "option",
            rub_core::model::ElementTag::Other => "generic",
        })
        .trim()
        .to_string()
}

fn interactability_label(element: &rub_core::model::Element) -> String {
    element
        .ax_info
        .as_ref()
        .and_then(|info| info.accessible_name.as_deref())
        .or_else(|| non_empty_string(&element.text))
        .or_else(|| attribute_value(element, "aria-label"))
        .or_else(|| attribute_value(element, "placeholder"))
        .or_else(|| attribute_value(element, "name"))
        .or_else(|| attribute_value(element, "value"))
        .or_else(|| attribute_value(element, "title"))
        .or_else(|| attribute_value(element, "alt"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn attribute_value<'a>(element: &'a rub_core::model::Element, key: &str) -> Option<&'a str> {
    element
        .attributes
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn non_empty_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
