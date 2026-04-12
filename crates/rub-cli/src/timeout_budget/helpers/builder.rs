use super::*;

pub(super) fn parse_extract_builder_field(
    raw: &str,
    command: &str,
) -> Result<(String, serde_json::Value), RubError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand cannot be empty"),
        ));
    }

    let (name, shorthand) = match raw.split_once('=') {
        Some((name, shorthand)) => (name.trim(), Some(shorthand.trim())),
        None => (raw, None),
    };
    if name.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing a field name"),
        ));
    }

    let mut spec = serde_json::Map::new();
    let selection = shorthand
        .map(|value| parse_builder_field_selection(value, command, raw))
        .transpose()?
        .flatten();
    let shorthand = selection
        .as_ref()
        .map_or(shorthand, |selection| Some(selection.base.as_str()));
    match shorthand {
        None => {
            spec.insert("kind".to_string(), serde_json::json!("text"));
        }
        Some("") => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("{command} field shorthand '{raw}' is missing a selector or kind"),
            ));
        }
        Some(shorthand) => {
            if let Some(selector) = shorthand.strip_prefix("text:") {
                insert_builder_kind(&mut spec, "text", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("html:") {
                insert_builder_kind(&mut spec, "html", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("value:") {
                insert_builder_kind(&mut spec, "value", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("bbox:") {
                insert_builder_kind(&mut spec, "bbox", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("attributes:") {
                insert_builder_kind(&mut spec, "attributes", selector, command, raw)?;
            } else if let Some(rest) = shorthand.strip_prefix("attribute:") {
                let (attribute, selector) = match rest.split_once(':') {
                    Some((attribute, selector)) => (attribute.trim(), Some(selector.trim())),
                    None => (rest.trim(), None),
                };
                if attribute.is_empty() {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        format!("{command} field shorthand '{raw}' is missing an attribute name"),
                    ));
                }
                spec.insert("kind".to_string(), serde_json::json!("attribute"));
                spec.insert("attribute".to_string(), serde_json::json!(attribute));
                if let Some(selector) = selector {
                    insert_builder_locator(
                        &mut spec,
                        selector,
                        command,
                        raw,
                        "a selector or locator after the attribute name",
                    )?;
                }
            } else {
                insert_builder_kind(&mut spec, "text", shorthand, command, raw)?;
            }
        }
    }
    if let Some(selection) = selection {
        selection.apply(&mut spec);
    }

    Ok((name.to_string(), serde_json::Value::Object(spec)))
}

#[derive(Debug)]
struct BuilderFieldSelection {
    base: String,
    mode: BuilderFieldSelectionMode,
}

#[derive(Debug)]
enum BuilderFieldSelectionMode {
    First,
    Last,
    Many,
    Nth(u32),
}

impl BuilderFieldSelection {
    fn apply(self, spec: &mut serde_json::Map<String, serde_json::Value>) {
        match self.mode {
            BuilderFieldSelectionMode::First => {
                spec.insert("first".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Last => {
                spec.insert("last".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Many => {
                spec.insert("many".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Nth(nth) => {
                spec.insert("nth".to_string(), serde_json::json!(nth));
            }
        }
    }
}

fn parse_builder_field_selection(
    shorthand: &str,
    command: &str,
    raw: &str,
) -> Result<Option<BuilderFieldSelection>, RubError> {
    let selection = if let Some(base) = shorthand.strip_suffix("@first") {
        Some((base, BuilderFieldSelectionMode::First))
    } else if let Some(base) = shorthand.strip_suffix("@last") {
        Some((base, BuilderFieldSelectionMode::Last))
    } else if let Some(base) = shorthand.strip_suffix("@many") {
        Some((base, BuilderFieldSelectionMode::Many))
    } else if let Some((base, suffix)) = shorthand.rsplit_once('@') {
        if let Some(argument) = suffix
            .strip_prefix("nth(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let nth = argument.parse::<u32>().map_err(|_| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("{command} field shorthand '{raw}' has an invalid @nth(...) selection"),
                )
            })?;
            Some((base, BuilderFieldSelectionMode::Nth(nth)))
        } else {
            None
        }
    } else {
        None
    };

    let Some((base, mode)) = selection else {
        return Ok(None);
    };

    let base = base.trim();
    if base.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{command} field shorthand '{raw}' is missing a selector or kind before the match selection"
            ),
        ));
    }

    Ok(Some(BuilderFieldSelection {
        base: base.to_string(),
        mode,
    }))
}

fn insert_builder_kind(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    kind: &str,
    locator: &str,
    command: &str,
    raw: &str,
) -> Result<(), RubError> {
    spec.insert("kind".to_string(), serde_json::json!(kind));
    insert_builder_locator(spec, locator, command, raw, "a selector or locator")?;
    Ok(())
}

fn insert_builder_locator(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    locator: &str,
    command: &str,
    raw: &str,
    missing_description: &str,
) -> Result<(), RubError> {
    let locator = locator.trim();
    if locator.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing {missing_description}"),
        ));
    }

    if let Some(selector) = locator.strip_prefix("selector:") {
        return insert_named_builder_locator(
            spec,
            "selector",
            selector,
            command,
            raw,
            "a selector after 'selector:'",
        );
    }
    if let Some(target_text) = locator.strip_prefix("target_text:") {
        return insert_named_builder_locator(
            spec,
            "target_text",
            target_text,
            command,
            raw,
            "target text after 'target_text:'",
        );
    }
    if let Some(role) = locator.strip_prefix("role:") {
        return insert_named_builder_locator(
            spec,
            "role",
            role,
            command,
            raw,
            "a role after 'role:'",
        );
    }
    if let Some(label) = locator.strip_prefix("label:") {
        return insert_named_builder_locator(
            spec,
            "label",
            label,
            command,
            raw,
            "a label after 'label:'",
        );
    }
    if let Some(testid) = locator.strip_prefix("testid:") {
        return insert_named_builder_locator(
            spec,
            "testid",
            testid,
            command,
            raw,
            "a test id after 'testid:'",
        );
    }

    spec.insert("selector".to_string(), serde_json::json!(locator));
    Ok(())
}

fn insert_named_builder_locator(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &str,
    command: &str,
    raw: &str,
    missing_description: &str,
) -> Result<(), RubError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing {missing_description}"),
        ));
    }
    spec.insert(key.to_string(), serde_json::json!(value));
    Ok(())
}
