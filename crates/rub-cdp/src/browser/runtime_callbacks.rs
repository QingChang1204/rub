use super::*;

#[derive(Clone)]
struct RuntimeCallbackReconfigureSnapshot<T> {
    callbacks: T,
    hook_states: HashMap<String, PageHookInstallState>,
}

struct RuntimeCallbackReconfigureTransaction<T> {
    snapshot: RuntimeCallbackReconfigureSnapshot<T>,
}

impl<T> RuntimeCallbackReconfigureTransaction<T>
where
    T: Clone + Send + 'static,
{
    async fn begin(manager: &BrowserManager, callback_store: &Arc<Mutex<T>>, callbacks: T) -> Self {
        let snapshot = RuntimeCallbackReconfigureSnapshot {
            callbacks: callback_store.lock().await.clone(),
            hook_states: manager.page_hook_states.lock().await.clone(),
        };
        *callback_store.lock().await = callbacks;
        manager.invalidate_runtime_callback_page_hooks().await;
        Self { snapshot }
    }

    async fn rollback(
        &self,
        manager: &BrowserManager,
        callback_store: &Arc<Mutex<T>>,
    ) -> Result<(), RubError> {
        self.restore_snapshot(manager, callback_store).await
    }

    async fn rollback_after_failure(
        self,
        manager: &BrowserManager,
        callback_store: &Arc<Mutex<T>>,
        surface: &str,
        reconfigure_error: RubError,
    ) -> RubError {
        if let Err(rollback_error) = self.rollback(manager, callback_store).await {
            *manager.page_hook_states.lock().await = self.snapshot.hook_states;
            return RubError::domain_with_context(
                ErrorCode::InternalError,
                format!(
                    "{surface} callback reconfigure failed and rollback could not restore the previous authority",
                ),
                serde_json::json!({
                    "surface": surface,
                    "reconfigure_error": reconfigure_error.into_envelope(),
                    "rollback_error": rollback_error.into_envelope(),
                }),
            );
        }
        reconfigure_error
    }

    async fn restore_snapshot(
        &self,
        manager: &BrowserManager,
        callback_store: &Arc<Mutex<T>>,
    ) -> Result<(), RubError> {
        *callback_store.lock().await = self.snapshot.callbacks.clone();
        if manager.browser.lock().await.is_some() {
            manager.invalidate_runtime_callback_page_hooks().await;
            manager.reconcile_runtime_callbacks().await.map(|_| ())
        } else {
            *manager.page_hook_states.lock().await = self.snapshot.hook_states.clone();
            Ok(())
        }
    }
}

impl BrowserManager {
    pub(super) fn maybe_fail_runtime_callback_reconcile_for_test(&self) -> Result<(), RubError> {
        #[cfg(test)]
        {
            if self
                .force_reconcile_runtime_callbacks_failure
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(RubError::domain(
                    ErrorCode::InternalError,
                    "forced runtime callback reconcile failure",
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn reconcile_runtime_callbacks(&self) -> Result<ListenerGeneration, RubError> {
        self.maybe_fail_runtime_callback_reconcile_for_test()?;
        let Some(browser) = self.browser.lock().await.clone() else {
            return Ok(self.listener_generation());
        };
        self.reconcile_generation_bound_runtime_candidate(browser)
            .await
    }

    #[cfg(test)]
    pub(super) fn force_runtime_callback_reconcile_failure(&self) {
        self.force_reconcile_runtime_callbacks_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) async fn invalidate_runtime_callback_page_hooks(&self) {
        let mut hook_states = self.page_hook_states.lock().await;
        for state in hook_states.values_mut() {
            state.invalidate_runtime_callback_hooks();
        }
    }

    pub(super) async fn reconfigure_runtime_callbacks<T>(
        &self,
        callback_store: &Arc<Mutex<T>>,
        callbacks: T,
        surface: &str,
    ) -> Result<(), RubError>
    where
        T: Clone + Send + 'static,
    {
        let transaction =
            RuntimeCallbackReconfigureTransaction::begin(self, callback_store, callbacks).await;

        if let Err(reconfigure_error) = self.reconcile_runtime_callbacks().await {
            return Err(transaction
                .rollback_after_failure(self, callback_store, surface, reconfigure_error)
                .await);
        }

        Ok(())
    }
}
