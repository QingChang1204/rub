use super::*;
use crate::dialogs::DialogCallbacks;
use crate::downloads::DownloadCallbacks;
use std::sync::atomic::Ordering;

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
    ) -> (RubError, bool) {
        if let Err(rollback_error) = self.rollback(manager, callback_store).await {
            *manager.page_hook_states.lock().await = self.snapshot.hook_states;
            return (
                RubError::domain_with_context(
                    ErrorCode::InternalError,
                    format!(
                        "{surface} callback reconfigure failed and rollback could not restore the previous authority",
                    ),
                    serde_json::json!({
                        "surface": surface,
                        "reconfigure_error": reconfigure_error.into_envelope(),
                        "rollback_error": rollback_error.into_envelope(),
                    }),
                ),
                false,
            );
        }
        (reconfigure_error, true)
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
    #[cfg(test)]
    async fn maybe_pause_runtime_callback_reconfigure_before_reconcile_for_test(&self) {
        if self
            .pause_runtime_callback_reconfigure_before_reconcile
            .swap(false, Ordering::SeqCst)
        {
            self.runtime_callback_reconfigure_paused.notify_one();
            self.resume_runtime_callback_reconfigure.notified().await;
        }
    }

    #[cfg(test)]
    pub(super) fn force_pause_runtime_callback_reconfigure_before_reconcile(&self) {
        self.pause_runtime_callback_reconfigure_before_reconcile
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) async fn wait_for_runtime_callback_reconfigure_pause(&self) {
        self.runtime_callback_reconfigure_paused.notified().await;
    }

    #[cfg(test)]
    pub(super) fn resume_runtime_callback_reconfigure_for_test(&self) {
        self.resume_runtime_callback_reconfigure.notify_one();
    }

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
        let _reconfigure_guard = self.runtime_callback_reconfigure_lock.lock().await;
        self.set_runtime_callback_reconfigure_in_progress(true);
        let transaction =
            RuntimeCallbackReconfigureTransaction::begin(self, callback_store, callbacks).await;

        #[cfg(test)]
        self.maybe_pause_runtime_callback_reconfigure_before_reconcile_for_test()
            .await;

        if let Err(reconfigure_error) = self.reconcile_runtime_callbacks().await {
            let (error, rollback_restored_authority) = transaction
                .rollback_after_failure(self, callback_store, surface, reconfigure_error)
                .await;
            self.set_runtime_callback_reconfigure_in_progress(false);
            if rollback_restored_authority {
                self.replay_runtime_state_projection_to_callbacks().await;
                self.replay_dialog_runtime_projection_to_callbacks().await;
                self.replay_download_runtime_projection_to_callbacks().await;
            }
            return Err(error);
        }

        self.set_runtime_callback_reconfigure_in_progress(false);
        self.replay_runtime_state_projection_to_callbacks().await;
        self.replay_dialog_runtime_projection_to_callbacks().await;
        self.replay_download_runtime_projection_to_callbacks().await;
        Ok(())
    }
}

pub(super) fn guard_download_callbacks_for_commit(
    callbacks: DownloadCallbacks,
    authority_commit_in_progress: Arc<std::sync::atomic::AtomicBool>,
    runtime_callback_reconfigure_in_progress: Arc<std::sync::atomic::AtomicBool>,
) -> DownloadCallbacks {
    DownloadCallbacks {
        on_runtime: callbacks.on_runtime.map(|callback| {
            let authority_commit_in_progress = authority_commit_in_progress.clone();
            let runtime_callback_reconfigure_in_progress =
                runtime_callback_reconfigure_in_progress.clone();
            Arc::new(move |value| {
                if authority_commit_in_progress.load(Ordering::SeqCst)
                    || runtime_callback_reconfigure_in_progress.load(Ordering::SeqCst)
                {
                    return;
                }
                callback(value);
            }) as Arc<dyn Fn(crate::downloads::DownloadRuntimeUpdate) + Send + Sync>
        }),
        on_started: callbacks.on_started.map(|callback| {
            let authority_commit_in_progress = authority_commit_in_progress.clone();
            let runtime_callback_reconfigure_in_progress =
                runtime_callback_reconfigure_in_progress.clone();
            Arc::new(move |value| {
                if authority_commit_in_progress.load(Ordering::SeqCst)
                    || runtime_callback_reconfigure_in_progress.load(Ordering::SeqCst)
                {
                    return;
                }
                callback(value);
            }) as Arc<dyn Fn(crate::downloads::BrowserDownloadStart) + Send + Sync>
        }),
        on_progress: callbacks.on_progress.map(|callback| {
            let authority_commit_in_progress = authority_commit_in_progress.clone();
            let runtime_callback_reconfigure_in_progress =
                runtime_callback_reconfigure_in_progress.clone();
            Arc::new(move |value| {
                if authority_commit_in_progress.load(Ordering::SeqCst)
                    || runtime_callback_reconfigure_in_progress.load(Ordering::SeqCst)
                {
                    return;
                }
                callback(value);
            }) as Arc<dyn Fn(crate::downloads::BrowserDownloadProgress) + Send + Sync>
        }),
    }
}

pub(super) fn guard_dialog_callbacks_for_commit(
    callbacks: DialogCallbacks,
    authority_commit_in_progress: Arc<std::sync::atomic::AtomicBool>,
    runtime_callback_reconfigure_in_progress: Arc<std::sync::atomic::AtomicBool>,
) -> DialogCallbacks {
    DialogCallbacks {
        on_runtime: callbacks.on_runtime.map(|callback| {
            let authority_commit_in_progress = authority_commit_in_progress.clone();
            let runtime_callback_reconfigure_in_progress =
                runtime_callback_reconfigure_in_progress.clone();
            Arc::new(move |value| {
                if authority_commit_in_progress.load(Ordering::SeqCst)
                    || runtime_callback_reconfigure_in_progress.load(Ordering::SeqCst)
                {
                    return;
                }
                callback(value);
            }) as Arc<dyn Fn(crate::dialogs::DialogRuntimeUpdate) + Send + Sync>
        }),
        on_opened: callbacks.on_opened,
        on_closed: callbacks.on_closed,
        on_listener_ended: callbacks.on_listener_ended,
    }
}

#[cfg(test)]
mod tests {
    use super::{guard_dialog_callbacks_for_commit, guard_download_callbacks_for_commit};
    use crate::dialogs::{DialogCallbacks, DialogRuntimeUpdate};
    use crate::downloads::{
        BrowserDownloadProgress, BrowserDownloadStart, DownloadCallbacks, DownloadRuntimeUpdate,
    };
    use rub_core::model::{
        DialogRuntimeInfo, DialogRuntimeStatus, DownloadRuntimeInfo, DownloadRuntimeStatus,
        DownloadState,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[test]
    fn guarded_download_callbacks_suppress_delivery_while_reconfigure_commit_is_in_progress() {
        let authority_commit_in_progress = Arc::new(AtomicBool::new(false));
        let runtime_callback_reconfigure_in_progress = Arc::new(AtomicBool::new(true));
        let runtime_hits = Arc::new(AtomicUsize::new(0));
        let started_hits = Arc::new(AtomicUsize::new(0));
        let progress_hits = Arc::new(AtomicUsize::new(0));
        let callbacks = DownloadCallbacks {
            on_runtime: {
                let runtime_hits = runtime_hits.clone();
                Some(Arc::new(move |_| {
                    runtime_hits.fetch_add(1, Ordering::SeqCst);
                }))
            },
            on_started: {
                let started_hits = started_hits.clone();
                Some(Arc::new(move |_| {
                    started_hits.fetch_add(1, Ordering::SeqCst);
                }))
            },
            on_progress: {
                let progress_hits = progress_hits.clone();
                Some(Arc::new(move |_| {
                    progress_hits.fetch_add(1, Ordering::SeqCst);
                }))
            },
        };
        let guarded = guard_download_callbacks_for_commit(
            callbacks,
            authority_commit_in_progress.clone(),
            runtime_callback_reconfigure_in_progress.clone(),
        );

        guarded.on_runtime.as_ref().expect("runtime callback")(DownloadRuntimeUpdate {
            generation: 1,
            runtime: DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                ..DownloadRuntimeInfo::default()
            },
        });
        guarded.on_started.as_ref().expect("started callback")(BrowserDownloadStart {
            generation: 1,
            guid: "guid-1".to_string(),
            url: "https://example.test/file".to_string(),
            suggested_filename: "file.txt".to_string(),
            frame_id: None,
        });
        guarded.on_progress.as_ref().expect("progress callback")(BrowserDownloadProgress {
            generation: 1,
            guid: "guid-1".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 1,
            total_bytes: Some(10),
            final_path: None,
        });

        assert_eq!(runtime_hits.load(Ordering::SeqCst), 0);
        assert_eq!(started_hits.load(Ordering::SeqCst), 0);
        assert_eq!(progress_hits.load(Ordering::SeqCst), 0);

        runtime_callback_reconfigure_in_progress.store(false, Ordering::SeqCst);

        guarded.on_runtime.as_ref().expect("runtime callback")(DownloadRuntimeUpdate {
            generation: 2,
            runtime: DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                ..DownloadRuntimeInfo::default()
            },
        });
        guarded.on_started.as_ref().expect("started callback")(BrowserDownloadStart {
            generation: 2,
            guid: "guid-2".to_string(),
            url: "https://example.test/file".to_string(),
            suggested_filename: "file.txt".to_string(),
            frame_id: None,
        });
        guarded.on_progress.as_ref().expect("progress callback")(BrowserDownloadProgress {
            generation: 2,
            guid: "guid-2".to_string(),
            state: DownloadState::Completed,
            received_bytes: 10,
            total_bytes: Some(10),
            final_path: Some("/tmp/file.txt".to_string()),
        });

        assert_eq!(runtime_hits.load(Ordering::SeqCst), 1);
        assert_eq!(started_hits.load(Ordering::SeqCst), 1);
        assert_eq!(progress_hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn guarded_dialog_callbacks_suppress_runtime_delivery_while_reconfigure_commit_is_in_progress()
    {
        let authority_commit_in_progress = Arc::new(AtomicBool::new(false));
        let runtime_callback_reconfigure_in_progress = Arc::new(AtomicBool::new(true));
        let runtime_hits = Arc::new(AtomicUsize::new(0));
        let callbacks = DialogCallbacks {
            on_runtime: {
                let runtime_hits = runtime_hits.clone();
                Some(Arc::new(move |_| {
                    runtime_hits.fetch_add(1, Ordering::SeqCst);
                }))
            },
            on_opened: None,
            on_closed: None,
            on_listener_ended: None,
        };

        let guarded = guard_dialog_callbacks_for_commit(
            callbacks,
            authority_commit_in_progress.clone(),
            runtime_callback_reconfigure_in_progress.clone(),
        );

        guarded.on_runtime.as_ref().expect("runtime callback")(DialogRuntimeUpdate {
            generation: 3,
            runtime: DialogRuntimeInfo {
                status: DialogRuntimeStatus::Active,
                ..DialogRuntimeInfo::default()
            },
        });
        assert_eq!(runtime_hits.load(Ordering::SeqCst), 0);

        runtime_callback_reconfigure_in_progress.store(false, Ordering::SeqCst);
        guarded.on_runtime.as_ref().expect("runtime callback")(DialogRuntimeUpdate {
            generation: 3,
            runtime: DialogRuntimeInfo {
                status: DialogRuntimeStatus::Active,
                ..DialogRuntimeInfo::default()
            },
        });
        assert_eq!(runtime_hits.load(Ordering::SeqCst), 1);

        authority_commit_in_progress.store(true, Ordering::SeqCst);
        guarded.on_runtime.as_ref().expect("runtime callback")(DialogRuntimeUpdate {
            generation: 3,
            runtime: DialogRuntimeInfo {
                status: DialogRuntimeStatus::Degraded,
                ..DialogRuntimeInfo::default()
            },
        });
        assert_eq!(runtime_hits.load(Ordering::SeqCst), 1);
    }
}
