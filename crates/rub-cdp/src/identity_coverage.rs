//! Runtime coverage tracking for stealth / identity installation.

use std::collections::BTreeMap;

use rub_core::model::{IdentityProbeStatus, IdentitySelfProbeInfo, StealthCoverageInfo};

use crate::identity_policy::IdentityPolicy;

#[derive(Debug, Clone)]
pub struct IdentityCoverageRegistry {
    coverage_mode: String,
    observed_targets: BTreeMap<String, String>,
    page_hook_installations: u32,
    page_hook_failures: u32,
    user_agent_override: bool,
    user_agent_metadata_override: bool,
    self_probe: Option<IdentitySelfProbeInfo>,
}

impl IdentityCoverageRegistry {
    pub fn new(policy: &IdentityPolicy) -> Self {
        Self {
            coverage_mode: policy.coverage_mode().as_str().to_string(),
            observed_targets: BTreeMap::new(),
            page_hook_installations: 0,
            page_hook_failures: 0,
            user_agent_override: false,
            user_agent_metadata_override: false,
            self_probe: None,
        }
    }

    pub fn record_target(&mut self, target_id: impl Into<String>, target_type: impl Into<String>) {
        self.observed_targets
            .insert(target_id.into(), target_type.into());
    }

    pub fn remove_target(&mut self, target_id: &str) {
        self.observed_targets.remove(target_id);
    }

    pub fn record_page_hook_installation(&mut self) {
        self.page_hook_installations = self.page_hook_installations.saturating_add(1);
    }

    pub fn record_page_hook_failure(&mut self) {
        self.page_hook_failures = self.page_hook_failures.saturating_add(1);
    }

    pub fn record_user_agent_override(&mut self, with_metadata: bool) {
        self.user_agent_override = true;
        if with_metadata {
            self.user_agent_metadata_override = true;
        }
    }

    pub fn record_self_probe(&mut self, probe: IdentitySelfProbeInfo) {
        self.self_probe = Some(match self.self_probe.take() {
            Some(existing) => merge_probe(existing, probe),
            None => probe,
        });
    }

    pub fn project(&self) -> StealthCoverageInfo {
        let mut unique_types = Vec::new();
        let mut iframe_targets = 0u32;
        let mut worker_targets = 0u32;
        let mut service_worker_targets = 0u32;
        let mut shared_worker_targets = 0u32;

        let mut last_type = None::<&str>;
        for target_type in self.observed_targets.values().map(String::as_str) {
            if last_type != Some(target_type)
                && !unique_types.iter().any(|seen| seen == target_type)
            {
                unique_types.push(target_type.to_string());
                last_type = Some(target_type);
            }

            match target_type {
                "iframe" => iframe_targets = iframe_targets.saturating_add(1),
                "worker" | "dedicated_worker" => {
                    worker_targets = worker_targets.saturating_add(1);
                }
                "service_worker" => {
                    worker_targets = worker_targets.saturating_add(1);
                    service_worker_targets = service_worker_targets.saturating_add(1);
                }
                "shared_worker" => {
                    worker_targets = worker_targets.saturating_add(1);
                    shared_worker_targets = shared_worker_targets.saturating_add(1);
                }
                _ => {}
            }
        }

        StealthCoverageInfo {
            coverage_mode: Some(self.coverage_mode.clone()),
            page_hook_installations: Some(self.page_hook_installations),
            page_hook_failures: Some(self.page_hook_failures),
            iframe_targets_detected: Some(iframe_targets),
            worker_targets_detected: Some(worker_targets),
            service_worker_targets_detected: Some(service_worker_targets),
            shared_worker_targets_detected: Some(shared_worker_targets),
            user_agent_override: Some(self.user_agent_override),
            user_agent_metadata_override: Some(self.user_agent_metadata_override),
            observed_target_types: unique_types,
            self_probe: self.self_probe.clone(),
        }
    }
}

fn merge_probe(
    mut existing: IdentitySelfProbeInfo,
    next: IdentitySelfProbeInfo,
) -> IdentitySelfProbeInfo {
    existing.page_main_world = merge_probe_status(existing.page_main_world, next.page_main_world);
    existing.iframe_context = merge_probe_status(existing.iframe_context, next.iframe_context);
    existing.worker_context = merge_probe_status(existing.worker_context, next.worker_context);
    existing.ua_consistency = merge_probe_status(existing.ua_consistency, next.ua_consistency);
    existing.webgl_surface = merge_probe_status(existing.webgl_surface, next.webgl_surface);
    existing.canvas_surface = merge_probe_status(existing.canvas_surface, next.canvas_surface);
    existing.audio_surface = merge_probe_status(existing.audio_surface, next.audio_surface);
    existing.permissions_surface =
        merge_probe_status(existing.permissions_surface, next.permissions_surface);
    existing.viewport_surface =
        merge_probe_status(existing.viewport_surface, next.viewport_surface);
    existing.touch_surface = merge_probe_status(existing.touch_surface, next.touch_surface);
    existing.window_metrics_surface =
        merge_probe_status(existing.window_metrics_surface, next.window_metrics_surface);
    for surface in next.unsupported_surfaces {
        if !existing
            .unsupported_surfaces
            .iter()
            .any(|value| value == &surface)
        {
            existing.unsupported_surfaces.push(surface);
        }
    }
    existing
}

fn merge_probe_status(
    current: Option<IdentityProbeStatus>,
    next: Option<IdentityProbeStatus>,
) -> Option<IdentityProbeStatus> {
    match (current, next) {
        (Some(IdentityProbeStatus::Failed), _) | (_, Some(IdentityProbeStatus::Failed)) => {
            Some(IdentityProbeStatus::Failed)
        }
        (Some(IdentityProbeStatus::Passed), _) | (_, Some(IdentityProbeStatus::Passed)) => {
            Some(IdentityProbeStatus::Passed)
        }
        (Some(IdentityProbeStatus::Unknown), _) | (_, Some(IdentityProbeStatus::Unknown)) => {
            Some(IdentityProbeStatus::Unknown)
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserLaunchOptions;

    fn policy() -> IdentityPolicy {
        IdentityPolicy::from_options(&BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        })
    }

    #[test]
    fn coverage_projection_counts_worker_targets_without_double_counting_updates() {
        let mut registry = IdentityCoverageRegistry::new(&policy());
        registry.record_target("page-1", "page");
        registry.record_target("worker-1", "worker");
        registry.record_target("worker-1", "worker");
        registry.record_target("shared-1", "shared_worker");
        registry.record_page_hook_installation();
        registry.record_user_agent_override(true);

        let projection = registry.project();
        assert_eq!(projection.page_hook_installations, Some(1));
        assert_eq!(projection.worker_targets_detected, Some(2));
        assert_eq!(projection.shared_worker_targets_detected, Some(1));
        assert_eq!(projection.user_agent_metadata_override, Some(true));
        assert!(
            projection
                .observed_target_types
                .contains(&"page".to_string())
        );
        assert!(
            projection
                .observed_target_types
                .contains(&"worker".to_string())
        );
    }

    #[test]
    fn record_self_probe_merges_with_failure_precedence() {
        let mut registry = IdentityCoverageRegistry::new(&policy());
        registry.record_self_probe(IdentitySelfProbeInfo {
            page_main_world: Some(IdentityProbeStatus::Passed),
            iframe_context: Some(IdentityProbeStatus::Passed),
            worker_context: Some(IdentityProbeStatus::Unknown),
            ua_consistency: Some(IdentityProbeStatus::Passed),
            webgl_surface: Some(IdentityProbeStatus::Passed),
            canvas_surface: Some(IdentityProbeStatus::Passed),
            audio_surface: Some(IdentityProbeStatus::Unknown),
            permissions_surface: Some(IdentityProbeStatus::Passed),
            viewport_surface: Some(IdentityProbeStatus::Passed),
            touch_surface: Some(IdentityProbeStatus::Passed),
            window_metrics_surface: Some(IdentityProbeStatus::Unknown),
            unsupported_surfaces: vec!["service_worker".to_string()],
        });
        registry.record_self_probe(IdentitySelfProbeInfo {
            page_main_world: Some(IdentityProbeStatus::Passed),
            iframe_context: Some(IdentityProbeStatus::Failed),
            worker_context: Some(IdentityProbeStatus::Passed),
            ua_consistency: Some(IdentityProbeStatus::Passed),
            webgl_surface: Some(IdentityProbeStatus::Passed),
            canvas_surface: Some(IdentityProbeStatus::Failed),
            audio_surface: Some(IdentityProbeStatus::Passed),
            permissions_surface: Some(IdentityProbeStatus::Failed),
            viewport_surface: Some(IdentityProbeStatus::Unknown),
            touch_surface: Some(IdentityProbeStatus::Failed),
            window_metrics_surface: Some(IdentityProbeStatus::Passed),
            unsupported_surfaces: vec!["service_worker".to_string()],
        });

        let projection = registry.project();
        let self_probe = projection.self_probe.expect("probe should exist");
        assert_eq!(
            self_probe.page_main_world,
            Some(IdentityProbeStatus::Passed)
        );
        assert_eq!(self_probe.iframe_context, Some(IdentityProbeStatus::Failed));
        assert_eq!(self_probe.worker_context, Some(IdentityProbeStatus::Passed));
        assert_eq!(self_probe.ua_consistency, Some(IdentityProbeStatus::Passed));
        assert_eq!(self_probe.webgl_surface, Some(IdentityProbeStatus::Passed));
        assert_eq!(self_probe.canvas_surface, Some(IdentityProbeStatus::Failed));
        assert_eq!(self_probe.audio_surface, Some(IdentityProbeStatus::Passed));
        assert_eq!(
            self_probe.permissions_surface,
            Some(IdentityProbeStatus::Failed)
        );
        assert_eq!(
            self_probe.viewport_surface,
            Some(IdentityProbeStatus::Passed)
        );
        assert_eq!(self_probe.touch_surface, Some(IdentityProbeStatus::Failed));
        assert_eq!(
            self_probe.window_metrics_surface,
            Some(IdentityProbeStatus::Passed)
        );
        assert_eq!(self_probe.unsupported_surfaces, vec!["service_worker"]);
    }
}
