use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, ExecutionContextId, RemoteObjectId,
};
use rub_core::error::RubError;
use std::sync::Arc;

pub(crate) async fn call_function(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
    await_promise: bool,
) -> Result<(), RubError> {
    page.execute(
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

    match response.result.result.value {
        Some(serde_json::Value::String(value)) => Ok(value),
        Some(serde_json::Value::Null) | None => Ok(String::new()),
        Some(other) => Ok(other.to_string()),
    }
}
