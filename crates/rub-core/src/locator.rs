use serde::{Deserialize, Serialize, de};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocatorSelection {
    First,
    Last,
    Nth(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum CanonicalLocator {
    Index {
        index: u32,
    },
    Ref {
        element_ref: String,
    },
    Selector {
        css: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<LocatorSelection>,
    },
    TargetText {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<LocatorSelection>,
    },
    Role {
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<LocatorSelection>,
    },
    Label {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<LocatorSelection>,
    },
    TestId {
        testid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<LocatorSelection>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LiveLocator(CanonicalLocator);

impl LiveLocator {
    pub fn as_canonical(&self) -> &CanonicalLocator {
        &self.0
    }

    pub fn into_canonical(self) -> CanonicalLocator {
        self.0
    }

    pub fn kind_name(&self) -> &'static str {
        self.0.kind_name()
    }

    pub fn probe_value(&self) -> String {
        self.0.probe_value()
    }

    pub fn selection(&self) -> Option<LocatorSelection> {
        self.0.selection()
    }
}

impl std::ops::Deref for LiveLocator {
    type Target = CanonicalLocator;

    fn deref(&self) -> &Self::Target {
        self.as_canonical()
    }
}

impl AsRef<CanonicalLocator> for LiveLocator {
    fn as_ref(&self) -> &CanonicalLocator {
        self.as_canonical()
    }
}

impl TryFrom<CanonicalLocator> for LiveLocator {
    type Error = CanonicalLocator;

    fn try_from(locator: CanonicalLocator) -> Result<Self, Self::Error> {
        match locator {
            CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. } => Err(locator),
            other => Ok(Self(other)),
        }
    }
}

impl<'de> Deserialize<'de> for LiveLocator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let locator = CanonicalLocator::deserialize(deserializer)?;
        LiveLocator::try_from(locator).map_err(|invalid| {
            de::Error::custom(format!(
                "locator kind '{}' is not live-query compatible",
                invalid.kind_name()
            ))
        })
    }
}

impl CanonicalLocator {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Index { .. } => "index",
            Self::Ref { .. } => "ref",
            Self::Selector { .. } => "selector",
            Self::TargetText { .. } => "target_text",
            Self::Role { .. } => "role",
            Self::Label { .. } => "label",
            Self::TestId { .. } => "test_id",
        }
    }

    pub fn probe_value(&self) -> String {
        match self {
            Self::Index { index } => index.to_string(),
            Self::Ref { element_ref } => element_ref.clone(),
            Self::Selector { css, .. } => css.clone(),
            Self::TargetText { text, .. } => text.clone(),
            Self::Role { role, .. } => role.clone(),
            Self::Label { label, .. } => label.clone(),
            Self::TestId { testid, .. } => testid.clone(),
        }
    }

    pub fn selection(&self) -> Option<LocatorSelection> {
        match self {
            Self::Index { .. } | Self::Ref { .. } => None,
            Self::Selector { selection, .. }
            | Self::TargetText { selection, .. }
            | Self::Role { selection, .. }
            | Self::Label { selection, .. }
            | Self::TestId { selection, .. } => *selection,
        }
    }

    pub fn requires_a11y_snapshot(&self) -> bool {
        matches!(self, Self::Role { .. } | Self::Label { .. })
    }

    pub fn supports_selection(&self) -> bool {
        !matches!(self, Self::Index { .. } | Self::Ref { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalLocator, LiveLocator, LocatorSelection};

    #[test]
    fn canonical_locator_rejects_unknown_variant_fields() {
        let error = serde_json::from_value::<CanonicalLocator>(serde_json::json!({
            "kind": "selector",
            "css": "#login",
            "selction": { "nth": 0 }
        }))
        .expect_err("unknown locator fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn live_locator_rejects_unknown_variant_fields() {
        let error = serde_json::from_value::<LiveLocator>(serde_json::json!({
            "kind": "role",
            "role": "button",
            "rol": "button"
        }))
        .expect_err("unknown live-locator fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn live_locator_deserialize_rejects_ref_locator() {
        let error = serde_json::from_value::<LiveLocator>(serde_json::json!({
            "kind": "ref",
            "element_ref": "frame-1:42"
        }))
        .expect_err("ref locator should be rejected");
        assert!(error.to_string().contains("not live-query compatible"));
    }

    #[test]
    fn live_locator_deserialize_accepts_selector_locator() {
        let locator = serde_json::from_value::<LiveLocator>(serde_json::json!({
            "kind": "selector",
            "css": ".cta",
            "selection": "first"
        }))
        .expect("selector locator should deserialize");
        assert_eq!(
            locator.as_canonical(),
            &CanonicalLocator::Selector {
                css: ".cta".to_string(),
                selection: Some(LocatorSelection::First),
            }
        );
    }
}
