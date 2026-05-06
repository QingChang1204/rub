use crate::protocol::IpcResponse;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestCorrelation {
    pub command_id: Option<String>,
    pub daemon_session_id: Option<String>,
}

impl RequestCorrelation {
    pub fn from_request_value(value: &serde_json::Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            command_id: sanitize_optional_protocol_string(object.get("command_id")),
            daemon_session_id: sanitize_optional_protocol_string(object.get("daemon_session_id")),
        }
    }

    pub fn from_request_frame(frame: &[u8]) -> Self {
        Self {
            command_id: recover_top_level_string_field_from_frame(frame, "command_id"),
            daemon_session_id: recover_top_level_string_field_from_frame(
                frame,
                "daemon_session_id",
            ),
        }
    }

    pub fn attach_to_response(
        self,
        mut response: IpcResponse,
        authoritative_daemon_session_id: Option<&str>,
    ) -> IpcResponse {
        if let Some(command_id) = self.command_id {
            response = response
                .with_command_id(command_id)
                .expect("sanitized ingress command_id must remain protocol-valid");
        }
        if let Some(daemon_session_id) = authoritative_daemon_session_id {
            response = response
                .with_daemon_session_id(daemon_session_id.to_string())
                .expect("authoritative daemon_session_id must remain protocol-valid");
        }
        response
    }
}

fn sanitize_optional_protocol_string(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value.and_then(serde_json::Value::as_str)?;
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn recover_top_level_string_field_from_frame(frame: &[u8], field: &str) -> Option<String> {
    let bytes = frame;
    let mut cursor = 0usize;
    skip_json_whitespace(bytes, &mut cursor);
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor += 1;

    loop {
        skip_json_whitespace(bytes, &mut cursor);
        match bytes.get(cursor) {
            Some(b'}') | None => return None,
            Some(b'"') => {}
            _ => return None,
        }

        let key = parse_json_string_token(bytes, &mut cursor)?;
        skip_json_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b':') {
            return None;
        }
        cursor += 1;
        skip_json_whitespace(bytes, &mut cursor);

        if key == field {
            let value = parse_json_string_token(bytes, &mut cursor)?;
            return (!value.trim().is_empty()).then_some(value);
        }

        skip_json_value_token(bytes, &mut cursor)?;
        skip_json_whitespace(bytes, &mut cursor);
        match bytes.get(cursor) {
            Some(b',') => {
                cursor += 1;
            }
            Some(b'}') | None => return None,
            _ => return None,
        }
    }
}

fn skip_json_whitespace(bytes: &[u8], cursor: &mut usize) {
    while matches!(bytes.get(*cursor), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        *cursor += 1;
    }
}

fn parse_json_string_token(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    if bytes.get(*cursor) != Some(&b'"') {
        return None;
    }
    let start = *cursor;
    *cursor += 1;
    let mut escaped = false;
    while let Some(&byte) = bytes.get(*cursor) {
        *cursor += 1;
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'"' => {
                return serde_json::from_slice::<String>(&bytes[start..*cursor]).ok();
            }
            _ => {}
        }
    }
    None
}

fn skip_json_value_token(bytes: &[u8], cursor: &mut usize) -> Option<()> {
    match bytes.get(*cursor) {
        Some(b'"') => {
            parse_json_string_token(bytes, cursor)?;
            Some(())
        }
        Some(b'{') => skip_nested_json_structure(bytes, cursor, b'{', b'}'),
        Some(b'[') => skip_nested_json_structure(bytes, cursor, b'[', b']'),
        Some(_) => {
            while let Some(&byte) = bytes.get(*cursor) {
                if matches!(byte, b',' | b'}') {
                    break;
                }
                *cursor += 1;
            }
            Some(())
        }
        None => None,
    }
}

fn skip_nested_json_structure(bytes: &[u8], cursor: &mut usize, open: u8, close: u8) -> Option<()> {
    if bytes.get(*cursor) != Some(&open) {
        return None;
    }
    let mut depth = 0usize;
    let mut escaped = false;
    let mut in_string = false;
    while let Some(&byte) = bytes.get(*cursor) {
        *cursor += 1;
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            value if value == open => depth += 1,
            value if value == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(());
                }
            }
            _ => {}
        }
    }
    None
}
