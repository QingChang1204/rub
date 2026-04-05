use rub_core::model::{IdentityProbeStatus, LaunchPolicyInfo};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct DetectionRisk<'a> {
    pub(super) risk: &'a str,
    pub(super) severity: &'a str,
    pub(super) mitigation: &'a str,
}

pub(super) fn detection_risks(launch_policy: &LaunchPolicyInfo) -> Vec<DetectionRisk<'static>> {
    let mut risks = Vec::new();
    if launch_policy.headless {
        risks.push(DetectionRisk {
            risk: "headless_mode",
            severity: "medium",
            mitigation: "Use --headed for sites with strict headless detection",
        });
    }
    if !launch_policy.stealth_default_enabled.unwrap_or(true) {
        risks.push(DetectionRisk {
            risk: "stealth_disabled",
            severity: "high",
            mitigation: "Re-enable the default stealth baseline or omit --no-stealth",
        });
    }
    if launch_policy.user_data_dir.is_none() {
        risks.push(DetectionRisk {
            risk: "no_user_data_dir",
            severity: "low",
            mitigation: "Use --profile or --user-data-dir for a persistent browser profile",
        });
    }
    if let Some(coverage) = &launch_policy.stealth_coverage {
        if coverage.page_hook_failures.unwrap_or(0) > 0 {
            risks.push(DetectionRisk {
                risk: "stealth_patch_partial",
                severity: "medium",
                mitigation: "Inspect doctor.launch_policy.stealth_coverage for partial page-hook installation failures",
            });
        }
        if let Some(self_probe) = &coverage.self_probe {
            if matches!(
                self_probe.page_main_world,
                Some(IdentityProbeStatus::Failed)
            ) {
                risks.push(DetectionRisk {
                    risk: "page_identity_probe_failed",
                    severity: "high",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.page_main_world and the active stealth patch set",
                });
            }
            if matches!(self_probe.iframe_context, Some(IdentityProbeStatus::Failed)) {
                risks.push(DetectionRisk {
                    risk: "iframe_context_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.iframe_context for iframe identity drift",
                });
            }
            if matches!(
                self_probe.worker_context,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) && coverage.worker_targets_detected.unwrap_or(0) > 0
            {
                risks.push(DetectionRisk {
                    risk: "worker_context_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.worker_context for worker identity drift",
                });
            }
            if launch_policy.headless
                && matches!(
                    self_probe.ua_consistency,
                    Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
                )
            {
                risks.push(DetectionRisk {
                    risk: "ua_consistency_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.ua_consistency and userAgentMetadata override coverage",
                });
            }
            if matches!(
                self_probe.webgl_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "webgl_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.webgl_surface and the active fingerprint profile",
                });
            }
            if matches!(
                self_probe.canvas_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "canvas_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.canvas_surface and the active fingerprint profile",
                });
            }
            if matches!(
                self_probe.audio_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "audio_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.audio_surface and the active fingerprint profile",
                });
            }
            if matches!(
                self_probe.permissions_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "permissions_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.permissions_surface and the active stealth function-cloaking patches",
                });
            }
            if matches!(
                self_probe.viewport_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "viewport_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.viewport_surface and the active environment profile",
                });
            }
            if matches!(
                self_probe.touch_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "touch_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.touch_surface and the active environment profile",
                });
            }
            if matches!(
                self_probe.window_metrics_surface,
                Some(IdentityProbeStatus::Failed) | Some(IdentityProbeStatus::Unknown)
            ) {
                risks.push(DetectionRisk {
                    risk: "window_metrics_surface_unverified",
                    severity: "medium",
                    mitigation: "Inspect doctor.launch_policy.stealth_coverage.self_probe.window_metrics_surface and the active environment profile",
                });
            }
            if self_probe
                .unsupported_surfaces
                .iter()
                .any(|surface| surface == "service_worker")
                && coverage.service_worker_targets_detected.unwrap_or(0) > 0
            {
                risks.push(DetectionRisk {
                    risk: "service_worker_unverified",
                    severity: "medium",
                    mitigation: "Service worker targets were detected, but the current identity runtime does not yet self-probe that surface",
                });
            }
        } else if launch_policy.headless && !coverage.user_agent_metadata_override.unwrap_or(false)
        {
            risks.push(DetectionRisk {
                risk: "ua_metadata_unverified",
                severity: "medium",
                mitigation: "Apply protocol-level userAgentMetadata override so UA Client Hints match the cleaned User-Agent",
            });
        }
    }
    risks
}

pub(super) fn agent_capabilities() -> serde_json::Value {
    serde_json::json!({
        "a11y_state": true,
        "bootstrap_blank_tab_cleanup": true,
        "launch_policy_report": true,
        "non_blocking_wait": true,
        "scoped_cookie_clear": true,
        "startup_locking": true,
        "highlight_screenshot": true,
        "state_diff": true,
        "viewport_filter": true,
        "js_listener_detection": true,
        "persistent_js_context": true,
        "external_cdp_connect": true,
        "profile_connect": true,
        "batch_close": true,
        "cookie_url_filter": true,
        "env_session_selection": true,
        "integration_runtime_projection": true,
        "dialog_runtime_projection": true,
        "frame_runtime_projection": true,
        "network_rule_projection": true,
        "runtime_observatory_projection": true,
        "state_inspector_projection": true,
        "readiness_projection": true,
        "human_verification_handoff_projection": true,
        "takeover_runtime_projection": true,
        "orchestration_runtime_projection": true,
        "trigger_runtime_projection": true,
        "interference_runtime_projection": true,
        "interference_recovery": true,
        "download_runtime_projection": true,
        "storage_runtime_projection": true,
    })
}
