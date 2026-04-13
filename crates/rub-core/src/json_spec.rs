use crate::error::{ErrorCode, RubError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedJsonSpec {
    value: Value,
}

impl NormalizedJsonSpec {
    pub fn from_raw_str(raw: &str, command: &str) -> Result<Self, RubError> {
        let value = serde_json::from_str(raw).map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Invalid JSON spec for '{command}': {error}"),
            )
        })?;
        Ok(Self { value })
    }

    pub fn from_value(value: Value) -> Self {
        Self { value }
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn to_canonical_string(&self) -> Result<String, RubError> {
        serde_json::to_string(&self.value).map_err(RubError::from)
    }
}

impl Serialize for NormalizedJsonSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NormalizedJsonSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        match raw {
            Value::String(spec) => {
                let value = serde_json::from_str(&spec).map_err(serde::de::Error::custom)?;
                Ok(Self { value })
            }
            value => Ok(Self { value }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NormalizedJsonSpec;

    #[test]
    fn normalized_json_spec_accepts_string_and_structured_json() {
        let from_string = serde_json::from_value::<NormalizedJsonSpec>(serde_json::json!("[]"))
            .expect("string spec should parse");
        assert_eq!(from_string.as_value(), &serde_json::json!([]));

        let from_structured =
            serde_json::from_value::<NormalizedJsonSpec>(serde_json::json!([{ "kind": "text" }]))
                .expect("structured spec should parse");
        assert_eq!(
            from_structured.as_value(),
            &serde_json::json!([{ "kind": "text" }])
        );
    }

    #[test]
    fn normalized_json_spec_serializes_as_structured_json() {
        let spec = NormalizedJsonSpec::from_value(serde_json::json!({
            "items": {
                "collection": ".mail-row"
            }
        }));
        assert_eq!(
            serde_json::to_value(spec).expect("serialize spec"),
            serde_json::json!({
                "items": {
                    "collection": ".mail-row"
                }
            })
        );
    }
}
