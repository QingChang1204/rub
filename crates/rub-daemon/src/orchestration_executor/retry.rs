use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::OrchestrationRuleInfo;

use super::{ORCHESTRATION_TRANSIENT_RETRY_DELAY_MS, ORCHESTRATION_TRANSIENT_RETRY_LIMIT};

#[derive(Debug, Clone, Copy)]
pub(super) struct OrchestrationRetryPolicy {
    pub(super) max_retries: u32,
    pub(super) delay: Duration,
}

#[derive(Debug)]
pub(super) struct OrchestrationRetryFailure {
    pub(super) error: ErrorEnvelope,
    pub(super) attempts: u32,
}

pub(super) fn orchestration_retry_policy(rule: &OrchestrationRuleInfo) -> OrchestrationRetryPolicy {
    OrchestrationRetryPolicy {
        max_retries: rule
            .execution_policy
            .max_retries
            .min(ORCHESTRATION_TRANSIENT_RETRY_LIMIT),
        delay: Duration::from_millis(ORCHESTRATION_TRANSIENT_RETRY_DELAY_MS),
    }
}

pub(super) async fn run_with_orchestration_retry<T, F, Fut>(
    policy: OrchestrationRetryPolicy,
    mut operation: F,
) -> Result<(T, u32), OrchestrationRetryFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorEnvelope>>,
{
    let mut attempts = 0;
    let mut last_retry_reason = None;

    loop {
        attempts += 1;
        match operation().await {
            Ok(value) => return Ok((value, attempts)),
            Err(error) => {
                if let Some(retry_reason) = classify_retryable_orchestration_error(&error)
                    && attempts <= policy.max_retries.saturating_add(1)
                {
                    if attempts <= policy.max_retries {
                        last_retry_reason = Some(retry_reason.to_string());
                        tokio::time::sleep(policy.delay).await;
                        continue;
                    }
                    return Err(OrchestrationRetryFailure {
                        error: attach_orchestration_retry_diagnostics(
                            error,
                            attempts.saturating_sub(1),
                            Some(retry_reason),
                        ),
                        attempts,
                    });
                }

                return Err(OrchestrationRetryFailure {
                    error: attach_orchestration_retry_diagnostics(
                        error,
                        attempts.saturating_sub(1),
                        last_retry_reason.as_deref(),
                    ),
                    attempts,
                });
            }
        }
    }
}

fn classify_retryable_orchestration_error(error: &ErrorEnvelope) -> Option<&'static str> {
    let reason = error
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str());
    match (error.code, reason) {
        (ErrorCode::DaemonNotRunning, Some("orchestration_target_session_unreachable"))
        | (ErrorCode::DaemonNotRunning, Some("orchestration_source_session_unreachable")) => {
            Some("session_unreachable")
        }
        (ErrorCode::IpcProtocolError, Some("orchestration_target_dispatch_transport_failed"))
        | (
            ErrorCode::IpcProtocolError,
            Some("orchestration_source_var_dispatch_transport_failed"),
        ) => Some("dispatch_transport_transient"),
        (ErrorCode::BrowserCrashed, Some("orchestration_target_not_active")) => {
            Some("target_activation_transient")
        }
        (ErrorCode::BrowserCrashed, Some("continuity_frame_unavailable")) => {
            Some("target_continuity_transient")
        }
        (ErrorCode::BrowserCrashed, Some("continuity_readiness_degraded")) => {
            Some("target_readiness_transient")
        }
        (ErrorCode::BrowserCrashed, Some("continuity_runtime_degraded")) => {
            Some("target_runtime_transient")
        }
        _ => None,
    }
}

fn attach_orchestration_retry_diagnostics(
    mut error: ErrorEnvelope,
    retry_count: u32,
    retry_reason: Option<&str>,
) -> ErrorEnvelope {
    if retry_count == 0 && retry_reason.is_none() {
        return error;
    }

    let mut context = error
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert("retry_count".to_string(), serde_json::json!(retry_count));
    if let Some(reason) = retry_reason {
        context.insert("retry_reason".to_string(), serde_json::json!(reason));
    }
    error.context = Some(serde_json::Value::Object(context));
    error
}

#[cfg(test)]
mod source_retry_tests {
    use super::{
        OrchestrationRetryPolicy, classify_retryable_orchestration_error,
        run_with_orchestration_retry,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[test]
    fn source_session_failures_are_classified_as_retryable() {
        let envelope = ErrorEnvelope::new(ErrorCode::DaemonNotRunning, "source session down")
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_unreachable",
            }));
        assert_eq!(
            classify_retryable_orchestration_error(&envelope),
            Some("session_unreachable")
        );
    }

    #[tokio::test]
    async fn retry_loop_recovers_after_transient_source_session_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let retry_attempts = attempts.clone();

        let (value, attempts_used) = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 1,
                delay: Duration::from_millis(0),
            },
            move || {
                let attempts = retry_attempts.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return Err(ErrorEnvelope::new(
                            ErrorCode::DaemonNotRunning,
                            "source session down",
                        )
                        .with_context(serde_json::json!({
                            "reason": "orchestration_source_session_unreachable",
                        })));
                    }
                    Ok("resolved")
                }
            },
        )
        .await
        .expect("transient source-session failure should retry");

        assert_eq!(value, "resolved");
        assert_eq!(attempts_used, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use rub_core::error::{ErrorCode, ErrorEnvelope};

    use super::{
        OrchestrationRetryPolicy, classify_retryable_orchestration_error,
        run_with_orchestration_retry,
    };

    #[tokio::test]
    async fn orchestration_retry_retries_transient_dispatch_failures_then_succeeds() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let result = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 2,
                delay: Duration::from_millis(0),
            },
            move || {
                let counter = counter.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<&'static str, ErrorEnvelope>(
                            ErrorEnvelope::new(ErrorCode::IpcProtocolError, "dispatch failed")
                                .with_context(serde_json::json!({
                                    "reason": "orchestration_target_dispatch_transport_failed",
                                })),
                        )
                    } else {
                        Ok::<&'static str, ErrorEnvelope>("ok")
                    }
                }
            },
        )
        .await
        .expect("transient dispatch failure should retry and succeed");
        assert_eq!(result.0, "ok");
        assert_eq!(result.1, 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn orchestration_retry_retries_pre_dispatch_readiness_failures_then_succeeds() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let result = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 2,
                delay: Duration::from_millis(0),
            },
            move || {
                let counter = counter.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<&'static str, ErrorEnvelope>(
                            ErrorEnvelope::new(
                                ErrorCode::BrowserCrashed,
                                "target readiness degraded",
                            )
                            .with_context(serde_json::json!({
                                "reason": "continuity_readiness_degraded",
                            })),
                        )
                    } else {
                        Ok::<&'static str, ErrorEnvelope>("ok")
                    }
                }
            },
        )
        .await
        .expect("pre-dispatch readiness failure should retry and succeed");
        assert_eq!(result.0, "ok");
        assert_eq!(result.1, 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn orchestration_retry_exhaustion_attaches_retry_diagnostics() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let failure = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 2,
                delay: Duration::from_millis(0),
            },
            move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<(), ErrorEnvelope>(
                        ErrorEnvelope::new(ErrorCode::IpcProtocolError, "dispatch failed")
                            .with_context(serde_json::json!({
                                "reason": "orchestration_target_dispatch_transport_failed",
                            })),
                    )
                }
            },
        )
        .await
        .expect_err("transient failures should exhaust retry budget");
        assert_eq!(failure.attempts, 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let context = failure
            .error
            .context
            .as_ref()
            .and_then(|value| value.as_object())
            .expect("retry diagnostics should attach context");
        assert_eq!(context.get("retry_count"), Some(&serde_json::json!(2)));
        assert_eq!(
            context.get("retry_reason"),
            Some(&serde_json::json!("dispatch_transport_transient")),
        );
    }

    #[test]
    fn orchestration_retry_does_not_retry_protocol_dispatch_failures() {
        let envelope = ErrorEnvelope::new(ErrorCode::IpcProtocolError, "dispatch failed")
            .with_context(serde_json::json!({
                "reason": "orchestration_target_dispatch_protocol_failed",
            }));
        assert_eq!(classify_retryable_orchestration_error(&envelope), None);
    }

    #[tokio::test]
    async fn orchestration_retry_does_not_retry_non_transient_errors() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let failure = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 3,
                delay: Duration::from_millis(0),
            },
            move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<(), ErrorEnvelope>(ErrorEnvelope::new(
                        ErrorCode::ElementNotFound,
                        "missing element",
                    ))
                }
            },
        )
        .await
        .expect_err("non-transient errors should not retry");
        assert_eq!(failure.attempts, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn orchestration_retry_does_not_retry_target_automation_pause() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let failure = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 3,
                delay: Duration::from_millis(0),
            },
            move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<(), ErrorEnvelope>(
                        ErrorEnvelope::new(ErrorCode::AutomationPaused, "target automation paused")
                            .with_context(serde_json::json!({
                                "reason": "orchestration_target_automation_paused",
                            })),
                    )
                }
            },
        )
        .await
        .expect_err("automation paused must remain blocked and non-retryable");
        assert_eq!(failure.attempts, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
