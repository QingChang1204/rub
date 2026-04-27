use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, ExceptionDetails, ExecutionContextId, RemoteObjectId,
};
use rub_core::error::{ErrorCode, RubError};
use std::sync::Arc;

pub(crate) async fn call_function(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
    await_promise: bool,
) -> Result<(), RubError> {
    let response = page
        .execute(
            CallFunctionOnParams::builder()
                .function_declaration(function_declaration)
                .object_id(object_id.clone())
                .await_promise(await_promise)
                .user_gesture(true)
                .build()
                .map_err(|e| RubError::Internal(format!("Build CallFunctionOn failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("CallFunctionOn failed: {e}")))?;
    ensure_no_runtime_exception("CallFunctionOn", response.result.exception_details)?;

    Ok(())
}

pub(crate) async fn call_function_returning_value(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
) -> Result<serde_json::Value, RubError> {
    let response = page
        .execute(
            CallFunctionOnParams::builder()
                .function_declaration(function_declaration)
                .object_id(object_id.clone())
                .await_promise(true)
                .return_by_value(true)
                .user_gesture(true)
                .build()
                .map_err(|e| RubError::Internal(format!("Build CallFunctionOn failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("CallFunctionOn failed: {e}")))?;
    ensure_no_runtime_exception("CallFunctionOn", response.result.exception_details)?;

    response
        .result
        .result
        .value
        .ok_or_else(|| RubError::Internal("CallFunctionOn returned no value".to_string()))
}

pub(crate) async fn call_function_returning_string(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
) -> Result<String, RubError> {
    let value = call_function_returning_value(page, object_id, function_declaration).await?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

pub(crate) async fn call_function_returning_object_id(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
) -> Result<RemoteObjectId, RubError> {
    let response = page
        .execute(
            CallFunctionOnParams::builder()
                .function_declaration(function_declaration)
                .object_id(object_id.clone())
                .await_promise(true)
                .user_gesture(true)
                .build()
                .map_err(|e| RubError::Internal(format!("Build CallFunctionOn failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("CallFunctionOn failed: {e}")))?;
    ensure_no_runtime_exception("CallFunctionOn", response.result.exception_details)?;

    response
        .result
        .result
        .object_id
        .ok_or_else(|| RubError::Internal("CallFunctionOn did not return an objectId".to_string()))
}

pub(crate) async fn evaluate_returning_object_id(
    page: &Arc<Page>,
    expression: &str,
) -> Result<RemoteObjectId, RubError> {
    evaluate_returning_object_id_in_context(page, None, expression).await
}

pub(crate) async fn evaluate_returning_object_id_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
    expression: &str,
) -> Result<RemoteObjectId, RubError> {
    let mut builder = EvaluateParams::builder()
        .expression(expression)
        .await_promise(true);
    if let Some(context_id) = context_id {
        builder = builder.context_id(context_id);
    }
    let response = page
        .execute(
            builder
                .build()
                .map_err(|e| RubError::Internal(format!("Build evaluate params failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("Evaluate failed: {e}")))?;
    ensure_no_runtime_exception("Evaluate", response.result.exception_details)?;

    response
        .result
        .result
        .object_id
        .ok_or_else(|| RubError::Internal("Evaluate did not return an objectId".to_string()))
}

pub(crate) async fn evaluate_returning_string(
    page: &Arc<Page>,
    expression: &str,
) -> Result<String, RubError> {
    let response = page
        .execute(
            EvaluateParams::builder()
                .expression(expression)
                .await_promise(true)
                .return_by_value(true)
                .build()
                .map_err(|e| RubError::Internal(format!("Build evaluate params failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("Evaluate failed: {e}")))?;
    ensure_no_runtime_exception("Evaluate", response.result.exception_details)?;

    match response.result.result.value {
        Some(serde_json::Value::String(value)) => Ok(value),
        Some(serde_json::Value::Null) | None => Ok(String::new()),
        Some(other) => Ok(other.to_string()),
    }
}

pub(crate) async fn evaluate_returning_string_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
    expression: &str,
) -> Result<String, RubError> {
    let mut builder = EvaluateParams::builder()
        .expression(expression)
        .await_promise(true)
        .return_by_value(true);
    if let Some(context_id) = context_id {
        builder = builder.context_id(context_id);
    }
    let response = page
        .execute(
            builder
                .build()
                .map_err(|e| RubError::Internal(format!("Build evaluate params failed: {e}")))?,
        )
        .await
        .map_err(|e| RubError::Internal(format!("Evaluate failed: {e}")))?;
    ensure_no_runtime_exception("Evaluate", response.result.exception_details)?;

    match response.result.result.value {
        Some(serde_json::Value::String(value)) => Ok(value),
        Some(serde_json::Value::Null) | None => Ok(String::new()),
        Some(other) => Ok(other.to_string()),
    }
}

fn ensure_no_runtime_exception(
    operation: &'static str,
    exception: Option<ExceptionDetails>,
) -> Result<(), RubError> {
    let Some(exception) = exception else {
        return Ok(());
    };
    Err(runtime_exception_error(operation, exception))
}

fn runtime_exception_error(operation: &'static str, exception: ExceptionDetails) -> RubError {
    let exception_description = exception
        .exception
        .as_ref()
        .and_then(|object| object.description.clone())
        .or_else(|| {
            exception
                .exception
                .as_ref()
                .and_then(|object| object.value.as_ref())
                .map(|value| value.to_string())
        });
    RubError::domain_with_context(
        ErrorCode::JsEvalError,
        format!(
            "{operation} raised a JavaScript exception: {}",
            exception_description
                .as_deref()
                .unwrap_or(exception.text.as_str())
        ),
        serde_json::json!({
            "reason": "cdp_runtime_exception",
            "operation": operation,
            "text": exception.text,
            "line_number": exception.line_number,
            "column_number": exception.column_number,
            "exception_description": exception_description,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{ensure_no_runtime_exception, runtime_exception_error};
    use chromiumoxide::cdp::js_protocol::runtime::{ExceptionDetails, RemoteObject};
    use rub_core::error::ErrorCode;

    #[test]
    fn runtime_exception_details_are_projected_as_js_eval_error() {
        let exception = ExceptionDetails {
            exception: Some(
                serde_json::from_value::<RemoteObject>(serde_json::json!({
                    "type": "object",
                    "subtype": "error",
                    "description": "Error: boom",
                }))
                .expect("remote exception object"),
            ),
            ..ExceptionDetails::new(7, "Uncaught", 3, 9)
        };

        let envelope = runtime_exception_error("Evaluate", exception).into_envelope();

        assert_eq!(envelope.code, ErrorCode::JsEvalError);
        assert!(envelope.message.contains("Error: boom"));
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("cdp_runtime_exception")
        );
    }

    #[test]
    fn runtime_exception_guard_fails_closed() {
        let error = ensure_no_runtime_exception(
            "CallFunctionOn",
            Some(ExceptionDetails::new(1, "ReferenceError", 0, 0)),
        )
        .expect_err("exceptionDetails must not be treated as a successful result");

        assert_eq!(error.into_envelope().code, ErrorCode::JsEvalError);
    }
}
