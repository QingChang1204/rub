use super::*;

#[derive(Clone)]
pub(super) struct BrowserAuthoritySnapshot {
    browser: Arc<Browser>,
    page: Arc<Page>,
    is_external: bool,
    connection_target: Option<ConnectionTarget>,
}

pub(super) struct BrowserAuthorityInstall {
    pub(super) browser: Arc<Browser>,
    pub(super) page: Arc<Page>,
    pub(super) is_external: bool,
    pub(super) connection_target: Option<ConnectionTarget>,
}

pub(super) struct BrowserRuntimeCandidate {
    browser: Arc<Browser>,
    page: Arc<Page>,
    is_external: bool,
    connection_target: Option<ConnectionTarget>,
    listener_generation: ListenerGeneration,
    projected_coverage: StealthCoverageInfo,
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
        capture_previous_authority: bool,
    ) -> Self {
        let previous_authority = if capture_previous_authority {
            manager.snapshot_current_browser_authority().await
        } else {
            None
        };
        let candidate = manager
            .prepare_runtime_state_candidate(browser, page, is_external, connection_target)
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
        if let Some(previous_authority) = self.previous_authority {
            manager
                .release_browser_authority_snapshot(previous_authority)
                .await
                .map_err(|error| {
                    RubError::domain_with_context(
                        ErrorCode::BrowserLaunchFailed,
                        format!(
                            "Replacement browser authority committed but release of the previous authority failed: {error}"
                        ),
                        serde_json::json!({
                            "new_authority_committed": true,
                            "previous_authority_released": false,
                        }),
                    )
                })?;
        }

        Ok(())
    }
}

impl BrowserManager {
    pub(super) async fn reconcile_generation_bound_runtime(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) -> Result<(), RubError> {
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
    ) -> BrowserRuntimeCandidate {
        let listener_generation = self.bump_listener_generation();
        let projected_coverage = {
            let mut coverage = self.identity_coverage.lock().await;
            coverage.record_target(page.target_id().as_ref().to_string(), "page");
            coverage.project()
        };
        BrowserRuntimeCandidate {
            browser,
            page,
            is_external,
            connection_target,
            listener_generation,
            projected_coverage,
        }
    }

    pub(super) async fn project_runtime_state_candidate(
        &self,
        candidate: &BrowserRuntimeCandidate,
    ) {
        *self.browser.lock().await = Some(candidate.browser.clone());
        *self.page.lock().await = Some(candidate.page.clone());
        *self.pages.lock().await = vec![candidate.page.clone()];
        *self.active_target_id.lock().await = Some(candidate.page.target_id().clone());
        *self.is_external.lock().await = candidate.is_external;
        *self.connection_target.lock().await = candidate.connection_target.clone();
        self.update_connection_target_projection(candidate.connection_target.clone());
        self.update_stealth_coverage_projection(Some(candidate.projected_coverage.clone()));
        self.spawn_epoch_listeners(candidate.page.clone(), candidate.listener_generation)
            .await;
    }

    pub(super) async fn rollback_runtime_state_candidate(
        &self,
        listener_generation: ListenerGeneration,
        error: RubError,
    ) -> RubError {
        self.retire_uncommitted_listener_generation(listener_generation);
        let cleanup_error = self.release_current_browser_authority().await.err();
        match cleanup_error {
            Some(cleanup_error) => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to install browser runtime state: {error}"),
                serde_json::json!({
                    "runtime_state_cleanup_succeeded": false,
                    "runtime_state_cleanup_error": cleanup_error.to_string(),
                }),
            ),
            None => RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to install browser runtime state: {error}"),
                serde_json::json!({
                    "runtime_state_cleanup_succeeded": true,
                }),
            ),
        }
    }

    pub(super) async fn projection_context(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) -> ProjectionContext {
        ProjectionContext {
            browser,
            page_hook_states: self.page_hook_states.clone(),
            active_target_store: self.active_target_id.clone(),
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
            listener_generation,
            listener_generation_rx: self.listener_generation_receiver(),
        }
    }

    pub(super) async fn clear_local_browser_authority(&self) {
        *self.browser.lock().await = None;
        *self.page.lock().await = None;
        self.pages.lock().await.clear();
        *self.active_target_id.lock().await = None;
        self.page_hook_states.lock().await.clear();
        self.reset_identity_coverage().await;
        self.network_rule_runtime
            .write()
            .await
            .clear_browser_installation_state();
        *self.request_correlation.lock().await = RequestCorrelationRegistry::default();
        *self.dialog_runtime.write().await = Default::default();
        self.recover_dialog_intercept_after_authority_reset();
    }

    pub(super) async fn snapshot_current_browser_authority(
        &self,
    ) -> Option<BrowserAuthoritySnapshot> {
        let browser = self.browser.lock().await.clone()?;
        let page = self.page.lock().await.clone()?;
        let is_external = *self.is_external.lock().await;
        let connection_target = self.connection_target.lock().await.clone();
        Some(BrowserAuthoritySnapshot {
            browser,
            page,
            is_external,
            connection_target,
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
                })
            }
            Some(ConnectionTarget::AutoDiscovered { url, port }) => {
                let (browser, page, _) = crate::runtime::attach_external_browser(&url).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: true,
                    connection_target: Some(ConnectionTarget::AutoDiscovered { url, port }),
                })
            }
            _ => {
                let options = self.current_options();
                let identity_policy = self.current_identity_policy();
                let (browser, page) =
                    crate::runtime::launch_managed_browser(&options, &identity_policy).await?;
                Ok(BrowserAuthorityInstall {
                    browser,
                    page,
                    is_external: false,
                    connection_target: existing_target,
                })
            }
        }
    }

    pub(super) async fn restore_browser_authority(
        &self,
        snapshot: BrowserAuthoritySnapshot,
    ) -> Result<(), RubError> {
        self.install_runtime_state(
            snapshot.browser,
            snapshot.page,
            snapshot.is_external,
            snapshot.connection_target,
        )
        .await
    }

    pub(super) async fn release_browser_authority_snapshot(
        &self,
        snapshot: BrowserAuthoritySnapshot,
    ) -> Result<(), RubError> {
        if snapshot.is_external {
            drop(snapshot.browser);
            info!("Disconnected from previous external browser authority");
            return Ok(());
        }

        let options = self.current_options();
        let profile = resolve_managed_profile_dir(options.user_data_dir.clone());
        shutdown_managed_browser(snapshot.browser.as_ref(), &profile).await?;
        info!(
            user_data_dir = %profile.path.display(),
            "Released previous managed browser authority after replacement commit"
        );
        Ok(())
    }

    pub(super) async fn release_current_browser_authority(&self) -> Result<(), RubError> {
        let is_external = *self.is_external.lock().await;
        let options = self.current_options();
        let browser = self.browser.lock().await.clone();

        if let Some(browser) = browser {
            if is_external {
                self.clear_local_browser_authority().await;
                drop(browser);
                info!("Disconnected from external browser (browser still running)");
            } else {
                let profile = resolve_managed_profile_dir(options.user_data_dir.clone());
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

    pub(super) async fn install_runtime_state(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
    ) -> Result<(), RubError> {
        BrowserAuthorityInstallTransaction::begin(
            self,
            browser,
            page,
            is_external,
            connection_target,
            false,
        )
        .await
        .commit_candidate(self)
        .await
    }

    pub(super) async fn replace_browser_authority(
        &self,
        browser: Arc<Browser>,
        page: Arc<Page>,
        is_external: bool,
        connection_target: Option<ConnectionTarget>,
    ) -> Result<(), RubError> {
        let transaction = BrowserAuthorityInstallTransaction::begin(
            self,
            browser,
            page,
            is_external,
            connection_target,
            true,
        )
        .await;
        if let Err(error) = transaction.commit_candidate(self).await {
            return Err(transaction
                .restore_previous_after_failed_commit(self, error)
                .await);
        }
        transaction.release_previous_after_commit(self).await
    }
}
