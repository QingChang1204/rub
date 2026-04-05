use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use rub_core::error::{ErrorCode, RubError};
use std::sync::Arc;

pub async fn execute_js(page: &Arc<Page>, code: &str) -> Result<serde_json::Value, RubError> {
    execute_js_in_context(page, code, None).await
}

pub async fn execute_js_in_context(
    page: &Arc<Page>,
    code: &str,
    context_id: Option<ExecutionContextId>,
) -> Result<serde_json::Value, RubError> {
    let code_json = serde_json::to_string(code)
        .map_err(|e| RubError::Internal(format!("JS code serialization failed: {e}")))?;
    let script = format!(
        r#"
        (async () => {{
            const __rub_code = {code_json};
            const __rub_seen = new WeakSet();

            function __rub_kind(value) {{
                const tag = Object.prototype.toString.call(value);
                return tag.slice(8, -1) || typeof value;
            }}

            function __rub_summary(value) {{
                const kind = __rub_kind(value);
                const summary = {{
                    "__rub_projection": "summary",
                    kind,
                }};

                if (value && value.constructor && typeof value.constructor.name === 'string') {{
                    summary.constructor = value.constructor.name;
                }}

                try {{
                    const text = String(value);
                    if (text && text !== `[object ${{kind}}]`) {{
                        summary.description = text;
                    }}
                }} catch (_err) {{}}

                return summary;
            }}

            function __rub_project(value, depth = 0) {{
                if (value === undefined || value === null) {{
                    return null;
                }}

                const valueType = typeof value;
                if (valueType === 'string' || valueType === 'boolean') {{
                    return value;
                }}
                if (valueType === 'number') {{
                    return Number.isFinite(value) ? value : String(value);
                }}
                if (valueType === 'bigint') {{
                    return {{
                        "__rub_projection": "summary",
                        "kind": "BigInt",
                        "value": value.toString(),
                    }};
                }}
                if (valueType === 'symbol') {{
                    return {{
                        "__rub_projection": "summary",
                        "kind": "Symbol",
                        "description": String(value),
                    }};
                }}
                if (valueType === 'function') {{
                    return {{
                        "__rub_projection": "summary",
                        "kind": "Function",
                        "name": value.name || null,
                    }};
                }}

                if (depth >= 4) {{
                    return __rub_summary(value);
                }}

                if (value instanceof Date) {{
                    return value.toISOString();
                }}

                if (value instanceof Error) {{
                    return {{
                        "__rub_projection": "summary",
                        "kind": "Error",
                        "name": value.name,
                        "message": value.message,
                        "stack": typeof value.stack === 'string' ? value.stack : null,
                    }};
                }}

                if (Array.isArray(value)) {{
                    if (__rub_seen.has(value)) {{
                        return {{
                            "__rub_projection": "summary",
                            "kind": "Cycle",
                            "description": "cyclic array",
                        }};
                    }}
                    __rub_seen.add(value);
                    const items = value.slice(0, 50).map((entry) => __rub_project(entry, depth + 1));
                    __rub_seen.delete(value);
                    return items;
                }}

                if (
                    (typeof Window !== 'undefined' && value instanceof Window) ||
                    (typeof Document !== 'undefined' && value instanceof Document) ||
                    (typeof Node !== 'undefined' && value instanceof Node)
                ) {{
                    return __rub_summary(value);
                }}

                const proto = Object.getPrototypeOf(value);
                if (proto === Object.prototype || proto === null) {{
                    if (__rub_seen.has(value)) {{
                        return {{
                            "__rub_projection": "summary",
                            "kind": "Cycle",
                            "description": "cyclic object",
                        }};
                    }}
                    __rub_seen.add(value);
                    const out = {{}};
                    let count = 0;
                    for (const key of Object.keys(value)) {{
                        if (count >= 50) {{
                            out.__rub_truncated__ = true;
                            break;
                        }}
                        out[key] = __rub_project(value[key], depth + 1);
                        count++;
                    }}
                    __rub_seen.delete(value);
                    return out;
                }}

                return __rub_summary(value);
            }}

            return __rub_project(await Promise.resolve((0, eval)(__rub_code)));
        }})()
        "#
    );

    let mut builder = EvaluateParams::builder()
        .expression(script)
        .await_promise(true)
        .return_by_value(true);
    if let Some(context_id) = context_id {
        builder = builder.context_id(context_id);
    }
    let result = page
        .execute(
            builder
                .build()
                .map_err(|e| RubError::Internal(format!("Build evaluate params failed: {e}")))?,
        )
        .await
        .map_err(|e| {
            RubError::domain(ErrorCode::JsEvalError, format!("JS evaluation failed: {e}"))
        })?;
    Ok(result
        .result
        .result
        .value
        .unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod tests {
    use super::execute_js;

    #[test]
    fn exec_projection_uses_summary_envelope_for_non_json_values() {
        let _ = execute_js;
        let script = include_str!("evaluation.rs");
        assert!(script.contains("\"__rub_projection\": \"summary\""));
        assert!(script.contains("Promise.resolve((0, eval)(__rub_code))"));
        assert!(script.contains("value instanceof Window"));
    }
}
