use serde_json::{Value, json};

pub const STORAGE_RECENT_MUTATIONS_AUTHORITY: &str = "storage_runtime.recent_mutations";
const ALREADY_EXECUTED_RESPONSE_EVICTED_DO_NOT_RERUN: &str =
    "already_executed_response_evicted_do_not_rerun";
const NO_PUBLIC_RECOVERY_CONTRACT: &str = "no_public_recovery_contract";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCommitState {
    PossibleCommit,
}

impl EffectCommitState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PossibleCommit => "possible_commit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackAuthority {
    CommandSpecificReplayOrRecoveryContract,
    PendingExternalDomChange,
    LiveViewportState,
    SessionStateRegistry,
    SpentWithoutCachedResponse,
}

impl FallbackAuthority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandSpecificReplayOrRecoveryContract => {
                "command_specific_replay_or_recovery_contract"
            }
            Self::PendingExternalDomChange => "pending_external_dom_change",
            Self::LiveViewportState => "live_viewport_state",
            Self::SessionStateRegistry => "session_state_registry",
            Self::SpentWithoutCachedResponse => "spent_without_cached_response",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryContractKind {
    PartialCommit,
    CommandPossibleCommit,
    RegistryCommit,
    TargetReplayOrSpentTombstone,
    SessionPostCommitJournal,
    StorageMutation,
    InteractionPossibleCommit,
    ViewportSideEffectPossibleCommit,
    FillAtomicPossibleCommit,
    AtomicFillRollback,
    PostCommitJournalRecovery,
}

impl RecoveryContractKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PartialCommit => "partial_commit",
            Self::CommandPossibleCommit => "command_possible_commit",
            Self::RegistryCommit => "registry_commit",
            Self::TargetReplayOrSpentTombstone => "target_replay_or_spent_tombstone",
            Self::SessionPostCommitJournal => "session_post_commit_journal",
            Self::StorageMutation => "storage_mutation",
            Self::InteractionPossibleCommit => "interaction_possible_commit",
            Self::ViewportSideEffectPossibleCommit => "viewport_side_effect_possible_commit",
            Self::FillAtomicPossibleCommit => "fill_atomic_possible_commit",
            Self::AtomicFillRollback => "atomic_fill_rollback",
            Self::PostCommitJournalRecovery => "post_commit_journal_recovery",
        }
    }
}

pub fn partial_commit_steps_contract() -> Value {
    json!({
        "kind": RecoveryContractKind::PartialCommit.as_str(),
        "committed_steps_authoritative": true,
        "rollback_available": false,
        "resume_from_failed_step_supported": false,
    })
}

pub fn orchestration_partial_commit_steps_contract(
    possibly_committed_step: Option<Value>,
) -> Value {
    json!({
        "kind": RecoveryContractKind::PartialCommit.as_str(),
        "committed_steps_authoritative": true,
        "rollback_available": false,
        "resume_from_failed_step_supported": false,
        "possibly_committed_step": possibly_committed_step,
    })
}

pub fn registry_commit_contract() -> Value {
    json!({
        "kind": RecoveryContractKind::RegistryCommit.as_str(),
        "committed_projection_authoritative": true,
        "rollback_available": false,
        "resume_supported": false,
    })
}

pub fn command_possible_commit_contract(command: &str, command_id: Option<&str>) -> Value {
    json!({
        "kind": RecoveryContractKind::CommandPossibleCommit.as_str(),
        "command": command,
        "command_id": command_id,
        "effect_commit_state": EffectCommitState::PossibleCommit.as_str(),
        "projection_authoritative": false,
        "retry_requires_same_command_id": command_id.is_some(),
        "fresh_command_retry_safe": false,
        "fallback_authority": FallbackAuthority::CommandSpecificReplayOrRecoveryContract.as_str(),
    })
}

pub fn interaction_possible_commit_contract(command: &str, redacted_request: Value) -> Value {
    json!({
        "kind": RecoveryContractKind::InteractionPossibleCommit.as_str(),
        "command": command,
        "same_command_retry_requires_same_command_id": true,
        "request": redacted_request,
    })
}

pub fn same_epoch_viewport_side_effect_contract(command: &str) -> Value {
    json!({
        "command": command,
        "effect_commit_state": EffectCommitState::PossibleCommit.as_str(),
        "cache_fence": "snapshot_cache_cleared",
        "fallback_authority": FallbackAuthority::LiveViewportState.as_str(),
        "recovery_contract": {
            "kind": RecoveryContractKind::ViewportSideEffectPossibleCommit.as_str(),
            "fresh_snapshot_required": true,
            "fresh_command_retry_safe": false,
        },
    })
}

pub fn fill_atomic_possible_commit_contract(
    step_index: usize,
    committed_step_indices: &[usize],
    rollback_command: &str,
    rollback_class: &str,
) -> Value {
    json!({
        "kind": RecoveryContractKind::FillAtomicPossibleCommit.as_str(),
        "step_index": step_index,
        "committed_step_indices": committed_step_indices,
        "rollback_required": true,
        "rollback_command": rollback_command,
        "rollback_class": rollback_class,
    })
}

pub fn atomic_fill_rollback_contract(rollback_failed: bool) -> Value {
    json!({
        "kind": RecoveryContractKind::AtomicFillRollback.as_str(),
        "rollback_authority": "fill_atomic",
        "rollback_result_authoritative": true,
        "rollback_committed": !rollback_failed,
        "rollback_failed": rollback_failed,
        "steps_authoritative": true,
        "retry_same_command_safe": false,
        "resume_from_failed_step_supported": false,
    })
}

pub fn post_commit_journal_recovery_contract(failure_count: u64) -> Value {
    json!({
        "kind": RecoveryContractKind::PostCommitJournalRecovery.as_str(),
        "daemon_commit_truth_preserved": true,
        "journal_append_authoritative": failure_count == 0,
        "operator_action": if failure_count == 0 {
            "none"
        } else {
            "inspect daemon logs and runtime post_commit_journal surface before relying on local recovery journal completeness"
        },
    })
}

pub fn already_executed_response_evicted_do_not_rerun_contract() -> Value {
    json!(ALREADY_EXECUTED_RESPONSE_EVICTED_DO_NOT_RERUN)
}

pub fn no_public_recovery_contract() -> Value {
    json!(NO_PUBLIC_RECOVERY_CONTRACT)
}

pub fn target_replay_or_spent_tombstone_contract(
    target_command_id: Option<&str>,
    target_daemon_session_id: Option<&str>,
) -> Value {
    target_replay_or_spent_tombstone_contract_with_fresh_retry_field(
        target_command_id,
        target_daemon_session_id,
        false,
    )
}

pub fn target_replay_or_spent_tombstone_contract_with_fresh_retry_field(
    target_command_id: Option<&str>,
    target_daemon_session_id: Option<&str>,
    include_fresh_command_retry_safe: bool,
) -> Value {
    let mut contract = serde_json::Map::from_iter([
        (
            "kind".to_string(),
            json!(RecoveryContractKind::TargetReplayOrSpentTombstone.as_str()),
        ),
        ("target_command_id".to_string(), json!(target_command_id)),
        (
            "target_daemon_session_id".to_string(),
            json!(target_daemon_session_id),
        ),
        (
            "retry_requires_same_command_id".to_string(),
            json!(target_command_id.is_some()),
        ),
    ]);
    if include_fresh_command_retry_safe {
        contract.insert("fresh_command_retry_safe".to_string(), json!(false));
    }
    Value::Object(contract)
}

pub fn storage_partial_commit_contract(authoritative_surface: &'static str) -> Value {
    json!({
        "kind": RecoveryContractKind::PartialCommit.as_str(),
        "authoritative_surface": authoritative_surface,
    })
}

pub fn storage_mutation_partial_commit_projection(authoritative_surface: &'static str) -> Value {
    json!({
        "kind": RecoveryContractKind::StorageMutation.as_str(),
        "recovery_contract": storage_partial_commit_contract(authoritative_surface),
    })
}

pub fn session_post_commit_journal_recovery_contract(
    session_name: &str,
    daemon_session_id: &str,
    journal_path: String,
) -> Value {
    json!({
        "kind": RecoveryContractKind::SessionPostCommitJournal.as_str(),
        "scope": "daemon_rollover_recovery",
        "session_name": session_name,
        "daemon_session_id": daemon_session_id,
        "journal_path": journal_path,
        "reader_contract": "ndjson_post_commit_journal",
        "committed_truth_may_exist": true,
        "safe_to_rerun_with_new_command_id": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_possible_commit_contract_serializes_public_shape() {
        assert_eq!(
            command_possible_commit_contract("scroll", Some("cmd-1")),
            json!({
                "kind": "command_possible_commit",
                "command": "scroll",
                "command_id": "cmd-1",
                "effect_commit_state": "possible_commit",
                "projection_authoritative": false,
                "retry_requires_same_command_id": true,
                "fresh_command_retry_safe": false,
                "fallback_authority": "command_specific_replay_or_recovery_contract",
            })
        );
    }

    #[test]
    fn interaction_possible_commit_contract_preserves_redacted_request_shape() {
        assert_eq!(
            interaction_possible_commit_contract(
                "type",
                json!({
                    "locator": {"selector": "input.password"},
                    "arguments_redacted": true,
                }),
            ),
            json!({
                "kind": "interaction_possible_commit",
                "command": "type",
                "same_command_retry_requires_same_command_id": true,
                "request": {
                    "locator": {"selector": "input.password"},
                    "arguments_redacted": true,
                },
            })
        );
    }

    #[test]
    fn same_epoch_viewport_side_effect_contract_preserves_recovery_shape() {
        assert_eq!(
            same_epoch_viewport_side_effect_contract("scroll"),
            json!({
                "command": "scroll",
                "effect_commit_state": "possible_commit",
                "cache_fence": "snapshot_cache_cleared",
                "fallback_authority": "live_viewport_state",
                "recovery_contract": {
                    "kind": "viewport_side_effect_possible_commit",
                    "fresh_snapshot_required": true,
                    "fresh_command_retry_safe": false,
                },
            })
        );
    }

    #[test]
    fn fill_atomic_possible_commit_contract_preserves_rollback_shape() {
        assert_eq!(
            fill_atomic_possible_commit_contract(2, &[0, 1], "type", "text_restore"),
            json!({
                "kind": "fill_atomic_possible_commit",
                "step_index": 2,
                "committed_step_indices": [0, 1],
                "rollback_required": true,
                "rollback_command": "type",
                "rollback_class": "text_restore",
            })
        );
    }

    #[test]
    fn atomic_fill_rollback_contract_preserves_authority_shape() {
        assert_eq!(
            atomic_fill_rollback_contract(true),
            json!({
                "kind": "atomic_fill_rollback",
                "rollback_authority": "fill_atomic",
                "rollback_result_authoritative": true,
                "rollback_committed": false,
                "rollback_failed": true,
                "steps_authoritative": true,
                "retry_same_command_safe": false,
                "resume_from_failed_step_supported": false,
            })
        );
        assert_eq!(
            atomic_fill_rollback_contract(false)["rollback_committed"],
            true
        );
    }

    #[test]
    fn post_commit_journal_recovery_contract_preserves_health_shape() {
        assert_eq!(
            post_commit_journal_recovery_contract(0),
            json!({
                "kind": "post_commit_journal_recovery",
                "daemon_commit_truth_preserved": true,
                "journal_append_authoritative": true,
                "operator_action": "none",
            })
        );
        assert_eq!(
            post_commit_journal_recovery_contract(2),
            json!({
                "kind": "post_commit_journal_recovery",
                "daemon_commit_truth_preserved": true,
                "journal_append_authoritative": false,
                "operator_action": "inspect daemon logs and runtime post_commit_journal surface before relying on local recovery journal completeness",
            })
        );
    }

    #[test]
    fn string_sentinel_contracts_preserve_wire_shape() {
        assert_eq!(
            already_executed_response_evicted_do_not_rerun_contract(),
            json!("already_executed_response_evicted_do_not_rerun")
        );
        assert_eq!(
            no_public_recovery_contract(),
            json!("no_public_recovery_contract")
        );
    }

    #[test]
    fn target_replay_contract_preserves_optional_fresh_retry_field() {
        assert_eq!(
            target_replay_or_spent_tombstone_contract(Some("cmd-1"), Some("daemon-1")),
            json!({
                "kind": "target_replay_or_spent_tombstone",
                "target_command_id": "cmd-1",
                "target_daemon_session_id": "daemon-1",
                "retry_requires_same_command_id": true,
            })
        );
        assert_eq!(
            target_replay_or_spent_tombstone_contract_with_fresh_retry_field(
                Some("cmd-1"),
                Some("daemon-1"),
                true
            ),
            json!({
                "kind": "target_replay_or_spent_tombstone",
                "target_command_id": "cmd-1",
                "target_daemon_session_id": "daemon-1",
                "retry_requires_same_command_id": true,
                "fresh_command_retry_safe": false,
            })
        );
    }

    #[test]
    fn partial_commit_contracts_preserve_stable_keys() {
        assert_eq!(
            partial_commit_steps_contract(),
            json!({
                "kind": "partial_commit",
                "committed_steps_authoritative": true,
                "rollback_available": false,
                "resume_from_failed_step_supported": false,
            })
        );
        assert_eq!(
            orchestration_partial_commit_steps_contract(None),
            json!({
                "kind": "partial_commit",
                "committed_steps_authoritative": true,
                "rollback_available": false,
                "resume_from_failed_step_supported": false,
                "possibly_committed_step": null,
            })
        );
    }

    #[test]
    fn storage_mutation_contract_preserves_authoritative_surface() {
        assert_eq!(
            storage_mutation_partial_commit_projection(STORAGE_RECENT_MUTATIONS_AUTHORITY),
            json!({
                "kind": "storage_mutation",
                "recovery_contract": {
                    "kind": "partial_commit",
                    "authoritative_surface": "storage_runtime.recent_mutations",
                },
            })
        );
    }

    #[test]
    fn session_post_commit_journal_contract_serializes_public_shape() {
        assert_eq!(
            session_post_commit_journal_recovery_contract(
                "default",
                "daemon-1",
                "/tmp/rub/session/post_commit_journal.ndjson".to_string()
            ),
            json!({
                "kind": "session_post_commit_journal",
                "scope": "daemon_rollover_recovery",
                "session_name": "default",
                "daemon_session_id": "daemon-1",
                "journal_path": "/tmp/rub/session/post_commit_journal.ndjson",
                "reader_contract": "ndjson_post_commit_journal",
                "committed_truth_may_exist": true,
                "safe_to_rerun_with_new_command_id": false,
            })
        );
    }
}
