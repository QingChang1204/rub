mod automation;
mod binding;
mod command;
mod diff;
mod interaction;
mod runtime;
mod session;

pub use automation::*;
pub use binding::*;
pub use command::*;
pub use diff::*;
pub use interaction::*;
pub use runtime::*;
pub use session::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_result_success_serializes() {
        let result = CommandResult::success(
            "open",
            "default",
            "req-123",
            serde_json::json!({"url": "https://example.com"}),
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["command"], "open");
        assert_eq!(json["stdout_schema_version"], "3.0");
        assert!(json["error"].is_null());
    }

    #[test]
    fn command_result_error_serializes() {
        let envelope =
            crate::error::ErrorEnvelope::new(crate::error::ErrorCode::NavigationFailed, "DNS fail");
        let result = CommandResult::error("open", "default", "req-456", envelope);
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], "NAVIGATION_FAILED");
        assert!(json["data"].is_null());
    }

    #[test]
    fn command_result_contract_rejects_blank_command_id() {
        let result = CommandResult::success(
            "open",
            "default",
            "req-123",
            serde_json::json!({"ok": true}),
        )
        .with_command_id("   ");
        let error = result
            .contract_error_envelope()
            .expect("blank command_id should violate stdout contract");
        assert_eq!(error.code, crate::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("command_id")
        );
    }

    #[test]
    fn command_result_contract_rejects_success_with_error() {
        let mut result = CommandResult::success(
            "open",
            "default",
            "req-123",
            serde_json::json!({"ok": true}),
        );
        result.error = Some(crate::error::ErrorEnvelope::new(
            crate::error::ErrorCode::InvalidInput,
            "invalid",
        ));
        let error = result
            .contract_error_envelope()
            .expect("success with error should violate stdout contract");
        assert_eq!(error.code, crate::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["status"],
            serde_json::json!("success")
        );
    }

    #[test]
    fn load_strategy_serializes() {
        assert_eq!(
            serde_json::to_string(&LoadStrategy::DomContentLoaded).unwrap(),
            "\"domcontentloaded\""
        );
        assert_eq!(
            serde_json::to_string(&LoadStrategy::NetworkIdle).unwrap(),
            "\"networkidle\""
        );
    }

    #[test]
    fn element_tag_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ElementTag::Button).unwrap(),
            "\"button\""
        );
        assert_eq!(
            serde_json::to_string(&ElementTag::TextArea).unwrap(),
            "\"textarea\""
        );
    }

    #[test]
    fn key_combo_parse_single() {
        let combo = KeyCombo::parse("Enter").unwrap();
        assert_eq!(combo.key, "Enter");
        assert!(combo.modifiers.is_empty());
    }

    #[test]
    fn key_combo_parse_with_modifier() {
        let combo = KeyCombo::parse("Control+a").unwrap();
        assert_eq!(combo.key, "a");
        assert_eq!(combo.modifiers, vec![Modifier::Control]);
    }

    #[test]
    fn key_combo_parse_multiple_modifiers() {
        let combo = KeyCombo::parse("Control+Shift+Enter").unwrap();
        assert_eq!(combo.key, "Enter");
        assert_eq!(combo.modifiers.len(), 2);
        assert!(combo.modifiers.contains(&Modifier::Control));
        assert!(combo.modifiers.contains(&Modifier::Shift));
    }

    #[test]
    fn key_combo_parse_modifier_aliases() {
        let combo = KeyCombo::parse("Ctrl+a").unwrap();
        assert_eq!(combo.modifiers, vec![Modifier::Control]);

        let combo = KeyCombo::parse("Cmd+c").unwrap();
        assert_eq!(combo.modifiers, vec![Modifier::Meta]);
    }

    #[test]
    fn key_combo_parse_empty_error() {
        assert!(KeyCombo::parse("").is_err());
    }

    #[test]
    fn key_combo_parse_unknown_modifier_error() {
        let err = KeyCombo::parse("FooBar+a").unwrap_err();
        let envelope = err.into_envelope();
        assert_eq!(envelope.code, crate::error::ErrorCode::InvalidKeyName);
    }
}
