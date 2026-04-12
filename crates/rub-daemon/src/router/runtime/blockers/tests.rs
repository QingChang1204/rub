use super::{blocker_diagnosis_payload, blocker_diagnosis_result};
use rub_core::model::{
    HumanVerificationHandoffInfo, HumanVerificationHandoffStatus, ReadinessInfo, ReadinessStatus,
};

#[test]
fn blocker_diagnosis_prefers_provider_gate_over_overlay_noise() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        overlay_state: rub_core::model::OverlayState::UserBlocking,
        blocking_signals: vec!["overlay:user_blocking".to_string()],
        ..ReadinessInfo::default()
    };
    let interference = rub_core::model::InterferenceRuntimeInfo {
        status: rub_core::model::InterferenceRuntimeStatus::Active,
        current_interference: Some(rub_core::model::InterferenceObservation {
            kind: rub_core::model::InterferenceKind::HumanVerificationRequired,
            summary: "cloudflare gate".to_string(),
            current_url: Some("https://mail.example/challenge".to_string()),
            primary_url: Some("https://mail.example/inbox".to_string()),
        }),
        handoff_required: true,
        ..rub_core::model::InterferenceRuntimeInfo::default()
    };
    let handoff = HumanVerificationHandoffInfo {
        status: HumanVerificationHandoffStatus::Available,
        automation_paused: false,
        resume_supported: true,
        unavailable_reason: None,
    };

    let result = blocker_diagnosis_result(&readiness, &interference, &handoff);
    assert_eq!(result["class"], "provider_gate");
    assert_eq!(result["primary_reason"], "handoff_required");
    assert!(
        result["summary"]
            .as_str()
            .expect("summary")
            .contains("required handoff")
    );
    assert_eq!(
        result["workflow_guidance"]["continuation_kind"],
        "fresh_rub_home"
    );
    assert_eq!(
        result["workflow_guidance"]["recommended_runtime"]["kind"],
        "fresh_rub_home"
    );
    assert_eq!(result["workflow_guidance"]["signal"], "handoff_required");
    assert_eq!(
        result["workflow_guidance"]["runtime_roles"]["current_runtime"]["role"],
        "gated_recovery_runtime"
    );
    assert_eq!(
        result["workflow_guidance"]["runtime_roles"]["recommended_runtime"]["role"],
        "alternate_provider_runtime"
    );
}

#[test]
fn blocker_diagnosis_keeps_active_handoff_in_same_runtime() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        ..ReadinessInfo::default()
    };
    let interference = rub_core::model::InterferenceRuntimeInfo {
        status: rub_core::model::InterferenceRuntimeStatus::Active,
        current_interference: Some(rub_core::model::InterferenceObservation {
            kind: rub_core::model::InterferenceKind::HumanVerificationRequired,
            summary: "challenge active".to_string(),
            current_url: Some("https://mail.example/challenge".to_string()),
            primary_url: Some("https://mail.example/inbox".to_string()),
        }),
        handoff_required: true,
        ..rub_core::model::InterferenceRuntimeInfo::default()
    };
    let handoff = HumanVerificationHandoffInfo {
        status: HumanVerificationHandoffStatus::Active,
        automation_paused: true,
        resume_supported: true,
        unavailable_reason: None,
    };

    let result = blocker_diagnosis_result(&readiness, &interference, &handoff);
    assert_eq!(result["class"], "provider_gate");
    assert_eq!(result["primary_reason"], "handoff_active");
    assert_eq!(
        result["workflow_guidance"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        result["workflow_guidance"]["recommended_runtime"]["kind"],
        "current_runtime"
    );
    assert_eq!(
        result["workflow_guidance"]["next_command_hints"][1]["command"],
        "rub handoff complete"
    );
    assert_eq!(
        result["workflow_guidance"]["runtime_roles"]["current_runtime"]["role"],
        "manual_recovery_runtime"
    );
}

#[test]
fn blocker_diagnosis_marks_degraded_runtime_as_not_authoritative() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Degraded,
        degraded_reason: Some("probe_timeout".to_string()),
        ..ReadinessInfo::default()
    };

    let result = blocker_diagnosis_result(
        &readiness,
        &rub_core::model::InterferenceRuntimeInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );
    assert_eq!(result["class"], "degraded_runtime");
    assert_eq!(result["authoritative"], false);
}

#[test]
fn blocker_diagnosis_payload_projects_concise_surface_first() {
    let payload = blocker_diagnosis_payload(
        &ReadinessInfo::default(),
        &rub_core::model::InterferenceRuntimeInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );
    assert_eq!(payload["subject"]["kind"], "blocker_explain");
    assert_eq!(payload["subject"]["surface"], "runtime_blockers");
    assert_eq!(payload["result"]["diagnosis"]["class"], "clear");
    assert_eq!(payload["result"]["diagnosis"]["primary_reason"], "clear");
    assert_eq!(
        payload["result"]["diagnosis"]["workflow_guidance"]["continuation_kind"],
        "same_runtime"
    );
}

#[test]
fn blocker_diagnosis_marks_loading_as_primary_route_transition_reason() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        loading_present: true,
        ..ReadinessInfo::default()
    };

    let result = blocker_diagnosis_result(
        &readiness,
        &rub_core::model::InterferenceRuntimeInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );
    assert_eq!(result["class"], "route_transition");
    assert_eq!(result["primary_reason"], "loading_present");
    assert!(
        result["summary"]
            .as_str()
            .expect("summary")
            .contains("Loading blockers")
    );
    assert_eq!(result["workflow_guidance"]["signal"], "loading_present");
    assert_eq!(
        result["workflow_guidance"]["next_command_hints"][0]["command"],
        "rub runtime readiness"
    );
    assert_eq!(
        result["workflow_guidance"]["next_command_hints"][1]["command"],
        "rub wait --selector ... --state visible"
    );
    assert_eq!(
        result["workflow_guidance"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime"
    );
}

#[test]
fn blocker_diagnosis_marks_overlay_interference_as_recoverable_same_runtime_path() {
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        overlay_state: rub_core::model::OverlayState::UserBlocking,
        ..ReadinessInfo::default()
    };
    let interference = rub_core::model::InterferenceRuntimeInfo {
        status: rub_core::model::InterferenceRuntimeStatus::Active,
        current_interference: Some(rub_core::model::InterferenceObservation {
            kind: rub_core::model::InterferenceKind::OverlayInterference,
            summary: "cookie overlay".to_string(),
            current_url: None,
            primary_url: None,
        }),
        ..rub_core::model::InterferenceRuntimeInfo::default()
    };

    let result = blocker_diagnosis_result(
        &readiness,
        &interference,
        &HumanVerificationHandoffInfo::default(),
    );
    assert_eq!(result["class"], "overlay_blocker");
    assert_eq!(result["primary_reason"], "overlay_interference");
    assert_eq!(
        result["workflow_guidance"]["next_command_hints"][0]["command"],
        "rub interference recover"
    );
    assert_eq!(
        result["workflow_guidance"]["signal"],
        "overlay_interference"
    );
    assert_eq!(
        result["workflow_guidance"]["runtime_roles"]["current_runtime"]["role"],
        "manual_recovery_runtime"
    );
}
