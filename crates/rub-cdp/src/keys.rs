//! W3C UIEvents key name → CDP key definition mapping.
//! Reference: <https://www.w3.org/TR/uievents-key/>

/// CDP key definition used to construct `Input.dispatchKeyEvent` parameters.
#[derive(Debug, Clone)]
pub struct KeyDefinition {
    /// The `key` property (e.g., "Enter", "a").
    pub key: &'static str,
    /// The `code` property (e.g., "Enter", "KeyA").
    pub code: &'static str,
    /// Windows virtual key code.
    pub key_code: u32,
    /// Optional text to insert (e.g., "\r" for Enter).
    pub text: Option<&'static str>,
}

/// Look up a key definition by its W3C key name (case-insensitive for named keys).
/// Returns `None` if the key name is not recognized.
pub fn lookup(key_name: &str) -> Option<&'static KeyDefinition> {
    let lower = key_name.to_lowercase();
    NAMED_KEYS.iter().find(|k| k.key.to_lowercase() == lower)
}

/// Check if a string looks like a plain text input rather than a key name.
/// Returns `true` for multi-character strings that don't match any known
/// key name or modifier, suggesting the user meant `rub type`.
pub fn looks_like_plain_text(input: &str) -> bool {
    // Single character is a valid key press
    if input.chars().count() <= 1 {
        return false;
    }
    // If it contains +, it's a key combo attempt
    if input.contains('+') {
        return false;
    }
    // If it matches a known key name, it's not plain text
    if lookup(input).is_some() {
        return false;
    }
    // Multi-character string that's not a known key → probably plain text
    true
}

// ── Named Key Table ─────────────────────────────────────────────────────

static NAMED_KEYS: &[KeyDefinition] = &[
    // Navigation
    KeyDefinition {
        key: "Enter",
        code: "Enter",
        key_code: 13,
        text: Some("\r"),
    },
    KeyDefinition {
        key: "Tab",
        code: "Tab",
        key_code: 9,
        text: None,
    },
    KeyDefinition {
        key: "Escape",
        code: "Escape",
        key_code: 27,
        text: None,
    },
    KeyDefinition {
        key: "Backspace",
        code: "Backspace",
        key_code: 8,
        text: None,
    },
    KeyDefinition {
        key: "Delete",
        code: "Delete",
        key_code: 46,
        text: None,
    },
    KeyDefinition {
        key: " ",
        code: "Space",
        key_code: 32,
        text: Some(" "),
    },
    // Arrow keys
    KeyDefinition {
        key: "ArrowUp",
        code: "ArrowUp",
        key_code: 38,
        text: None,
    },
    KeyDefinition {
        key: "ArrowDown",
        code: "ArrowDown",
        key_code: 40,
        text: None,
    },
    KeyDefinition {
        key: "ArrowLeft",
        code: "ArrowLeft",
        key_code: 37,
        text: None,
    },
    KeyDefinition {
        key: "ArrowRight",
        code: "ArrowRight",
        key_code: 39,
        text: None,
    },
    // Page navigation
    KeyDefinition {
        key: "Home",
        code: "Home",
        key_code: 36,
        text: None,
    },
    KeyDefinition {
        key: "End",
        code: "End",
        key_code: 35,
        text: None,
    },
    KeyDefinition {
        key: "PageUp",
        code: "PageUp",
        key_code: 33,
        text: None,
    },
    KeyDefinition {
        key: "PageDown",
        code: "PageDown",
        key_code: 34,
        text: None,
    },
    // Function keys
    KeyDefinition {
        key: "F1",
        code: "F1",
        key_code: 112,
        text: None,
    },
    KeyDefinition {
        key: "F2",
        code: "F2",
        key_code: 113,
        text: None,
    },
    KeyDefinition {
        key: "F3",
        code: "F3",
        key_code: 114,
        text: None,
    },
    KeyDefinition {
        key: "F4",
        code: "F4",
        key_code: 115,
        text: None,
    },
    KeyDefinition {
        key: "F5",
        code: "F5",
        key_code: 116,
        text: None,
    },
    KeyDefinition {
        key: "F6",
        code: "F6",
        key_code: 117,
        text: None,
    },
    KeyDefinition {
        key: "F7",
        code: "F7",
        key_code: 118,
        text: None,
    },
    KeyDefinition {
        key: "F8",
        code: "F8",
        key_code: 119,
        text: None,
    },
    KeyDefinition {
        key: "F9",
        code: "F9",
        key_code: 120,
        text: None,
    },
    KeyDefinition {
        key: "F10",
        code: "F10",
        key_code: 121,
        text: None,
    },
    KeyDefinition {
        key: "F11",
        code: "F11",
        key_code: 122,
        text: None,
    },
    KeyDefinition {
        key: "F12",
        code: "F12",
        key_code: 123,
        text: None,
    },
    // Insert / editing
    KeyDefinition {
        key: "Insert",
        code: "Insert",
        key_code: 45,
        text: None,
    },
    // Letters (a-z) — key_code uses uppercase ASCII
    KeyDefinition {
        key: "a",
        code: "KeyA",
        key_code: 65,
        text: Some("a"),
    },
    KeyDefinition {
        key: "b",
        code: "KeyB",
        key_code: 66,
        text: Some("b"),
    },
    KeyDefinition {
        key: "c",
        code: "KeyC",
        key_code: 67,
        text: Some("c"),
    },
    KeyDefinition {
        key: "d",
        code: "KeyD",
        key_code: 68,
        text: Some("d"),
    },
    KeyDefinition {
        key: "e",
        code: "KeyE",
        key_code: 69,
        text: Some("e"),
    },
    KeyDefinition {
        key: "f",
        code: "KeyF",
        key_code: 70,
        text: Some("f"),
    },
    KeyDefinition {
        key: "g",
        code: "KeyG",
        key_code: 71,
        text: Some("g"),
    },
    KeyDefinition {
        key: "h",
        code: "KeyH",
        key_code: 72,
        text: Some("h"),
    },
    KeyDefinition {
        key: "i",
        code: "KeyI",
        key_code: 73,
        text: Some("i"),
    },
    KeyDefinition {
        key: "j",
        code: "KeyJ",
        key_code: 74,
        text: Some("j"),
    },
    KeyDefinition {
        key: "k",
        code: "KeyK",
        key_code: 75,
        text: Some("k"),
    },
    KeyDefinition {
        key: "l",
        code: "KeyL",
        key_code: 76,
        text: Some("l"),
    },
    KeyDefinition {
        key: "m",
        code: "KeyM",
        key_code: 77,
        text: Some("m"),
    },
    KeyDefinition {
        key: "n",
        code: "KeyN",
        key_code: 78,
        text: Some("n"),
    },
    KeyDefinition {
        key: "o",
        code: "KeyO",
        key_code: 79,
        text: Some("o"),
    },
    KeyDefinition {
        key: "p",
        code: "KeyP",
        key_code: 80,
        text: Some("p"),
    },
    KeyDefinition {
        key: "q",
        code: "KeyQ",
        key_code: 81,
        text: Some("q"),
    },
    KeyDefinition {
        key: "r",
        code: "KeyR",
        key_code: 82,
        text: Some("r"),
    },
    KeyDefinition {
        key: "s",
        code: "KeyS",
        key_code: 83,
        text: Some("s"),
    },
    KeyDefinition {
        key: "t",
        code: "KeyT",
        key_code: 84,
        text: Some("t"),
    },
    KeyDefinition {
        key: "u",
        code: "KeyU",
        key_code: 85,
        text: Some("u"),
    },
    KeyDefinition {
        key: "v",
        code: "KeyV",
        key_code: 86,
        text: Some("v"),
    },
    KeyDefinition {
        key: "w",
        code: "KeyW",
        key_code: 87,
        text: Some("w"),
    },
    KeyDefinition {
        key: "x",
        code: "KeyX",
        key_code: 88,
        text: Some("x"),
    },
    KeyDefinition {
        key: "y",
        code: "KeyY",
        key_code: 89,
        text: Some("y"),
    },
    KeyDefinition {
        key: "z",
        code: "KeyZ",
        key_code: 90,
        text: Some("z"),
    },
    // Digits
    KeyDefinition {
        key: "0",
        code: "Digit0",
        key_code: 48,
        text: Some("0"),
    },
    KeyDefinition {
        key: "1",
        code: "Digit1",
        key_code: 49,
        text: Some("1"),
    },
    KeyDefinition {
        key: "2",
        code: "Digit2",
        key_code: 50,
        text: Some("2"),
    },
    KeyDefinition {
        key: "3",
        code: "Digit3",
        key_code: 51,
        text: Some("3"),
    },
    KeyDefinition {
        key: "4",
        code: "Digit4",
        key_code: 52,
        text: Some("4"),
    },
    KeyDefinition {
        key: "5",
        code: "Digit5",
        key_code: 53,
        text: Some("5"),
    },
    KeyDefinition {
        key: "6",
        code: "Digit6",
        key_code: 54,
        text: Some("6"),
    },
    KeyDefinition {
        key: "7",
        code: "Digit7",
        key_code: 55,
        text: Some("7"),
    },
    KeyDefinition {
        key: "8",
        code: "Digit8",
        key_code: 56,
        text: Some("8"),
    },
    KeyDefinition {
        key: "9",
        code: "Digit9",
        key_code: 57,
        text: Some("9"),
    },
];

/// CDP modifier flag bitmask values.
pub mod modifiers {
    pub const ALT: u32 = 1;
    pub const CONTROL: u32 = 2;
    pub const META: u32 = 4;
    pub const SHIFT: u32 = 8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_named_key() {
        let def = lookup("Enter").unwrap();
        assert_eq!(def.key_code, 13);
        assert_eq!(def.text, Some("\r"));
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(lookup("enter").is_some());
        assert!(lookup("ESCAPE").is_some());
        assert!(lookup("arrowdown").is_some());
    }

    #[test]
    fn lookup_letter() {
        let def = lookup("a").unwrap();
        assert_eq!(def.code, "KeyA");
        assert_eq!(def.key_code, 65);
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("FooBar").is_none());
    }

    #[test]
    fn plain_text_detection() {
        assert!(looks_like_plain_text("hello"));
        assert!(looks_like_plain_text("Hello World"));
        assert!(!looks_like_plain_text("Enter"));
        assert!(!looks_like_plain_text("a"));
        assert!(!looks_like_plain_text("Control+a"));
    }
}
