use rub_core::error::RubError;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::IpcRequest;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;

use super::{
    BootstrapClient, DaemonConnection, TransientSocketPolicy, bootstrap_client,
    detect_or_connect_hardened_until, ipc_budget_exhausted_error, ipc_timeout_error,
    ipc_transport_error, project_request_onto_deadline, remaining_budget_ms,
    replay_recoverable_transport_reason,
};
use crate::session_policy::ConnectionRequest;

pub(crate) async fn send_existing_request_with_replay_recovery(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    rub_home: &Path,
    session: &str,
    original_daemon_session_id: Option<&str>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    send_request_with_replay_strategy(
        client,
        request,
        deadline,
        original_daemon_session_id,
        ReplayReconnectStrategy::Existing { rub_home, session },
    )
    .await
}

pub(crate) async fn send_request_with_replay_recovery(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    recovery: ReplayRecoveryContext<'_>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    send_request_with_replay_strategy(
        client,
        request,
        deadline,
        recovery.original_daemon_session_id,
        ReplayReconnectStrategy::Bootstrap(recovery),
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) struct ReplayRecoveryContext<'a> {
    pub rub_home: &'a Path,
    pub session: &'a str,
    pub daemon_args: &'a [String],
    pub connection_request: &'a ConnectionRequest,
    pub attachment_identity: Option<&'a str>,
    pub original_daemon_session_id: Option<&'a str>,
}

struct ReplayAttempt<'a> {
    started: std::time::Instant,
    command_id: &'a str,
    retry_reason: &'static str,
    original_timeout_ms: u64,
    original_daemon_session_id: Option<&'a str>,
}

struct ReplayReconnectResult {
    client: IpcClient,
    daemon_session_id: Option<String>,
}

#[derive(Clone, Copy)]
enum ReplayReconnectStrategy<'a> {
    Existing {
        rub_home: &'a Path,
        session: &'a str,
    },
    Bootstrap(ReplayRecoveryContext<'a>),
}

#[derive(Clone, Copy)]
struct ReplaySendLifecycle<'a> {
    deadline: Instant,
    original_daemon_session_id: Option<&'a str>,
    strategy: ReplayReconnectStrategy<'a>,
}

struct ReplayRecoverySurface<'a> {
    rub_home: &'a Path,
    session: &'a str,
}

impl ReplayAttempt<'_> {
    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

fn bind_request_to_daemon_authority(
    request: &IpcRequest,
    daemon_session_id: Option<&str>,
) -> IpcRequest {
    if request.daemon_session_id.is_none()
        && let Some(daemon_session_id) = daemon_session_id
    {
        return request
            .clone()
            .with_daemon_session_id(daemon_session_id.to_string())
            .expect("validated daemon session id must remain protocol-valid");
    }
    request.clone()
}

impl<'a> ReplaySendLifecycle<'a> {
    fn recovery_surface(self) -> ReplayRecoverySurface<'a> {
        match self.strategy {
            ReplayReconnectStrategy::Existing { rub_home, session } => {
                ReplayRecoverySurface { rub_home, session }
            }
            ReplayReconnectStrategy::Bootstrap(recovery) => ReplayRecoverySurface {
                rub_home: recovery.rub_home,
                session: recovery.session,
            },
        }
    }

    fn replay_recovery_context(self, original_daemon_session_id: Option<&str>) -> Option<Value> {
        let original_daemon_session_id = original_daemon_session_id?;
        let surface = self.recovery_surface();
        Some(json!({
            "kind": "session_post_commit_journal",
            "scope": "daemon_rollover_recovery",
            "session_name": surface.session,
            "daemon_session_id": original_daemon_session_id,
            "journal_path": RubPaths::new(surface.rub_home)
                .session_runtime(surface.session, original_daemon_session_id)
                .post_commit_journal_path()
                .display()
                .to_string(),
            "reader_contract": "ndjson_post_commit_journal",
            "committed_truth_may_exist": true,
            "safe_to_rerun_with_new_command_id": false,
        }))
    }

    async fn send(
        self,
        client: &mut IpcClient,
        request: &IpcRequest,
    ) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
        let started = std::time::Instant::now();
        let request = bind_request_to_daemon_authority(request, self.original_daemon_session_id);
        let original_timeout_ms = request.timeout_ms;
        let request = self.project_initial_request(&request)?;
        match client.send(&request).await {
            Ok(response) => Ok(response),
            Err(error) => {
                self.retry_after_transport(&error, &request, started, original_timeout_ms)
                    .await
            }
        }
    }

    fn project_initial_request(&self, request: &IpcRequest) -> Result<IpcRequest, RubError> {
        project_request_onto_deadline(request, self.deadline).ok_or_else(|| {
            ipc_budget_exhausted_error(
                request.command_id.as_deref(),
                request.timeout_ms,
                "initial_send",
            )
        })
    }

    async fn retry_after_transport(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        started: Instant,
        original_timeout_ms: u64,
    ) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
        let Some(command_id) = request.command_id.as_deref() else {
            return Err(ipc_transport_error(transport_error, None, None));
        };
        let Some(retry_reason) = replay_recoverable_transport_reason(transport_error) else {
            return Err(ipc_transport_error(transport_error, Some(command_id), None));
        };
        let attempt = ReplayAttempt {
            started,
            command_id,
            retry_reason,
            original_timeout_ms,
            original_daemon_session_id: self.original_daemon_session_id,
        };
        let (mut replay_client, replay_request) = self
            .reconnect_for_replay(transport_error, request, &attempt)
            .await?;
        let replay_timeout_ms = replay_request.timeout_ms;

        replay_client
            .send(&replay_request)
            .await
            .map_err(|replay_error| {
                let recovery_contract =
                    self.replay_recovery_context(self.original_daemon_session_id);
                ipc_transport_error(
                    &replay_error,
                    Some(command_id),
                    Some(serde_json::json!({
                        "reason": "ipc_replay_retry_failed",
                        "retry_reason": retry_reason,
                        "daemon_session_id": self.original_daemon_session_id,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "remaining_timeout_ms": replay_timeout_ms,
                        "recovery_contract": recovery_contract,
                    })),
                )
            })
    }

    fn budget_exhausted_after_transport(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
        phase: Option<&str>,
    ) -> RubError {
        let recovery_contract = self.replay_recovery_context(attempt.original_daemon_session_id);
        ipc_timeout_error(
            transport_error,
            Some(attempt.command_id),
            Some(serde_json::json!({
                "reason": "ipc_replay_budget_exhausted",
                "retry_reason": attempt.retry_reason,
                "elapsed_ms": attempt.elapsed_ms(),
                "original_timeout_ms": attempt.original_timeout_ms,
                "phase": phase,
                "recovery_contract": recovery_contract,
            })),
        )
    }

    fn identity_changed_error(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
        reconnected_daemon_session_id: Option<&str>,
    ) -> RubError {
        let recovery_contract = self.replay_recovery_context(attempt.original_daemon_session_id);
        ipc_transport_error(
            transport_error,
            Some(attempt.command_id),
            Some(serde_json::json!({
                "reason": "ipc_replay_daemon_identity_changed",
                "retry_reason": attempt.retry_reason,
                "original_daemon_session_id": attempt.original_daemon_session_id,
                "reconnected_daemon_session_id": reconnected_daemon_session_id,
                "recovery_contract": recovery_contract,
            })),
        )
    }

    async fn reconnect_client(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
    ) -> Result<ReplayReconnectResult, RubError> {
        if remaining_budget_ms(self.deadline) == 0 {
            return Err(self.budget_exhausted_after_transport(transport_error, attempt, None));
        }

        match self.strategy {
            ReplayReconnectStrategy::Existing { rub_home, session } => {
                match detect_or_connect_hardened_until(
                    rub_home,
                    session,
                    TransientSocketPolicy::FailAfterLock,
                    self.deadline,
                    attempt.original_timeout_ms,
                )
                .await
                {
                    Ok(DaemonConnection::Connected {
                        client,
                        daemon_session_id,
                        ..
                    }) => Ok(ReplayReconnectResult {
                        client,
                        daemon_session_id,
                    }),
                    Ok(DaemonConnection::NeedStart) => Err(ipc_transport_error(
                        transport_error,
                        Some(attempt.command_id),
                        Some(serde_json::json!({
                            "reason": "ipc_replay_existing_daemon_unavailable",
                            "retry_reason": attempt.retry_reason,
                            "original_daemon_session_id": attempt.original_daemon_session_id,
                            "elapsed_ms": attempt.elapsed_ms(),
                            "recovery_contract": self
                                .replay_recovery_context(attempt.original_daemon_session_id),
                        })),
                    )),
                    Err(reconnect_error) => Err(ipc_transport_error(
                        transport_error,
                        Some(attempt.command_id),
                        Some(serde_json::json!({
                            "reason": "ipc_replay_reconnect_failed",
                            "retry_reason": attempt.retry_reason,
                            "original_daemon_session_id": attempt.original_daemon_session_id,
                            "elapsed_ms": attempt.elapsed_ms(),
                            "reconnect_error": reconnect_error.into_envelope(),
                            "recovery_contract": self
                                .replay_recovery_context(attempt.original_daemon_session_id),
                        })),
                    )),
                }
            }
            ReplayReconnectStrategy::Bootstrap(recovery) => self
                .reconnect_bootstrap_client(recovery, attempt)
                .await
                .map_err(|reconnect_error| {
                    ipc_transport_error(
                        transport_error,
                        Some(attempt.command_id),
                        Some(serde_json::json!({
                            "reason": "ipc_replay_reconnect_failed",
                            "retry_reason": attempt.retry_reason,
                            "original_daemon_session_id": attempt.original_daemon_session_id,
                            "elapsed_ms": attempt.elapsed_ms(),
                            "reconnect_error": reconnect_error.into_envelope(),
                            "recovery_contract": self
                                .replay_recovery_context(attempt.original_daemon_session_id),
                        })),
                    )
                }),
        }
    }

    async fn reconnect_bootstrap_client(
        self,
        recovery: ReplayRecoveryContext<'_>,
        attempt: &ReplayAttempt<'_>,
    ) -> Result<ReplayReconnectResult, RubError> {
        bootstrap_client(
            recovery.rub_home,
            recovery.session,
            attempt.original_daemon_session_id,
            self.deadline,
            attempt.original_timeout_ms,
            recovery.daemon_args,
            super::StartupAuthorityRequest {
                connection_request: recovery.connection_request,
                attachment_identity: recovery.attachment_identity,
            },
        )
        .await
        .map(|bootstrap: BootstrapClient| ReplayReconnectResult {
            client: bootstrap.client,
            daemon_session_id: bootstrap.daemon_session_id,
        })
    }

    fn project_retry_request(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        attempt: &ReplayAttempt<'_>,
    ) -> Result<IpcRequest, RubError> {
        let replay_request =
            project_request_onto_deadline(request, self.deadline).ok_or_else(|| {
                self.budget_exhausted_after_transport(transport_error, attempt, Some("replay_send"))
            })?;
        if replay_request.timeout_ms == 0 {
            return Err(self.budget_exhausted_after_transport(
                transport_error,
                attempt,
                Some("replay_send"),
            ));
        }
        Ok(replay_request)
    }

    async fn reconnect_for_replay(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        attempt: &ReplayAttempt<'_>,
    ) -> Result<(IpcClient, IpcRequest), RubError> {
        let reconnect = self.reconnect_client(transport_error, attempt).await?;
        if !replay_retry_matches_daemon_authority(
            attempt.original_daemon_session_id,
            reconnect.daemon_session_id.as_deref(),
        ) {
            return Err(self.identity_changed_error(
                transport_error,
                attempt,
                reconnect.daemon_session_id.as_deref(),
            ));
        }

        let replay_request = self.project_retry_request(transport_error, request, attempt)?;
        Ok((reconnect.client, replay_request))
    }
}

async fn send_request_with_replay_strategy(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    original_daemon_session_id: Option<&str>,
    strategy: ReplayReconnectStrategy<'_>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    ReplaySendLifecycle {
        deadline,
        original_daemon_session_id,
        strategy,
    }
    .send(client, request)
    .await
}

pub(crate) fn replay_retry_matches_daemon_authority(
    original_daemon_session_id: Option<&str>,
    reconnected_daemon_session_id: Option<&str>,
) -> bool {
    match (original_daemon_session_id, reconnected_daemon_session_id) {
        (Some(original), Some(reconnected)) => original == reconnected,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ReplayAttempt, ReplayReconnectStrategy, ReplayRecoveryContext, ReplaySendLifecycle,
    };
    use crate::session_policy::ConnectionRequest;
    use rub_core::error::ErrorCode;
    use std::io;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn temp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rub-replay-test-{}", Uuid::now_v7()))
    }

    #[tokio::test]
    async fn replay_bootstrap_reconnect_refuses_to_start_when_original_authority_is_missing() {
        let home = temp_home();
        std::fs::create_dir_all(&home).expect("create replay test home");
        let startup_request = ConnectionRequest::None;
        let lifecycle = ReplaySendLifecycle {
            deadline: Instant::now() + Duration::from_secs(1),
            original_daemon_session_id: Some("sess-remembered"),
            strategy: ReplayReconnectStrategy::Bootstrap(ReplayRecoveryContext {
                rub_home: &home,
                session: "default",
                daemon_args: &[],
                connection_request: &startup_request,
                attachment_identity: None,
                original_daemon_session_id: Some("sess-remembered"),
            }),
        };
        let attempt = ReplayAttempt {
            started: Instant::now(),
            command_id: "cmd-1",
            retry_reason: "ipc_connection_broken_pipe",
            original_timeout_ms: 1_000,
            original_daemon_session_id: Some("sess-remembered"),
        };

        let error = match lifecycle
            .reconnect_bootstrap_client(
                ReplayRecoveryContext {
                    rub_home: &home,
                    session: "default",
                    daemon_args: &[],
                    connection_request: &startup_request,
                    attachment_identity: None,
                    original_daemon_session_id: Some("sess-remembered"),
                },
                &attempt,
            )
            .await
        {
            Ok(_) => panic!("replay reconnect must not start a replacement daemon"),
            Err(error) => error,
        };
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::DaemonNotRunning);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("existing_session_bootstrap_authority_unavailable")
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn replay_identity_changed_error_projects_public_recovery_contract() {
        let home = temp_home();
        let lifecycle = ReplaySendLifecycle {
            deadline: Instant::now() + Duration::from_secs(1),
            original_daemon_session_id: Some("sess-original"),
            strategy: ReplayReconnectStrategy::Existing {
                rub_home: &home,
                session: "work",
            },
        };
        let attempt = ReplayAttempt {
            started: Instant::now(),
            command_id: "cmd-1",
            retry_reason: "ipc_connection_broken_pipe",
            original_timeout_ms: 1_000,
            original_daemon_session_id: Some("sess-original"),
        };

        let envelope = lifecycle
            .identity_changed_error(
                &io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"),
                &attempt,
                Some("sess-new"),
            )
            .into_envelope();
        let context = envelope.context.expect("replay identity-change context");
        assert_eq!(
            context["recovery_contract"]["kind"],
            serde_json::json!("session_post_commit_journal")
        );
        assert_eq!(
            context["recovery_contract"]["scope"],
            serde_json::json!("daemon_rollover_recovery")
        );
        assert_eq!(
            context["recovery_contract"]["session_name"],
            serde_json::json!("work")
        );
        assert_eq!(
            context["recovery_contract"]["daemon_session_id"],
            serde_json::json!("sess-original")
        );
        assert_eq!(
            context["recovery_contract"]["reader_contract"],
            serde_json::json!("ndjson_post_commit_journal")
        );
    }

    #[test]
    fn replay_budget_exhausted_error_keeps_public_recovery_contract() {
        let home = temp_home();
        let lifecycle = ReplaySendLifecycle {
            deadline: Instant::now() + Duration::from_secs(1),
            original_daemon_session_id: Some("sess-original"),
            strategy: ReplayReconnectStrategy::Existing {
                rub_home: &home,
                session: "default",
            },
        };
        let attempt = ReplayAttempt {
            started: Instant::now(),
            command_id: "cmd-1",
            retry_reason: "ipc_connection_broken_pipe",
            original_timeout_ms: 1_000,
            original_daemon_session_id: Some("sess-original"),
        };

        let envelope = lifecycle
            .budget_exhausted_after_transport(
                &io::Error::new(io::ErrorKind::TimedOut, "timed out"),
                &attempt,
                Some("replay_send"),
            )
            .into_envelope();
        let context = envelope.context.expect("replay timeout context");
        assert_eq!(
            context["recovery_contract"]["kind"],
            serde_json::json!("session_post_commit_journal")
        );
        assert_eq!(
            context["recovery_contract"]["safe_to_rerun_with_new_command_id"],
            serde_json::json!(false)
        );
    }
}
