use super::*;

pub(super) fn resolve_close_tab_index(
    requested_index: Option<u32>,
    page_target_ids: &[String],
    active_target_id: Option<&str>,
) -> Result<usize, RubError> {
    if let Some(index) = requested_index {
        return Ok(index as usize);
    }
    if page_target_ids.len() <= 1 {
        return Ok(0);
    }
    let Some(active_target_id) = active_target_id else {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Cannot close the current tab because active tab authority is unavailable",
            serde_json::json!({
                "reason": "active_tab_authority_missing",
                "tab_count": page_target_ids.len(),
            }),
        ));
    };
    page_target_ids
        .iter()
        .position(|target| target == active_target_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "Cannot close the current tab because active tab authority is not present in the committed tab projection",
                serde_json::json!({
                    "reason": "active_tab_authority_missing",
                    "tab_count": page_target_ids.len(),
                    "active_target_id": active_target_id,
                }),
            )
        })
}

impl BrowserManager {
    async fn commit_local_active_page_authority(&self, page: Arc<Page>) {
        let target_id = page.target_id().clone();
        *self.local_active_target_authority.lock().await = Some(
            crate::tab_projection::LocalActiveTargetAuthority::new(target_id.clone()),
        );
        let mut projection = self.tab_projection.lock().await;
        *projection = projection.clone().with_local_active_page(page);
    }

    /// Handle a pending dialog on the page authority that actually surfaced it.
    ///
    /// Dialog runtime is session-scoped for projection convenience, but dialog
    /// actuation must still bind back to the originating page target. Falling
    /// back to the current active page would allow a background-tab dialog to
    /// be accepted or dismissed against the wrong page authority.
    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), RubError> {
        let pending_dialog =
            pending_dialog_authority(crate::dialogs::pending_dialog(&self.dialog_runtime()).await)?;
        let target_id = pending_dialog_target_id(&pending_dialog)?;
        let page = match self.page_for_target_id(&target_id).await {
            Ok(page) => page,
            Err(error) => {
                let code = match &error {
                    RubError::Domain(envelope) => envelope.code,
                    RubError::Io(_) => ErrorCode::IoError,
                    RubError::Json(_) => ErrorCode::JsonError,
                    RubError::Internal(_) => ErrorCode::InternalError,
                };
                if code == ErrorCode::TabNotFound {
                    return Err(stale_pending_dialog_target_error(&target_id, &error));
                }
                return Err(error);
            }
        };
        crate::dialogs::handle_dialog(&page, accept, prompt_text).await
    }

    /// Get one live page handle by stable target identity without mutating the
    /// active-tab authority.
    pub async fn page_for_target_id(&self, target_id: &str) -> Result<Arc<Page>, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.browser_handle()
            .await?
            .pages()
            .await
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserCrashed,
                    format!("Failed to enumerate browser tabs: {e}"),
                )
            })?
            .iter()
            .find(|page| page.target_id().as_ref() == target_id)
            .map(|page| Arc::new(page.clone()))
            .ok_or_else(|| {
                RubError::domain(
                    ErrorCode::TabNotFound,
                    format!("Tab target '{target_id}' is not present in the current session"),
                )
            })
    }

    /// Get all pages as TabInfo list.
    pub async fn tab_list(&self) -> Result<Vec<TabInfo>, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.sync_tabs_projection().await?;

        let projection = self.tab_projection.lock().await.clone();
        self.tab_list_from_projection(&projection).await
    }

    /// Switch to a tab by index and mark it as the active tab.
    pub async fn switch_to_tab(&self, index: u32) -> Result<TabInfo, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.sync_tabs_projection().await?;

        let pages = self.tab_projection.lock().await.pages.clone();
        let idx = index as usize;
        if idx >= pages.len() {
            return Err(crate::tab_projection::tab_not_found(index, pages.len()));
        }

        let target_page = pages[idx].clone();
        target_page.activate().await.map_err(|e| {
            RubError::Internal(format!("ActivateTarget failed for tab {index}: {e}"))
        })?;

        self.commit_local_active_page_authority(target_page.clone())
            .await;
        self.sync_tabs_projection().await?;

        let active_target_id = self.tab_projection.lock().await.active_target_id.clone();
        Ok(crate::tab_projection::tab_info_for_page(
            index,
            &target_page,
            active_target_id.as_ref(),
            self.tab_projection.lock().await.active_target_authority,
        )
        .await)
    }

    /// Close a tab by index. If it is the last tab, create `about:blank` first.
    pub async fn close_tab_at(&self, index: Option<u32>) -> Result<Vec<TabInfo>, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.sync_tabs_projection().await?;

        let projection_before = self.tab_projection.lock().await.clone();
        let pages_before = projection_before.pages;
        let active_before = projection_before.active_target_id;
        let page_target_ids = pages_before
            .iter()
            .map(|page| page.target_id().as_ref().to_string())
            .collect::<Vec<_>>();
        let idx = resolve_close_tab_index(
            index,
            &page_target_ids,
            active_before.as_ref().map(|target| target.as_ref()),
        )?;
        if idx >= pages_before.len() {
            return Err(crate::tab_projection::tab_not_found(
                idx as u32,
                pages_before.len(),
            ));
        }

        let target_page = pages_before[idx].clone();
        let closing_active = active_before
            .as_ref()
            .map(|target| target == target_page.target_id())
            .unwrap_or(false);
        if pages_before.len() == 1 {
            target_page.goto("about:blank").await.map_err(|e| {
                RubError::Internal(format!("Failed to reset last tab to about:blank: {e}"))
            })?;
            self.commit_local_active_page_authority(target_page).await;
            let projection = self.tab_projection.lock().await.clone();
            return self.tab_list_from_projection(&projection).await;
        }

        target_page
            .execute(CloseTargetParams::new(target_page.target_id().clone()))
            .await
            .map_err(|e| RubError::Internal(format!("CloseTarget failed: {e}")))?;

        self.sync_tabs_projection().await?;

        let pages_after = self.tab_projection.lock().await.pages.clone();
        if let Some(active_page) = self
            .select_active_page_after_close(
                &pages_after,
                active_before.as_ref(),
                closing_active,
                idx,
            )
            .await?
            && active_page.activate().await.is_ok()
        {
            self.commit_local_active_page_authority(active_page).await;
        }

        let projection = self.tab_projection.lock().await.clone();
        self.tab_list_from_projection(&projection).await
    }

    /// CDP health check: Browser.getVersion().
    pub async fn health_check(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        let browser = self.browser_handle().await?;
        browser
            .execute(chromiumoxide::cdp::browser_protocol::browser::GetVersionParams::default())
            .await
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserCrashed,
                    format!("Health check failed: {e}"),
                )
            })?;
        Ok(())
    }

    /// Recover from a browser crash by clearing local projections and relaunching.
    pub async fn recover_browser(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        tracing::warn!("Browser crash detected, auto-restarting");
        self.bump_listener_generation();
        self.clear_local_browser_authority().await;
        self.ensure_browser_locked().await
    }

    /// Close the browser.
    /// If external, only disconnects the CDP session (browser keeps running).
    pub async fn close(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.release_current_browser_authority_fail_closed().await?;
        Ok(())
    }

    /// Relaunch a managed headless browser as a visible managed browser while
    /// keeping the same session/profile authority.
    pub async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        if *self.is_external.lock().await {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Takeover elevation is only supported for managed browser sessions",
            ));
        }

        let previous_options = self.current_options();
        if !previous_options.headless {
            return Ok(self.launch_policy_info());
        }

        let restore_url = self.current_restore_url().await;
        self.release_current_browser_authority_fail_closed().await?;

        self.set_current_headless(false);
        self.reset_identity_coverage().await;

        if let Err(error) = self
            .relaunch_and_restore_visible_locked(restore_url.as_deref())
            .await
        {
            let rollback_cleanup = self.release_current_browser_authority_fail_closed().await;
            self.set_current_headless(previous_options.headless);
            self.reset_identity_coverage().await;
            let restored = self
                .relaunch_and_restore_visible_locked(restore_url.as_deref())
                .await;
            return Err(match restored {
                Ok(()) => RubError::domain_with_context(
                    ErrorCode::BrowserLaunchFailed,
                    format!("Failed to elevate session to a visible browser: {error}"),
                    serde_json::json!({
                        "restore_succeeded": true,
                        "rollback_cleanup_succeeded": rollback_cleanup.is_ok(),
                        "target_visibility": "headed",
                    }),
                ),
                Err(restore_error) => RubError::domain_with_context(
                    ErrorCode::BrowserLaunchFailed,
                    format!("Failed to elevate session to a visible browser: {error}"),
                    serde_json::json!({
                        "restore_succeeded": false,
                        "rollback_cleanup_succeeded": rollback_cleanup.is_ok(),
                        "rollback_cleanup_error": rollback_cleanup.err().map(|e| e.to_string()),
                        "restore_error": restore_error.to_string(),
                        "target_visibility": "headed",
                    }),
                ),
            });
        }

        Ok(self.launch_policy_info())
    }

    pub(super) async fn current_restore_url(&self) -> Option<String> {
        let page = self.projected_continuity_page().await;
        let current_url = match page {
            Some(page) => page.url().await.ok().flatten().map(|url| url.to_string()),
            None => None,
        };
        current_url.filter(|url| !url.is_empty() && url != "about:blank")
    }

    async fn relaunch_and_restore_visible_locked(
        &self,
        restore_url: Option<&str>,
    ) -> Result<(), RubError> {
        self.ensure_browser_locked().await?;
        if let Some(url) = restore_url {
            let page = self.projected_active_page().await?;
            crate::page::navigate(
                &page,
                url,
                LoadStrategy::Load,
                std::time::Duration::from_millis(rub_core::DEFAULT_WAIT_TIMEOUT_MS),
            )
            .await?;
            self.sync_tabs_projection().await?;
        }
        Ok(())
    }

    pub async fn cancel_download(&self, guid: &str) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        let browser = self.browser_handle().await?;
        crate::downloads::cancel_download(&browser, guid).await
    }

    pub(super) async fn browser_handle(&self) -> Result<Arc<Browser>, RubError> {
        self.browser
            .lock()
            .await
            .clone()
            .ok_or_else(|| RubError::domain(ErrorCode::BrowserCrashed, "Browser is not available"))
    }

    async fn select_active_page_after_close(
        &self,
        pages_after: &[Arc<Page>],
        active_before: Option<&TargetId>,
        closing_active: bool,
        closed_index: usize,
    ) -> Result<Option<Arc<Page>>, RubError> {
        if pages_after.is_empty() {
            return Ok(None);
        }

        if closing_active {
            let fallback_index = closed_index.min(pages_after.len().saturating_sub(1));
            return Ok(pages_after.get(fallback_index).cloned());
        }

        if let Some(active_target) = active_before
            && let Some(page) = pages_after
                .iter()
                .find(|page| page.target_id() == active_target)
                .cloned()
        {
            return Ok(Some(page));
        }

        Ok(pages_after.first().cloned())
    }

    async fn tab_list_from_projection(
        &self,
        projection: &CommittedTabProjection,
    ) -> Result<Vec<TabInfo>, RubError> {
        let pages = &projection.pages;
        let active_target_id = projection.active_target_id.as_ref();
        let active_target_authority = projection.active_target_authority;

        let mut tabs = Vec::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            tabs.push(
                crate::tab_projection::tab_info_for_page(
                    index as u32,
                    page,
                    active_target_id,
                    active_target_authority,
                )
                .await,
            );
        }
        Ok(tabs)
    }
}

pub(super) fn pending_dialog_authority(
    pending_dialog: Option<rub_core::model::PendingDialogInfo>,
) -> Result<rub_core::model::PendingDialogInfo, RubError> {
    pending_dialog.ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "No pending JavaScript dialog is currently active".to_string(),
            serde_json::json!({
                "reason": "pending_dialog_missing",
            }),
        )
    })
}

pub(super) fn pending_dialog_target_id(
    pending_dialog: &rub_core::model::PendingDialogInfo,
) -> Result<String, RubError> {
    pending_dialog.tab_target_id.clone().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Pending JavaScript dialog lost its originating tab authority".to_string(),
            serde_json::json!({
                "reason": "pending_dialog_target_missing",
                "dialog_url": pending_dialog.url,
                "dialog_kind": pending_dialog.kind,
                "dialog_frame_id": pending_dialog.frame_id,
            }),
        )
    })
}

pub(super) fn stale_pending_dialog_target_error(target_id: &str, error: &RubError) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Pending JavaScript dialog target '{target_id}' is no longer live"),
        serde_json::json!({
            "pending_dialog_target_id": target_id,
            "reason": error.to_string(),
        }),
    )
}
