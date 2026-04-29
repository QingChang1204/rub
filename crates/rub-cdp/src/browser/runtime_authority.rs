use super::*;
use crate::identity_coverage::IdentityCoverageRegistry;
use crate::request_correlation::CORRELATION_BROWSER_AUTHORITY_REBUILD_FAILED_REASON;
use crate::tab_projection::{CommittedTabProjection, LocalActiveTargetAuthority};
use rub_core::model::DialogRuntimeStatus;

const BROWSER_AUTHORITY_REBUILD_FAILED_REASON: &str = "browser_authority_rebuild_failed";
const PREVIOUS_AUTHORITY_CLEANUP_FAILED_AFTER_RELEASE_REASON: &str =
    "previous_authority_cleanup_failed_after_release";

#[derive(Clone)]
pub(super) struct BrowserAuthoritySnapshot {
    browser: Arc<Browser>,
    page: Arc<Page>,
    is_external: bool,
    connection_target: Option<ConnectionTarget>,
    managed_profile: Option<ManagedProfileDir>,
    tab_projection: CommittedTabProjection,
    local_active_target_authority: Option<LocalActiveTargetAuthority>,
    dialog_runtime: rub_core::model::DialogRuntimeInfo,
    download_runtime: crate::downloads::DownloadRuntimeProjectionState,
    dialog_intercept: Option<rub_core::model::DialogInterceptPolicy>,
    network_rule_runtime: NetworkRuleRuntime,
    request_correlation: RequestCorrelationRegistry,
    observatory_pending_registries:
        HashMap<String, crate::runtime_observatory::SharedPendingRequestRegistry>,
    identity_coverage: IdentityCoverageRegistry,
}

#[derive(Clone)]
pub(super) struct BrowserRuntimeFallbackSnapshot {
    dialog_runtime: rub_core::model::DialogRuntimeInfo,
    download_runtime: crate::downloads::DownloadRuntimeProjectionState,
    network_rule_runtime: NetworkRuleRuntime,
    request_correlation: RequestCorrelationRegistry,
    observatory_pending_registries:
        HashMap<String, crate::runtime_observatory::SharedPendingRequestRegistry>,
}

pub(super) struct BrowserAuthorityInstall {
    pub(super) browser: Arc<Browser>,
    pub(super) page: Arc<Page>,
    pub(super) is_external: bool,
    pub(super) connection_target: Option<ConnectionTarget>,
    pub(super) managed_profile: Option<ManagedProfileDir>,
    #[cfg(test)]
    pub(super) managed_browser_test_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

pub(super) struct BrowserRuntimeCandidate {
    browser: Arc<Browser>,
    page: Arc<Page>,
    is_external: bool,
    connection_target: Option<ConnectionTarget>,
    managed_profile: Option<ManagedProfileDir>,
    listener_generation: ListenerGeneration,
    identity_coverage: IdentityCoverageRegistry,
}

struct BrowserAuthorityInstallTransaction {
    previous_authority: Option<BrowserAuthoritySnapshot>,
    candidate: BrowserRuntimeCandidate,
}

impl BrowserAuthorityInstallTransaction {
    async fn begin(
        manager: &BrowserManager,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
        capture_previous_authority: bool,
    ) -> Self {
        let previous_authority = if capture_previous_authority {
            manager.snapshot_current_browser_authority().await
        } else {
            None
        };
        let candidate = manager
            .prepare_runtime_state_candidate(
                browser,
                page,
                is_external,
                connection_target,
                managed_profile,
            )
            .await;
        Self {
            previous_authority,
            candidate,
        }
    }

    async fn commit_candidate(&self, manager: &BrowserManager) -> Result<(), RubError> {
        manager
            .project_runtime_state_candidate(&self.candidate)
            .await;
        if let Err(error) = manager
            .reconcile_generation_bound_runtime(
                self.candidate.browser.clone(),
                self.candidate.listener_generation,
            )
            .await
        {
            return Err(manager
                .rollback_runtime_state_candidate(self.candidate.listener_generation, error)
                .await);
        }
        if let Err(error) = manager
            .commit_managed_profile_authority_candidate(&self.candidate)
            .await
        {
            return Err(manager
                .rollback_committed_runtime_state_candidate(&self.candidate, error)
                .await);
        }
        manager
            .commit_launch_policy_projection_candidate(&self.candidate)
            .await;
        Ok(())
    }

    async fn restore_previous_after_failed_commit(
        self,
        manager: &BrowserManager,
        install_error: RubError,
    ) -> RubError {
        let Some(previous_authority) = self.previous_authority else {
            return install_error;
        };
        match manager.restore_browser_authority(previous_authority).await {
            Ok(()) => install_error,
            Err(restore_error) => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to install replacement browser runtime state: {install_error}"),
                serde_json::json!({
                    "runtime_state_restore_succeeded": false,
                    "runtime_state_restore_error": restore_error.to_string(),
                }),
            ),
        }
    }

    async fn release_previous_after_commit(self, manager: &BrowserManager) -> Result<(), RubError> {
        if let Some(previous_authority) = self.previous_authority.clone()
            && let Err(release_error) = manager
                .release_browser_authority_snapshot(previous_authority.clone())
                .await
        {
            if previous_authority_release_error_is_post_shutdown_cleanup(&release_error) {
                return Err(previous_authority_cleanup_failed_after_release_error(
                    release_error,
                ));
            }
            return Err(self
                .rollback_after_failed_previous_release(manager, previous_authority, release_error)
                .await);
        }

        Ok(())
    }

    async fn rollback_after_failed_previous_release(
        self,
        manager: &BrowserManager,
        previous_authority: BrowserAuthoritySnapshot,
        release_error: RubError,
    ) -> RubError {
        if let Err(replacement_release_error) = manager
            .release_current_browser_authority_fail_closed()
            .await
        {
            return RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!(
                    "Replacement browser authority committed but releasing the previous authority failed: {release_error}"
                ),
                serde_json::json!({
                    "new_authority_committed": true,
                    "replacement_authority_released": false,
                    "replacement_authority_cleared": true,
                    "previous_authority_released": false,
                    "previous_authority_restored": false,
                    "rollback_release_error": replacement_release_error.to_string(),
                }),
            );
        }

        match manager.restore_browser_authority(previous_authority).await {
            Ok(()) => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!(
                    "Replacement browser authority rolled back because releasing the previous authority failed: {release_error}"
                ),
                serde_json::json!({
                    "new_authority_committed": false,
                    "replacement_authority_released": true,
                    "replacement_authority_cleared": true,
                    "previous_authority_released": false,
                    "previous_authority_restored": true,
                }),
            ),
            Err(restore_error) => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!(
                    "Replacement browser authority committed but releasing the previous authority failed: {release_error}"
                ),
                serde_json::json!({
                    "new_authority_committed": false,
                    "replacement_authority_released": true,
                    "replacement_authority_cleared": true,
                    "previous_authority_released": false,
                    "previous_authority_restored": false,
                    "runtime_state_restore_error": restore_error.to_string(),
                }),
            ),
        }
    }
}

fn previous_authority_release_error_is_post_shutdown_cleanup(error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(envelope)
            if envelope
                .context
                .as_ref()
                .and_then(|context| context.get("operation"))
                .and_then(|value| value.as_str())
                == Some(EPHEMERAL_PROFILE_REMOVE_AFTER_SHUTDOWN_OPERATION)
    )
}

fn previous_authority_cleanup_failed_after_release_error(release_error: RubError) -> RubError {
    let release_error_context = match &release_error {
        RubError::Domain(envelope) => envelope.context.clone(),
        _ => None,
    };
    RubError::domain_with_context(
        ErrorCode::BrowserLaunchFailed,
        format!(
            "Replacement browser authority committed, but previous authority cleanup failed after release: {release_error}"
        ),
        serde_json::json!({
            "reason": PREVIOUS_AUTHORITY_CLEANUP_FAILED_AFTER_RELEASE_REASON,
            "new_authority_committed": true,
            "new_authority_usable": true,
            "previous_authority_released": true,
            "previous_authority_restored": false,
            "cleanup_degraded": true,
            "release_error": release_error.to_string(),
            "release_error_context": release_error_context,
        }),
    )
}

fn release_previous_result_should_replay_runtime_projection(result: &Result<(), RubError>) -> bool {
    match result {
        Ok(()) => true,
        Err(RubError::Domain(envelope)) => {
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str())
                == Some(PREVIOUS_AUTHORITY_CLEANUP_FAILED_AFTER_RELEASE_REASON)
        }
        Err(_) => false,
    }
}

impl BrowserManager {
    pub(super) async fn restore_degraded_runtime_fallback_after_failed_authority_rebuild(
        &self,
        snapshot: BrowserRuntimeFallbackSnapshot,
    ) {
        let mut dialog_runtime = snapshot.dialog_runtime.clone();
        dialog_runtime.degraded_reason = append_runtime_degraded_reason(
            dialog_runtime.degraded_reason.take(),
            BROWSER_AUTHORITY_REBUILD_FAILED_REASON,
        );
        dialog_runtime.status = DialogRuntimeStatus::Degraded;
        *self.dialog_runtime.write().await = dialog_runtime;

        let mut download_runtime = snapshot.download_runtime.clone();
        download_runtime.mark_runtime_degraded(BROWSER_AUTHORITY_REBUILD_FAILED_REASON);
        self.download_runtime
            .write()
            .await
            .restore_snapshot(&download_runtime);

        let mut network_rule_runtime = snapshot.network_rule_runtime.clone();
        network_rule_runtime.clear_browser_installation_state();
        *self.network_rule_runtime.write().await = network_rule_runtime;

        let mut request_correlation = snapshot.request_correlation.clone();
        request_correlation
            .mark_runtime_degraded(CORRELATION_BROWSER_AUTHORITY_REBUILD_FAILED_REASON);
        *self.request_correlation.lock().await = request_correlation;

        *self.observatory_pending_registries.lock().await =
            snapshot.observatory_pending_registries.clone();
        self.replay_dialog_runtime_projection_to_callbacks().await;
        self.replay_download_runtime_projection_to_callbacks().await;
    }

    pub(super) async fn degraded_dialog_runtime_after_failed_authority_rebuild(
        &self,
    ) -> Option<rub_core::model::DialogRuntimeInfo> {
        let runtime = self.dialog_runtime.read().await.clone();
        let degraded_for_failed_rebuild =
            runtime.degraded_reason.as_deref().is_some_and(|reason| {
                reason
                    .split(',')
                    .any(|current| current.trim() == BROWSER_AUTHORITY_REBUILD_FAILED_REASON)
            });
        if runtime.status == DialogRuntimeStatus::Degraded && degraded_for_failed_rebuild {
            Some(runtime)
        } else {
            None
        }
    }

    #[cfg(test)]
    fn maybe_fail_generation_bound_runtime_reconcile_for_test(&self) -> Result<(), RubError> {
        if self
            .force_generation_bound_runtime_reconcile_failure
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                "forced generation-bound runtime reconcile failure",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn maybe_fail_previous_authority_release_for_test(&self) -> Result<(), RubError> {
        if self
            .force_previous_authority_release_failure
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                "forced previous authority release failure",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn maybe_fail_current_authority_release_for_test(&self) -> Result<(), RubError> {
        if self
            .force_current_authority_release_failure
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                "forced current authority release failure",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn maybe_fail_managed_profile_ownership_commit_for_test(&self) -> Result<(), RubError> {
        if self
            .force_managed_profile_ownership_commit_failure
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                "forced managed-profile ownership commit failure",
            ));
        }
        Ok(())
    }

    pub(super) async fn reconcile_generation_bound_runtime(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) -> Result<(), RubError> {
        #[cfg(test)]
        self.maybe_fail_generation_bound_runtime_reconcile_for_test()?;
        self.spawn_target_listeners(browser.clone(), listener_generation)
            .await;
        self.sync_tabs_projection().await?;
        self.publish_download_runtime_for_generation(browser, listener_generation)
            .await;
        Ok(())
    }

    pub(super) async fn reconcile_generation_bound_runtime_candidate(
        &self,
        browser: Arc<Browser>,
    ) -> Result<ListenerGeneration, RubError> {
        let listener_generation = self.bump_listener_generation();
        if let Err(error) = self
            .reconcile_generation_bound_runtime(browser, listener_generation)
            .await
        {
            self.retire_uncommitted_listener_generation(listener_generation);
            return Err(error);
        }
        Ok(listener_generation)
    }

    pub(super) async fn publish_download_runtime_for_generation(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) {
        let callbacks = crate::browser::runtime_callbacks::guard_download_callbacks_for_commit(
            self.download_callbacks.lock().await.clone(),
            self.authority_commit_in_progress.clone(),
            self.runtime_callback_reconfigure_in_progress.clone(),
        );
        let options = self.current_options();
        let is_external = *self.is_external.lock().await;
        let download_runtime = crate::downloads::install_browser_download_runtime(
            crate::downloads::DownloadRuntimeInstall {
                browser,
                projection_state: self.download_runtime.clone(),
                callbacks: callbacks.clone(),
                is_external,
                download_dir: options.download_dir,
                listener_generation,
                listener_generation_rx: self.listener_generation_receiver(),
                authority_release_in_progress: self.authority_release_in_progress.clone(),
            },
        )
        .await;
        crate::downloads::publish_download_runtime(
            &callbacks,
            listener_generation,
            download_runtime,
        );
    }

    pub(super) async fn replay_download_runtime_projection_to_callbacks(&self) {
        let callbacks = crate::browser::runtime_callbacks::guard_download_callbacks_for_commit(
            self.download_callbacks.lock().await.clone(),
            self.authority_commit_in_progress.clone(),
            self.runtime_callback_reconfigure_in_progress.clone(),
        );
        if callbacks.is_empty() {
            return;
        }
        let (generation, runtime) = self
            .download_runtime
            .read()
            .await
            .projection_with_generation();
        if generation == 0 {
            return;
        }
        crate::downloads::publish_download_runtime(&callbacks, generation, runtime);
    }

    pub(super) async fn replay_dialog_runtime_projection_to_callbacks(&self) {
        let callbacks = crate::browser::runtime_callbacks::guard_dialog_callbacks_for_commit(
            self.dialog_callbacks.lock().await.clone(),
            self.authority_commit_in_progress.clone(),
            self.runtime_callback_reconfigure_in_progress.clone(),
        );
        let Some(callback) = callbacks.on_runtime else {
            return;
        };
        callback(crate::dialogs::DialogRuntimeUpdate {
            generation: self.listener_generation(),
            runtime: self.dialog_runtime.read().await.clone(),
        });
    }

    pub(super) async fn replay_runtime_state_projection_to_callbacks(&self) {
        #[cfg(test)]
        self.runtime_state_replay_attempt_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let Some(browser) = self.browser.lock().await.clone() else {
            return;
        };
        let context = self
            .projection_context(browser, self.listener_generation())
            .await;
        crate::tab_projection::replay_runtime_state_for_committed_active_page(&context).await;
    }

    pub(super) async fn prepare_runtime_state_candidate(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
    ) -> BrowserRuntimeCandidate {
        let listener_generation = self.bump_listener_generation();
        let mut identity_coverage = IdentityCoverageRegistry::new(&self.current_identity_policy());
        identity_coverage.record_target(page.target_id().as_ref().to_string(), "page");
        BrowserRuntimeCandidate {
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
            listener_generation,
            identity_coverage,
        }
    }

    pub(super) async fn project_runtime_state_candidate(
        &self,
        candidate: &BrowserRuntimeCandidate,
    ) {
        self.reset_runtime_state_for_authority_install().await;
        *self.browser.lock().await = Some(candidate.browser.clone());
        *self.tab_projection.lock().await = CommittedTabProjection::single(candidate.page.clone());
        *self.managed_profile.lock().await = candidate.managed_profile.clone();
        *self.is_external.lock().await = candidate.is_external;
        *self.connection_target.lock().await = candidate.connection_target.clone();
        *self.identity_coverage.lock().await = candidate.identity_coverage.clone();
        self.spawn_epoch_listeners(candidate.page.clone(), candidate.listener_generation)
            .await;
        #[cfg(test)]
        self.maybe_pause_authority_commit_after_projection_for_test()
            .await;
    }

    async fn commit_launch_policy_projection_candidate(&self, candidate: &BrowserRuntimeCandidate) {
        self.update_connection_target_projection(candidate.connection_target.clone());
        self.update_stealth_coverage_projection(Some(candidate.identity_coverage.project()));
    }

    async fn commit_managed_profile_authority_candidate(
        &self,
        candidate: &BrowserRuntimeCandidate,
    ) -> Result<(), RubError> {
        let Some(profile) = candidate.managed_profile.as_ref() else {
            return Ok(());
        };
        #[cfg(test)]
        self.maybe_fail_managed_profile_ownership_commit_for_test()?;
        crate::managed_browser::commit_managed_profile_ownership(profile)
    }

    pub(super) async fn rollback_runtime_state_candidate(
        &self,
        listener_generation: ListenerGeneration,
        error: RubError,
    ) -> RubError {
        self.retire_uncommitted_listener_generation(listener_generation);
        let cleanup_error = self
            .release_current_browser_authority_fail_closed()
            .await
            .err();
        match cleanup_error {
            Some(cleanup_error) => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to install browser runtime state: {error}"),
                serde_json::json!({
                    "runtime_state_cleanup_succeeded": false,
                    "runtime_state_cleanup_error": cleanup_error.to_string(),
                    "runtime_state_authority_cleared": true,
                }),
            ),
            None => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to install browser runtime state: {error}"),
                serde_json::json!({
                    "runtime_state_cleanup_succeeded": true,
                    "runtime_state_authority_cleared": true,
                }),
            ),
        }
    }

    pub(super) async fn rollback_committed_runtime_state_candidate(
        &self,
        candidate: &BrowserRuntimeCandidate,
        error: RubError,
    ) -> RubError {
        let release_error = self
            .release_current_browser_authority_fail_closed()
            .await
            .err();
        RubError::domain_with_context(
            ErrorCode::BrowserLaunchFailed,
            format!(
                "Browser authority installed but managed profile ownership commit failed: {error}"
            ),
            serde_json::json!({
                "managed_profile_authority_cleared": true,
                "user_data_dir": candidate.managed_profile.as_ref().map(|profile| profile.path.display().to_string()),
                "managed_profile_ephemeral": candidate.managed_profile.as_ref().map(|profile| profile.ephemeral),
                "ownership_commit_release_error": release_error.map(|err| err.to_string()),
            }),
        )
    }

    pub(super) async fn projection_context(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) -> ProjectionContext {
        ProjectionContext {
            browser,
            page_hook_states: self.page_hook_states.clone(),
            tab_projection_store: self.tab_projection.clone(),
            local_active_target_authority: self.local_active_target_authority.clone(),
            epoch_callback: self.epoch_callback.clone(),
            observatory_callbacks: self.observatory_callbacks.clone(),
            runtime_state_callbacks: self.runtime_state_callbacks.clone(),
            dialog_callbacks: self.dialog_callbacks.clone(),
            dialog_runtime: self.dialog_runtime.clone(),
            dialog_intercept: self.dialog_intercept.clone(),
            network_rule_runtime: self.network_rule_runtime.clone(),
            request_correlation: self.request_correlation.clone(),
            observatory_pending_registries: self.observatory_pending_registries.clone(),
            identity_policy: self.current_identity_policy(),
            identity_coverage: self.identity_coverage.clone(),
            authority_commit_in_progress: self.authority_commit_in_progress.clone(),
            authority_release_in_progress: self.authority_release_in_progress.clone(),
            runtime_callback_reconfigure_in_progress: self
                .runtime_callback_reconfigure_in_progress
                .clone(),
            listener_generation,
            listener_generation_rx: self.listener_generation_receiver(),
            #[cfg(test)]
            force_required_page_hook_install_failure: self
                .force_required_page_hook_install_failure
                .clone(),
        }
    }

    pub(super) async fn clear_local_browser_authority(&self) {
        *self.browser.lock().await = None;
        *self.tab_projection.lock().await = CommittedTabProjection::empty();
        *self.managed_profile.lock().await = None;
        *self.local_active_target_authority.lock().await = None;
        self.reset_runtime_state_for_authority_install().await;
        self.reset_identity_coverage().await;
    }

    pub(super) async fn snapshot_current_browser_authority(
        &self,
    ) -> Option<BrowserAuthoritySnapshot> {
        let browser = self.browser.lock().await.clone()?;
        let page = self
            .snapshot_current_browser_authority_page(&browser)
            .await?;
        let is_external = *self.is_external.lock().await;
        let connection_target = self.connection_target.lock().await.clone();
        let managed_profile = self.managed_profile.lock().await.clone();
        let tab_projection = self.tab_projection.lock().await.clone();
        let local_active_target_authority = self.local_active_target_authority.lock().await.clone();
        let dialog_runtime = self.dialog_runtime.read().await.clone();
        let download_runtime = self.download_runtime.read().await.clone();
        let dialog_intercept = self.snapshot_dialog_intercept_state();
        let network_rule_runtime = self.network_rule_runtime.read().await.clone();
        let request_correlation = self.request_correlation.lock().await.clone();
        let observatory_pending_registries =
            self.observatory_pending_registries.lock().await.clone();
        let identity_coverage = self.identity_coverage.lock().await.clone();
        Some(BrowserAuthoritySnapshot {
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
            tab_projection,
            local_active_target_authority,
            dialog_runtime,
            download_runtime,
            dialog_intercept,
            network_rule_runtime,
            request_correlation,
            observatory_pending_registries,
            identity_coverage,
        })
    }

    pub(super) async fn snapshot_browser_runtime_fallback(&self) -> BrowserRuntimeFallbackSnapshot {
        BrowserRuntimeFallbackSnapshot {
            dialog_runtime: self.dialog_runtime.read().await.clone(),
            download_runtime: self.download_runtime.read().await.clone(),
            network_rule_runtime: self.network_rule_runtime.read().await.clone(),
            request_correlation: self.request_correlation.lock().await.clone(),
            observatory_pending_registries: self
                .observatory_pending_registries
                .lock()
                .await
                .clone(),
        }
    }

    #[cfg(test)]
    pub(super) async fn snapshot_current_browser_authority_target_id_for_test(
        &self,
    ) -> Option<String> {
        self.snapshot_current_browser_authority()
            .await
            .map(|snapshot| snapshot.page.target_id().as_ref().to_string())
    }

    async fn snapshot_current_browser_authority_page(
        &self,
        browser: &Arc<Browser>,
    ) -> Option<Arc<Page>> {
        let projection = self.tab_projection.lock().await.clone();
        let projected_page = projection
            .continuity_page
            .clone()
            .or(projection.current_page.clone())
            .or_else(|| {
                projection.active_target_id.as_ref().and_then(|target_id| {
                    projection
                        .pages
                        .iter()
                        .find(|page| page.target_id() == target_id)
                        .cloned()
                })
            })
            .or_else(|| projection.pages.first().cloned());
        if projected_page.is_some() {
            return projected_page;
        }
        browser.pages().await.ok()?.into_iter().next().map(Arc::new)
    }

    #[cfg(test)]
    fn managed_browser_test_semaphore() -> Arc<tokio::sync::Semaphore> {
        static MANAGED_BROWSER_TEST_SEMAPHORE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        MANAGED_BROWSER_TEST_SEMAPHORE
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    }

    #[cfg(test)]
    pub(super) async fn holds_managed_browser_test_permit_for_test(&self) -> bool {
        self.managed_browser_test_permit.lock().await.is_some()
    }

    #[cfg(test)]
    async fn acquire_managed_browser_test_permit_if_needed(
        &self,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        if self.managed_browser_test_permit.lock().await.is_some() {
            None
        } else {
            Some(
                Self::managed_browser_test_semaphore()
                    .acquire_owned()
                    .await
                    .expect("managed-browser test semaphore"),
            )
        }
    }

    #[cfg(test)]
    async fn install_managed_browser_test_permit(
        &self,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        if let Some(permit) = permit {
            *self.managed_browser_test_permit.lock().await = Some(permit);
        }
    }

    #[cfg(test)]
    async fn release_managed_browser_test_permit(&self) {
        self.managed_browser_test_permit.lock().await.take();
    }

    pub(super) async fn resolve_browser_authority_install(
        &self,
        existing_target: Option<ConnectionTarget>,
        managed_reconnect_url: Option<String>,
    ) -> Result<BrowserAuthorityInstall, RubError> {
        match existing_target.clone() {
            Some(ConnectionTarget::CdpUrl { url }) => {
                let (browser, page, _) = crate::runtime::attach_external_browser(&url).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: true,
                    connection_target: Some(ConnectionTarget::CdpUrl { url }),
                    managed_profile: None,
                    #[cfg(test)]
                    managed_browser_test_permit: None,
                })
            }
            Some(ConnectionTarget::AutoDiscovered { url, port }) => {
                let (browser, page, _) = crate::runtime::attach_external_browser(&url).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: true,
                    connection_target: Some(ConnectionTarget::AutoDiscovered { url, port }),
                    managed_profile: None,
                    #[cfg(test)]
                    managed_browser_test_permit: None,
                })
            }
            _ => {
                let options = self.current_options();
                let identity_policy = self.current_identity_policy();
                let managed_profile = resolve_managed_profile_dir(
                    options.user_data_dir.clone(),
                    options.profile_directory.clone(),
                    options.managed_profile_ephemeral,
                );
                if let Some(url) = managed_reconnect_url {
                    match crate::runtime::attach_external_browser(&url).await {
                        Ok((browser, page, _)) => {
                            return Ok(BrowserAuthorityInstall {
                                browser,
                                page,
                                is_external: false,
                                connection_target: existing_target,
                                managed_profile: Some(managed_profile),
                                #[cfg(test)]
                                managed_browser_test_permit: None,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(
                                url = %url,
                                error = %error,
                                "Managed browser authority reconnect failed; falling back to relaunch"
                            );
                        }
                    }
                }
                #[cfg(test)]
                let managed_browser_test_permit =
                    self.acquire_managed_browser_test_permit_if_needed().await;
                let (browser, page) =
                    crate::runtime::launch_managed_browser(&options, &identity_policy).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: false,
                    connection_target: existing_target,
                    managed_profile: Some(managed_profile),
                    #[cfg(test)]
                    managed_browser_test_permit,
                })
            }
        }
    }

    pub(super) async fn restore_browser_authority(
        &self,
        snapshot: BrowserAuthoritySnapshot,
    ) -> Result<(), RubError> {
        self.install_runtime_state_locked_without_callback_replay(
            snapshot.browser.clone(),
            snapshot.page.clone(),
            snapshot.is_external,
            snapshot.connection_target.clone(),
            snapshot.managed_profile.clone(),
            #[cfg(test)]
            None,
        )
        .await?;
        self.restore_browser_authority_runtime_state(&snapshot)
            .await;
        self.set_authority_commit_in_progress(false);
        self.replay_runtime_state_projection_to_callbacks().await;
        self.replay_dialog_runtime_projection_to_callbacks().await;
        self.replay_download_runtime_projection_to_callbacks().await;
        Ok(())
    }

    async fn restore_browser_authority_runtime_state(&self, snapshot: &BrowserAuthoritySnapshot) {
        *self.dialog_runtime.write().await = snapshot.dialog_runtime.clone();
        self.download_runtime
            .write()
            .await
            .restore_snapshot(&snapshot.download_runtime);
        self.restore_dialog_intercept_state(snapshot.dialog_intercept.clone());
        *self.network_rule_runtime.write().await = snapshot.network_rule_runtime.clone();
        *self.request_correlation.lock().await = snapshot.request_correlation.clone();
        *self.observatory_pending_registries.lock().await =
            snapshot.observatory_pending_registries.clone();
        *self.local_active_target_authority.lock().await =
            snapshot.local_active_target_authority.clone();
        let mut restored_tab_projection = snapshot.tab_projection.clone();
        let mut tab_projection = self.tab_projection.lock().await;
        if restored_tab_projection.current_page.is_none() {
            restored_tab_projection.current_page = tab_projection.current_page.clone();
        }
        if restored_tab_projection.continuity_page.is_none() {
            restored_tab_projection.continuity_page = tab_projection.continuity_page.clone();
        }
        *tab_projection = restored_tab_projection;
        *self.identity_coverage.lock().await = snapshot.identity_coverage.clone();
        self.update_stealth_coverage_projection(Some(snapshot.identity_coverage.project()));
    }

    pub(super) async fn release_browser_authority_snapshot(
        &self,
        snapshot: BrowserAuthoritySnapshot,
    ) -> Result<(), RubError> {
        #[cfg(test)]
        self.maybe_fail_previous_authority_release_for_test()?;

        if snapshot.is_external {
            drop(snapshot.browser);
            info!("Disconnected from previous external browser authority");
            return Ok(());
        }

        let profile = snapshot.managed_profile.clone().unwrap_or_else(|| {
            let options = self.current_options();
            resolve_managed_profile_dir(
                options.user_data_dir,
                options.profile_directory,
                options.managed_profile_ephemeral,
            )
        });
        shutdown_managed_browser(snapshot.browser.as_ref(), &profile).await?;
        info!(
            user_data_dir = %profile.path.display(),
            "Released previous managed browser authority after replacement commit"
        );
        Ok(())
    }

    pub(super) async fn release_current_browser_authority(&self) -> Result<(), RubError> {
        #[cfg(test)]
        self.maybe_fail_current_authority_release_for_test()?;

        let is_external = *self.is_external.lock().await;
        let options = self.current_options();
        let browser = self.browser.lock().await.clone();

        if let Some(browser) = browser {
            if is_external {
                self.clear_local_browser_authority().await;
                drop(browser);
                info!("Disconnected from external browser (browser still running)");
            } else {
                let profile = self
                    .managed_profile
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| {
                        resolve_managed_profile_dir(
                            options.user_data_dir.clone(),
                            options.profile_directory.clone(),
                            options.managed_profile_ephemeral,
                        )
                    });
                shutdown_managed_browser(browser.as_ref(), &profile).await?;
                self.clear_local_browser_authority().await;
                drop(browser);
                info!(user_data_dir = %profile.path.display(), "Managed browser closed");
            }
        } else {
            self.clear_local_browser_authority().await;
        }

        #[cfg(test)]
        if !is_external {
            self.release_managed_browser_test_permit().await;
        }

        Ok(())
    }

    pub(super) async fn release_current_browser_authority_with_callback_fence(
        &self,
    ) -> Result<(), RubError> {
        self.set_authority_commit_in_progress(true);
        self.authority_release_in_progress
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let result = self.release_current_browser_authority().await;
        self.bump_listener_generation();
        self.authority_release_in_progress
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.set_authority_commit_in_progress(false);
        result
    }

    pub(super) async fn release_current_browser_authority_fail_closed(
        &self,
    ) -> Result<(), RubError> {
        let release_result = self
            .release_current_browser_authority_with_callback_fence()
            .await;
        if release_result.is_err() {
            self.clear_local_browser_authority().await;
        }
        release_result
    }

    fn snapshot_dialog_intercept_state(&self) -> Option<rub_core::model::DialogInterceptPolicy> {
        match self.dialog_intercept.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                let snapshot = guard.clone();
                drop(guard);
                self.dialog_intercept.clear_poison();
                tracing::warn!(
                    "Recovered poisoned dialog intercept state while snapshotting browser authority"
                );
                snapshot
            }
        }
    }

    fn restore_dialog_intercept_state(
        &self,
        policy: Option<rub_core::model::DialogInterceptPolicy>,
    ) {
        match self.dialog_intercept.lock() {
            Ok(mut guard) => {
                *guard = policy;
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = policy;
                self.dialog_intercept.clear_poison();
                tracing::warn!(
                    "Recovered poisoned dialog intercept state while restoring browser authority"
                );
            }
        }
    }

    async fn reset_runtime_state_for_authority_install(&self) {
        self.page_hook_states.lock().await.clear();
        self.observatory_pending_registries.lock().await.clear();
        self.download_runtime.write().await.clear();
        self.network_rule_runtime
            .write()
            .await
            .clear_browser_installation_state();
        *self.request_correlation.lock().await = RequestCorrelationRegistry::default();
        *self.local_active_target_authority.lock().await = None;
        *self.dialog_runtime.write().await = Default::default();
        self.recover_dialog_intercept_after_authority_reset();
    }

    pub(super) async fn install_runtime_state_locked(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
        #[cfg(test)] managed_browser_test_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), RubError> {
        self.set_authority_commit_in_progress(true);
        let result = BrowserAuthorityInstallTransaction::begin(
            self,
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
            false,
        )
        .await
        .commit_candidate(self)
        .await;
        self.set_authority_commit_in_progress(false);
        if result.is_ok() {
            self.replay_runtime_state_projection_to_callbacks().await;
            self.replay_dialog_runtime_projection_to_callbacks().await;
            self.replay_download_runtime_projection_to_callbacks().await;
        }
        #[cfg(test)]
        if result.is_ok() {
            self.install_managed_browser_test_permit(managed_browser_test_permit)
                .await;
        }
        result
    }

    async fn install_runtime_state_locked_without_callback_replay(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
        #[cfg(test)] managed_browser_test_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), RubError> {
        self.set_authority_commit_in_progress(true);
        let result = BrowserAuthorityInstallTransaction::begin(
            self,
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
            false,
        )
        .await
        .commit_candidate(self)
        .await;
        if result.is_err() {
            self.set_authority_commit_in_progress(false);
            return result;
        }
        #[cfg(test)]
        self.install_managed_browser_test_permit(managed_browser_test_permit)
            .await;
        result
    }

    #[cfg(test)]
    pub(super) async fn replace_browser_authority(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
    ) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.replace_browser_authority_locked(
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
        )
        .await
    }

    pub(super) async fn replace_browser_authority_locked(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
        managed_profile: Option<ManagedProfileDir>,
    ) -> Result<(), RubError> {
        self.set_authority_commit_in_progress(true);
        let transaction = BrowserAuthorityInstallTransaction::begin(
            self,
            browser,
            page,
            is_external,
            connection_target,
            managed_profile,
            true,
        )
        .await;
        if let Err(error) = transaction.commit_candidate(self).await {
            let restore_error = transaction
                .restore_previous_after_failed_commit(self, error)
                .await;
            self.set_authority_commit_in_progress(false);
            return Err(restore_error);
        }
        let result = transaction.release_previous_after_commit(self).await;
        self.set_authority_commit_in_progress(false);
        if release_previous_result_should_replay_runtime_projection(&result) {
            self.replay_runtime_state_projection_to_callbacks().await;
            self.replay_dialog_runtime_projection_to_callbacks().await;
            self.replay_download_runtime_projection_to_callbacks().await;
        }
        result
    }

    #[cfg(test)]
    pub(super) fn force_previous_authority_release_failure(&self) {
        self.force_previous_authority_release_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn force_current_authority_release_failure(&self) {
        self.force_current_authority_release_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn force_generation_bound_runtime_reconcile_failure(&self) {
        self.force_generation_bound_runtime_reconcile_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn force_managed_profile_ownership_commit_failure(&self) {
        self.force_managed_profile_ownership_commit_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn set_force_required_page_hook_install_failure(&self, enabled: bool) {
        self.force_required_page_hook_install_failure
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    async fn maybe_pause_authority_commit_after_projection_for_test(&self) {
        if !self
            .pause_authority_commit_after_projection
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        self.authority_commit_projected.notify_waiters();
        self.resume_authority_commit.notified().await;
    }

    #[cfg(test)]
    pub(super) fn pause_authority_commit_after_projection(&self) {
        self.pause_authority_commit_after_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) async fn wait_for_authority_commit_projection_pause(&self) {
        self.authority_commit_projected.notified().await;
    }

    #[cfg(test)]
    pub(super) fn resume_paused_authority_commit(&self) {
        self.pause_authority_commit_after_projection
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.resume_authority_commit.notify_waiters();
    }
}

fn append_runtime_degraded_reason(existing: Option<String>, reason: &str) -> Option<String> {
    match existing {
        None => Some(reason.to_string()),
        Some(existing) if existing.split(',').any(|current| current.trim() == reason) => {
            Some(existing)
        }
        Some(existing) => Some(format!("{existing},{reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_shutdown_cleanup_failure_keeps_replacement_authority_committed() {
        let release_error = RubError::domain_with_context(
            ErrorCode::BrowserLaunchFailed,
            "cleanup failed after previous authority release",
            serde_json::json!({
                "operation": EPHEMERAL_PROFILE_REMOVE_AFTER_SHUTDOWN_OPERATION,
                "user_data_dir": "/tmp/rub-profile",
            }),
        );

        assert!(previous_authority_release_error_is_post_shutdown_cleanup(
            &release_error
        ));
        let error = previous_authority_cleanup_failed_after_release_error(release_error);
        let replay_result = Err(error);
        assert!(release_previous_result_should_replay_runtime_projection(
            &replay_result
        ));
        let envelope = match replay_result {
            Ok(()) => panic!("expected cleanup-degraded error"),
            Err(error) => error.into_envelope(),
        };
        let context = envelope.context.expect("cleanup-degraded context");
        assert_eq!(
            context["reason"],
            serde_json::json!("previous_authority_cleanup_failed_after_release")
        );
        assert_eq!(context["new_authority_committed"], serde_json::json!(true));
        assert_eq!(context["new_authority_usable"], serde_json::json!(true));
        assert_eq!(
            context["previous_authority_released"],
            serde_json::json!(true)
        );
        assert_eq!(
            context["previous_authority_restored"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn pre_release_failure_still_requires_rollback_path() {
        let release_error = RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            "previous authority did not release",
        );

        assert!(!previous_authority_release_error_is_post_shutdown_cleanup(
            &release_error
        ));
        assert!(!release_previous_result_should_replay_runtime_projection(
            &Err(release_error)
        ));
    }
}
