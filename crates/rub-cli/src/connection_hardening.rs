use std::future::Future;
use std::time::Duration;

use rub_core::error::{ErrorCode, RubError};

const TRANSIENT_RETRY_LIMIT: u32 = 3;
const TRANSIENT_RETRY_DELAY_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionFailureClass {
    TransportTransient,
    BrowserUnavailable,
    ProtocolMismatch,
    PolicyRejection,
    UserInput,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Default)]
pub(crate) struct RetryAttribution {
    pub retry_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_reason: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub max_retries: u32,
    pub delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: TRANSIENT_RETRY_LIMIT,
            delay: Duration::from_millis(TRANSIENT_RETRY_DELAY_MS),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RetryFailure {
    pub error: RubError,
    pub attribution: RetryAttribution,
    pub final_failure_class: ConnectionFailureClass,
}

impl RetryFailure {
    pub(crate) fn into_error(self) -> RubError {
        attach_connection_diagnostics(self.error, &self.attribution, self.final_failure_class)
    }
}

#[derive(Debug)]
pub(crate) struct AttemptError {
    pub error: RubError,
    pub transient_reason: Option<String>,
    pub final_failure_class: ConnectionFailureClass,
}

impl AttemptError {
    pub(crate) fn retryable(error: RubError, reason: impl Into<String>) -> Self {
        Self {
            error,
            transient_reason: Some(reason.into()),
            final_failure_class: ConnectionFailureClass::TransportTransient,
        }
    }

    pub(crate) fn terminal(error: RubError, final_failure_class: ConnectionFailureClass) -> Self {
        Self {
            error,
            transient_reason: None,
            final_failure_class,
        }
    }
}

pub(crate) async fn run_with_bounded_retry<T, F, Fut>(
    policy: RetryPolicy,
    mut operation: F,
) -> Result<(T, RetryAttribution), RetryFailure>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AttemptError>>,
{
    let mut attribution = RetryAttribution::default();

    loop {
        match operation().await {
            Ok(value) => return Ok((value, attribution)),
            Err(attempt) => {
                if let Some(reason) = attempt.transient_reason.clone()
                    && attribution.retry_count < policy.max_retries
                {
                    attribution.retry_count += 1;
                    attribution.retry_reason = Some(reason);
                    tokio::time::sleep(policy.delay).await;
                    continue;
                }

                return Err(RetryFailure {
                    error: attempt.error,
                    attribution,
                    final_failure_class: if attempt.transient_reason.is_some() {
                        ConnectionFailureClass::TransportTransient
                    } else {
                        attempt.final_failure_class
                    },
                });
            }
        }
    }
}

pub(crate) fn classify_error_code(code: ErrorCode) -> ConnectionFailureClass {
    match code {
        ErrorCode::BrowserNotFound
        | ErrorCode::BrowserLaunchFailed
        | ErrorCode::BrowserCrashed
        | ErrorCode::CdpConnectionFailed
        | ErrorCode::CdpConnectionLost => ConnectionFailureClass::BrowserUnavailable,
        ErrorCode::IpcProtocolError | ErrorCode::IpcVersionMismatch => {
            ConnectionFailureClass::ProtocolMismatch
        }
        ErrorCode::ProfileInUse | ErrorCode::SessionBusy | ErrorCode::AutomationPaused => {
            ConnectionFailureClass::PolicyRejection
        }
        ErrorCode::InvalidInput
        | ErrorCode::ConflictingConnectOptions
        | ErrorCode::ProfileNotFound
        | ErrorCode::CdpConnectionAmbiguous
        | ErrorCode::FileNotFound => ConnectionFailureClass::UserInput,
        _ => ConnectionFailureClass::Unknown,
    }
}

pub(crate) fn classify_io_transient(error: &std::io::Error) -> Option<&'static str> {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => Some("connection_refused"),
        std::io::ErrorKind::ConnectionReset => Some("connection_reset"),
        std::io::ErrorKind::ConnectionAborted => Some("connection_aborted"),
        std::io::ErrorKind::TimedOut => Some("timed_out"),
        std::io::ErrorKind::Interrupted => Some("interrupted"),
        std::io::ErrorKind::WouldBlock => Some("would_block"),
        std::io::ErrorKind::BrokenPipe => Some("broken_pipe"),
        std::io::ErrorKind::UnexpectedEof => Some("unexpected_eof"),
        std::io::ErrorKind::NotFound => Some("socket_not_found"),
        _ => None,
    }
}

pub(crate) fn classify_transport_message(message: &str) -> Option<&'static str> {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("daemon closed connection") || normalized.contains("unexpected eof") {
        return Some("unexpected_eof");
    }
    if normalized.contains("broken pipe") {
        return Some("broken_pipe");
    }
    if normalized.contains("connection reset") {
        return Some("connection_reset");
    }
    if normalized.contains("connection refused") {
        return Some("connection_refused");
    }
    if normalized.contains("connection aborted") {
        return Some("connection_aborted");
    }
    if normalized.contains("timed out") {
        return Some("timed_out");
    }
    if normalized.contains("no such file or directory") {
        return Some("socket_not_found");
    }
    None
}

pub(crate) fn attach_connection_diagnostics(
    error: RubError,
    attribution: &RetryAttribution,
    final_failure_class: ConnectionFailureClass,
) -> RubError {
    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert(
        "retry_count".to_string(),
        serde_json::json!(attribution.retry_count),
    );
    if let Some(reason) = attribution.retry_reason.as_ref() {
        context.insert("retry_reason".to_string(), serde_json::json!(reason));
    }
    context.insert(
        "final_failure_class".to_string(),
        serde_json::to_value(final_failure_class).unwrap_or_else(|_| serde_json::json!("unknown")),
    );
    envelope.context = Some(serde_json::Value::Object(context));
    RubError::Domain(envelope)
}

#[cfg(test)]
mod tests {
    use super::{
        AttemptError, ConnectionFailureClass, RetryPolicy, attach_connection_diagnostics,
        classify_error_code, classify_io_transient, classify_transport_message,
        run_with_bounded_retry,
    };
    use rub_core::error::{ErrorCode, RubError};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn classify_error_code_maps_expected_failure_classes() {
        assert_eq!(
            classify_error_code(ErrorCode::CdpConnectionFailed),
            ConnectionFailureClass::BrowserUnavailable
        );
        assert_eq!(
            classify_error_code(ErrorCode::IpcVersionMismatch),
            ConnectionFailureClass::ProtocolMismatch
        );
        assert_eq!(
            classify_error_code(ErrorCode::ProfileInUse),
            ConnectionFailureClass::PolicyRejection
        );
        assert_eq!(
            classify_error_code(ErrorCode::InvalidInput),
            ConnectionFailureClass::UserInput
        );
    }

    #[test]
    fn classify_transport_helpers_detect_retryable_failures() {
        let err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        assert_eq!(classify_io_transient(&err), Some("connection_reset"));
        assert_eq!(
            classify_transport_message("Daemon closed connection"),
            Some("unexpected_eof")
        );
    }

    #[tokio::test]
    async fn startup_delay_fixture_retries_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_op = attempts.clone();
        let policy = RetryPolicy {
            max_retries: 3,
            delay: Duration::from_millis(0),
        };

        let (value, attribution) = run_with_bounded_retry(policy, move || {
            let attempts = attempts_for_op.clone();
            async move {
                let current = attempts.fetch_add(1, Ordering::Relaxed);
                if current < 2 {
                    Err(AttemptError::retryable(
                        RubError::domain(
                            ErrorCode::DaemonStartFailed,
                            "Injected startup delay before socket became reachable",
                        ),
                        "socket_not_found",
                    ))
                } else {
                    Ok("connected")
                }
            }
        })
        .await
        .expect("startup delay fixture should recover");

        assert_eq!(value, "connected");
        assert_eq!(attribution.retry_count, 2);
        assert_eq!(
            attribution.retry_reason.as_deref(),
            Some("socket_not_found")
        );
    }

    #[tokio::test]
    async fn connection_reset_fixture_retries_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_op = attempts.clone();
        let policy = RetryPolicy {
            max_retries: 2,
            delay: Duration::from_millis(0),
        };

        let (value, attribution) = run_with_bounded_retry(policy, move || {
            let attempts = attempts_for_op.clone();
            async move {
                let current = attempts.fetch_add(1, Ordering::Relaxed);
                if current == 0 {
                    Err(AttemptError::retryable(
                        RubError::domain(
                            ErrorCode::IpcProtocolError,
                            "Injected connection reset during daemon attach",
                        ),
                        "connection_reset",
                    ))
                } else {
                    Ok("attached")
                }
            }
        })
        .await
        .expect("connection reset fixture should recover");

        assert_eq!(value, "attached");
        assert_eq!(attribution.retry_count, 1);
        assert_eq!(
            attribution.retry_reason.as_deref(),
            Some("connection_reset")
        );
    }

    #[tokio::test]
    async fn handshake_flap_fixture_reports_transport_transient_diagnostics() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_op = attempts.clone();
        let policy = RetryPolicy {
            max_retries: 2,
            delay: Duration::from_millis(0),
        };

        let failure = run_with_bounded_retry(policy, move || {
            let attempts = attempts_for_op.clone();
            async move {
                let _current = attempts.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>(AttemptError::retryable(
                    RubError::domain(
                        ErrorCode::IpcProtocolError,
                        "Injected handshake flap closed the daemon connection",
                    ),
                    "unexpected_eof",
                ))
            }
        })
        .await
        .expect_err("handshake flap fixture should exhaust retries");

        let envelope = failure.into_error().into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(envelope.context.as_ref().unwrap()["retry_count"], 2);
        assert_eq!(
            envelope.context.as_ref().unwrap()["retry_reason"],
            "unexpected_eof"
        );
        assert_eq!(
            envelope.context.as_ref().unwrap()["final_failure_class"],
            "transport_transient"
        );
    }

    #[test]
    fn attach_connection_diagnostics_merges_existing_context() {
        let error = RubError::domain_with_context(
            ErrorCode::DaemonStartFailed,
            "bootstrap failed",
            serde_json::json!({ "phase": "startup" }),
        );
        let envelope = attach_connection_diagnostics(
            error,
            &super::RetryAttribution {
                retry_count: 1,
                retry_reason: Some("connection_refused".to_string()),
            },
            ConnectionFailureClass::TransportTransient,
        )
        .into_envelope();
        assert_eq!(envelope.context.as_ref().unwrap()["phase"], "startup");
        assert_eq!(envelope.context.as_ref().unwrap()["retry_count"], 1);
        assert_eq!(
            envelope.context.as_ref().unwrap()["final_failure_class"],
            "transport_transient"
        );
    }

    #[tokio::test]
    async fn non_retryable_failure_exits_without_retry() {
        let policy = RetryPolicy {
            max_retries: 3,
            delay: Duration::from_millis(0),
        };
        let err = run_with_bounded_retry::<(), _, _>(policy, || async {
            Err(AttemptError::terminal(
                RubError::domain(ErrorCode::ProfileInUse, "profile already in use"),
                ConnectionFailureClass::PolicyRejection,
            ))
        })
        .await
        .expect_err("non-retryable policy error should fail immediately")
        .into_error()
        .into_envelope();
        assert_eq!(err.context.as_ref().unwrap()["retry_count"], 0);
        assert_eq!(
            err.context.as_ref().unwrap()["final_failure_class"],
            "policy_rejection"
        );
    }
}
