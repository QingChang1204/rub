use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSelection {
    First,
    Last,
    Nth(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum ObservationScope {
    Selector {
        css: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ObservationSelection>,
    },
    Role {
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ObservationSelection>,
    },
    Label {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ObservationSelection>,
    },
    TestId {
        testid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ObservationSelection>,
    },
}

impl ObservationScope {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Selector { .. } => "selector",
            Self::Role { .. } => "role",
            Self::Label { .. } => "label",
            Self::TestId { .. } => "test_id",
        }
    }

    pub fn probe_value(&self) -> String {
        match self {
            Self::Selector { css, .. } => css.clone(),
            Self::Role { role, .. } => role.clone(),
            Self::Label { label, .. } => label.clone(),
            Self::TestId { testid, .. } => testid.clone(),
        }
    }

    pub fn selection(&self) -> Option<ObservationSelection> {
        match self {
            Self::Selector { selection, .. }
            | Self::Role { selection, .. }
            | Self::Label { selection, .. }
            | Self::TestId { selection, .. } => *selection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ObservationScope;

    #[test]
    fn observation_scope_rejects_unknown_fields() {
        let error = serde_json::from_value::<ObservationScope>(serde_json::json!({
            "kind": "selector",
            "css": ".ready",
            "scop": "wide"
        }))
        .expect_err("unknown observation fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }
}
