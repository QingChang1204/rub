use std::time::Duration;

use crate::router::TransactionDeadline;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::OrchestrationRuleInfo;

use super::{
    ORCHESTRATION_TRANSIENT_RETRY_DELAY_MS, ORCHESTRATION_TRANSIENT_RETRY_LIMIT,
    run_orchestration_future_with_outer_deadline,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct OrchestrationRetryPolicy {
    pub(super) max_retries: u32,
    pub(super) delay: Duration,
}

impl OrchestrationRetryPolicy {
    pub(super) fn remaining_after_attempts(self, attempts: u32) -> Self {
        let spent_retries = attempts.saturating_sub(1);
        Self {
            max_retries: self.max_retries.saturating_sub(spent_retries),
            delay: self.delay,
        }
    }
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
    outer_deadline: Option<TransactionDeadline>,
    operation: F,
) -> Result<(T, u32), OrchestrationRetryFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorEnvelope>>,
{
    run_with_orchestration_retry_with_timeout_error(
        policy,
        outer_deadline,
        orchestration_retry_timeout_budget_exhausted_error,
        operation,
    )
    .await
}

pub(super) async fn run_with_orchestration_retry_with_timeout_error<T, F, Fut, G>(
    policy: OrchestrationRetryPolicy,
    outer_deadline: Option<TransactionDeadline>,
    timeout_error: G,
    mut operation: F,
) -> Result<(T, u32), OrchestrationRetryFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorEnvelope>>,
    G: Fn() -> ErrorEnvelope,
{
    let mut attempts = 0;
    let mut last_retry_reason = None;

    loop {
        attempts += 1;
        let result = run_orchestration_future_with_outer_deadline(
            outer_deadline,
            &timeout_error,
            operation(),
        )
        .await;
        match result {
            Ok(value) => return Ok((value, attempts)),
            Err(error) => {
                if let Some(retry_reason) = classify_retryable_orchestration_error(&error)
                    && attempts <= policy.max_retries.saturating_add(1)
                {
                    if attempts <= policy.max_retries {
                        last_retry_reason = Some(retry_reason.to_string());
                        let delay = outer_deadline
                            .and_then(|deadline| deadline.remaining_duration())
                            .map(|remaining| remaining.min(policy.delay))
                            .unwrap_or(policy.delay);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
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

fn orchestration_retry_timeout_budget_exhausted_error() -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        "Orchestration retry loop exhausted the caller-owned timeout budget before a retryable phase completed",
    )
    .with_context(serde_json::json!({
        "reason": "orchestration_retry_timeout_budget_exhausted",
    }))
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
            None,
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

    #[test]
    fn retry_policy_remaining_after_attempts_consumes_shared_retry_budget() {
        let policy = OrchestrationRetryPolicy {
            max_retries: 2,
            delay: Duration::from_millis(0),
        };

        assert_eq!(policy.remaining_after_attempts(1).max_retries, 2);
        assert_eq!(policy.remaining_after_attempts(2).max_retries, 1);
        assert_eq!(policy.remaining_after_attempts(3).max_retries, 0);
        assert_eq!(policy.remaining_after_attempts(4).max_retries, 0);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use crate::router::TransactionDeadline;
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
            None,
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
    async fn orchestration_retry_does_not_retry_pre_dispatch_readiness_failures() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let failure = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 2,
                delay: Duration::from_millis(0),
            },
            None,
            move || {
                let counter = counter.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err::<&'static str, ErrorEnvelope>(
                            ErrorEnvelope::new(ErrorCode::SessionBusy, "target readiness degraded")
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
        .expect_err("pre-dispatch readiness degradation must remain non-retryable");
        assert_eq!(failure.attempts, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            failure
                .error
                .context
                .as_ref()
                .and_then(|value| value.get("retry_reason"))
                .and_then(|value| value.as_str()),
            None
        );
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
            None,
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

    #[test]
    fn orchestration_retry_ignores_remote_reason_collisions() {
        let envelope = ErrorEnvelope::new(ErrorCode::IpcProtocolError, "remote command failed")
            .with_context(serde_json::json!({
                "reason": "orchestration_remote_error_response",
                "remote_reason": "orchestration_target_dispatch_transport_failed",
                "local_dispatch_reason": "orchestration_target_dispatch_protocol_failed",
            }));
        assert_eq!(classify_retryable_orchestration_error(&envelope), None);
    }

    #[test]
    fn orchestration_retry_does_not_retry_invalid_target_frame_loss() {
        let envelope = ErrorEnvelope::new(ErrorCode::InvalidInput, "frame unavailable")
            .with_context(serde_json::json!({
                "reason": "continuity_frame_unavailable",
            }));
        assert_eq!(classify_retryable_orchestration_error(&envelope), None);
    }

    #[test]
    fn orchestration_retry_does_not_retry_degraded_target_activation_loss() {
        let envelope = ErrorEnvelope::new(ErrorCode::SessionBusy, "target tab inactive")
            .with_context(serde_json::json!({
                "reason": "orchestration_target_not_active",
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
            None,
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
            None,
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

    #[tokio::test]
    async fn orchestration_retry_fails_closed_when_outer_deadline_is_exhausted() {
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();
        let failure = run_with_orchestration_retry(
            OrchestrationRetryPolicy {
                max_retries: 2,
                delay: Duration::from_millis(10),
            },
            Some(TransactionDeadline::new(1)),
            move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
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
        .expect_err("expired outer deadline should fail closed during retry");

        assert_eq!(failure.error.code, ErrorCode::IpcTimeout);
        assert_eq!(
            failure
                .error
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_retry_timeout_budget_exhausted")
        );
    }
}
