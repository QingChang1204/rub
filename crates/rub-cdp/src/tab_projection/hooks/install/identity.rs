use chromiumoxide::cdp::browser_protocol::emulation::{
    SetDeviceMetricsOverrideParams, SetTouchEmulationEnabledParams,
};
use tracing::warn;

use super::super::{
    IDENTITY_HOOKS_MASK, PageHookFlag, PageHookResult, user_agent_protocol_override_succeeded,
};
use super::{
    AuxiliaryPageHookSpec, PageHookInstaller, install_auxiliary_page_hook, page_hook_with_timeout,
};

impl<'a> PageHookInstaller<'a> {
    pub(super) async fn install_identity_hooks(&mut self) {
        if !self.context.identity_policy.stealth_enabled() {
            self.transaction.hook_state.mark_all(IDENTITY_HOOKS_MASK);
            return;
        }

        let context = self.context;
        let page = self.page.clone();
        let stealth_cfg = context.identity_policy.stealth_config();
        let super::PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;

        if let Some(environment_profile) = context.identity_policy.environment_profile() {
            install_auxiliary_page_hook(
                hook_state,
                AuxiliaryPageHookSpec {
                    hook: PageHookFlag::EnvironmentMetrics,
                    label: "stealth.environment_metrics",
                    warn_message: "Stealth environment metrics override failed",
                    record_timeout_coverage: false,
                },
                context,
                outcome,
                || async {
                    page.execute(SetDeviceMetricsOverrideParams::new(
                        environment_profile.window_width,
                        environment_profile.window_height,
                        environment_profile.device_scale_factor,
                        false,
                    ))
                    .await
                },
            )
            .await;

            let page = self.page.clone();
            install_auxiliary_page_hook(
                hook_state,
                AuxiliaryPageHookSpec {
                    hook: PageHookFlag::TouchEmulation,
                    label: "stealth.touch_emulation",
                    warn_message: "Stealth touch emulation override failed",
                    record_timeout_coverage: false,
                },
                context,
                outcome,
                || async {
                    page.execute(SetTouchEmulationEnabledParams::new(
                        environment_profile.touch_enabled,
                    ))
                    .await
                },
            )
            .await;
        } else {
            hook_state.mark(PageHookFlag::EnvironmentMetrics);
            hook_state.mark(PageHookFlag::TouchEmulation);
        }

        if let Some(script) = crate::stealth::combined_stealth_script(&stealth_cfg) {
            let page = self.page.clone();
            install_auxiliary_page_hook(
                hook_state,
                AuxiliaryPageHookSpec {
                    hook: PageHookFlag::StealthNewDocument,
                    label: "stealth.evaluate_on_new_document",
                    warn_message: "Stealth patch injection failed (evaluate_on_new_document)",
                    record_timeout_coverage: false,
                },
                context,
                outcome,
                || async { page.evaluate_on_new_document(script.as_str()).await },
            )
            .await;

            let page = self.page.clone();
            install_auxiliary_page_hook(
                hook_state,
                AuxiliaryPageHookSpec {
                    hook: PageHookFlag::StealthLive,
                    label: "stealth.evaluate",
                    warn_message: "Stealth patch injection failed (evaluate)",
                    record_timeout_coverage: false,
                },
                context,
                outcome,
                || async { page.evaluate(script.as_str()).await },
            )
            .await;
        } else {
            hook_state.mark(PageHookFlag::StealthNewDocument);
            hook_state.mark(PageHookFlag::StealthLive);
        }

        self.install_user_agent_hook().await;
    }

    async fn install_user_agent_hook(&mut self) {
        if self
            .transaction
            .hook_state
            .contains(PageHookFlag::UserAgent)
        {
            return;
        }

        let context = self.context;
        let page = self.page.clone();
        match page_hook_with_timeout("ua.read_current_user_agent", || async {
            page.user_agent().await
        })
        .await
        {
            PageHookResult::Completed(Ok(ua)) => {
                if let Some(profile) = context.identity_policy.user_agent_override(&ua) {
                    let page = self.page.clone();
                    let protocol_override_result =
                        page_hook_with_timeout("ua.set_user_agent", || async {
                            page.set_user_agent(profile.params.clone()).await
                        })
                        .await;
                    let protocol_override_applied =
                        user_agent_protocol_override_succeeded(&protocol_override_result);
                    match protocol_override_result {
                        PageHookResult::Completed(Ok(_)) => {
                            self.transaction.hook_state.mark(PageHookFlag::UserAgent);
                            context
                                .identity_coverage
                                .lock()
                                .await
                                .record_user_agent_override(profile.has_metadata);
                        }
                        PageHookResult::Completed(Err(error)) => {
                            warn!(error = %error, "User-Agent override failed (set_user_agent)");
                            self.transaction
                                .outcome
                                .mark_auxiliary_failure(context, true)
                                .await;
                        }
                        PageHookResult::TimedOut => {
                            self.transaction
                                .outcome
                                .mark_auxiliary_failure(context, false)
                                .await;
                        }
                    }
                    if !protocol_override_applied {
                        let page = self.page.clone();
                        let fallback_new_document =
                            page_hook_with_timeout("ua.evaluate_on_new_document", || async {
                                page.evaluate_on_new_document(profile.script.as_str()).await
                            })
                            .await;
                        let page = self.page.clone();
                        let fallback_live = page_hook_with_timeout("ua.evaluate", || async {
                            page.evaluate(profile.script.as_str()).await
                        })
                        .await;
                        match (&fallback_new_document, &fallback_live) {
                            (
                                PageHookResult::Completed(Ok(_)),
                                PageHookResult::Completed(Ok(_)),
                            ) => {
                                self.transaction.hook_state.mark(PageHookFlag::UserAgent);
                                context
                                    .identity_coverage
                                    .lock()
                                    .await
                                    .record_user_agent_override(false);
                            }
                            _ => {
                                self.transaction
                                    .outcome
                                    .mark_auxiliary_failure(context, true)
                                    .await
                            }
                        }
                        if let PageHookResult::Completed(Err(ref error)) = fallback_new_document {
                            warn!(
                                error = %error,
                                "User-Agent override fallback failed (evaluate_on_new_document)"
                            );
                        }
                        if let PageHookResult::Completed(Err(ref error)) = fallback_live {
                            warn!(error = %error, "User-Agent override fallback failed (evaluate)");
                        }
                    }
                } else {
                    self.transaction.hook_state.mark(PageHookFlag::UserAgent);
                }
            }
            PageHookResult::Completed(Err(error)) => {
                warn!(error = %error, "Failed to read current User-Agent for hook installation");
                self.transaction
                    .outcome
                    .mark_auxiliary_failure(context, true)
                    .await;
            }
            PageHookResult::TimedOut => {
                self.transaction
                    .outcome
                    .mark_auxiliary_failure(context, true)
                    .await
            }
        }
    }
}
