use super::*;
use crate::identity_coverage::IdentityCoverageRegistry;
use crate::tab_projection::{CommittedTabProjection, LocalActiveTargetAuthority};

#[derive(Clone)]
pub(super) struct BrowserAuthoritySnapshot {
    browser: Arc<Browser>,
    page: Arc<Page>,
    is_external: bool,
    connection_target: Option<ConnectionTarget>,
    managed_profile: Option<ManagedProfileDir>,
    local_active_target_authority: Option<LocalActiveTargetAuthority>,
    dialog_runtime: rub_core::model::DialogRuntimeInfo,
    dialog_intercept: Option<rub_core::model::DialogInterceptPolicy>,
    network_rule_runtime: NetworkRuleRuntime,
    request_correlation: RequestCorrelationRegistry,
    observatory_pending_registries:
        HashMap<String, crate::runtime_observatory::SharedPendingRequestRegistry>,
    identity_coverage: IdentityCoverageRegistry,
}

pub(super) struct BrowserAuthorityInstall {
    pub(super) browser: Arc<Browser>,
    pub(super) page: Arc<Page>,
    pub(super) is_external: bool,
    pub(super) connection_target: Option<ConnectionTarget>,
    pub(super) managed_profile: Option<ManagedProfileDir>,
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

impl BrowserManager {
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
        let callbacks = self.download_callbacks.lock().await.clone();
        let options = self.current_options();
        let is_external = *self.is_external.lock().await;
        let download_runtime = crate::downloads::install_browser_download_runtime(
            browser,
            callbacks.clone(),
            is_external,
            options.download_dir,
            listener_generation,
            self.listener_generation_receiver(),
        )
        .await;
        crate::downloads::publish_download_runtime(
            &callbacks,
            listener_generation,
            download_runtime,
        );
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
            listener_generation,
            listener_generation_rx: self.listener_generation_receiver(),
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
        let page = self.projected_continuity_page().await?;
        let is_external = *self.is_external.lock().await;
        let connection_target = self.connection_target.lock().await.clone();
        let managed_profile = self.managed_profile.lock().await.clone();
        let local_active_target_authority = self.local_active_target_authority.lock().await.clone();
        let dialog_runtime = self.dialog_runtime.read().await.clone();
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
            local_active_target_authority,
            dialog_runtime,
            dialog_intercept,
            network_rule_runtime,
            request_correlation,
            observatory_pending_registries,
            identity_coverage,
        })
    }

    pub(super) async fn resolve_browser_authority_install(
        &self,
        existing_target: Option<ConnectionTarget>,
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
                })
            }
            _ => {
                let options = self.current_options();
                let identity_policy = self.current_identity_policy();
                let managed_profile = resolve_managed_profile_dir(
                    options.user_data_dir.clone(),
                    options.managed_profile_ephemeral,
                );
                let (browser, page) =
                    crate::runtime::launch_managed_browser(&options, &identity_policy).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: false,
                    connection_target: existing_target,
                    managed_profile: Some(managed_profile),
                })
            }
        }
    }

    pub(super) async fn restore_browser_authority(
        &self,
        snapshot: BrowserAuthoritySnapshot,
    ) -> Result<(), RubError> {
        self.install_runtime_state_locked(
            snapshot.browser.clone(),
            snapshot.page.clone(),
            snapshot.is_external,
            snapshot.connection_target.clone(),
            snapshot.managed_profile.clone(),
        )
        .await?;
        self.restore_browser_authority_runtime_state(&snapshot)
            .await;
        Ok(())
    }

    async fn restore_browser_authority_runtime_state(&self, snapshot: &BrowserAuthoritySnapshot) {
        *self.dialog_runtime.write().await = snapshot.dialog_runtime.clone();
        self.restore_dialog_intercept_state(snapshot.dialog_intercept.clone());
        *self.network_rule_runtime.write().await = snapshot.network_rule_runtime.clone();
        *self.request_correlation.lock().await = snapshot.request_correlation.clone();
        *self.observatory_pending_registries.lock().await =
            snapshot.observatory_pending_registries.clone();
        *self.local_active_target_authority.lock().await =
            snapshot.local_active_target_authority.clone();
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
            resolve_managed_profile_dir(options.user_data_dir, options.managed_profile_ephemeral)
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

        Ok(())
    }

    pub(super) async fn release_current_browser_authority_fail_closed(
        &self,
    ) -> Result<(), RubError> {
        let release_result = self.release_current_browser_authority().await;
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
