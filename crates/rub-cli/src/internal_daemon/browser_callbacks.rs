use super::observatory_ingress::{
    NETWORK_REQUEST_INGRESS_LIMIT, OBSERVATORY_INGRESS_LIMIT, ObservatoryMutation,
    enqueue_network_request_record, enqueue_observatory_mutation,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const OBSERVATORY_CALLBACK_INSTALL_FAILED_REASON: &str = "observatory_callback_install_failed";
const RUNTIME_STATE_CALLBACK_INSTALL_FAILED_REASON: &str = "runtime_state_callback_install_failed";
const DIALOG_CALLBACK_INSTALL_FAILED_REASON: &str = "dialog_callback_install_failed";
const DOWNLOAD_CALLBACK_INSTALL_FAILED_REASON: &str = "download_callback_install_failed";

pub(super) async fn install_browser_callbacks(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
    browser_event_sink: &rub_daemon::session::BrowserSessionEventSink,
) {
    install_observatory_callbacks(browser_manager, state).await;
    install_runtime_state_callbacks(browser_manager, state).await;
    install_dialog_callbacks(browser_manager, state, browser_event_sink).await;
    install_download_callbacks(browser_manager, state, browser_event_sink).await;

    let state_for_callback = state.clone();
    browser_manager
        .set_epoch_callback(std::sync::Arc::new(move |target_id| {
            let _ = state_for_callback.observe_external_dom_change(target_id);
        }))
        .await;
}

async fn install_observatory_callbacks(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
) {
    let (observatory_tx, mut observatory_rx) =
        tokio::sync::mpsc::channel::<ObservatoryMutation>(OBSERVATORY_INGRESS_LIMIT);
    let (network_request_tx, mut network_request_rx) = tokio::sync::mpsc::channel::<
        Box<rub_core::model::ObservedNetworkRequestRecord>,
    >(NETWORK_REQUEST_INGRESS_LIMIT);
    let observatory_state = state.clone();
    tokio::spawn(async move {
        while let Some(mutation) = observatory_rx.recv().await {
            match mutation {
                ObservatoryMutation::ConsoleError(event) => {
                    observatory_state.record_console_error(event).await;
                }
                ObservatoryMutation::PageError(event) => {
                    observatory_state.record_page_error(event).await;
                }
                ObservatoryMutation::NetworkFailure(event) => {
                    observatory_state.record_network_failure(event).await;
                }
                ObservatoryMutation::RequestSummary(event) => {
                    observatory_state.record_request_summary(event).await;
                }
            }
        }
    });
    let request_record_state = state.clone();
    tokio::spawn(async move {
        while let Some(record) = network_request_rx.recv().await {
            request_record_state
                .upsert_observed_network_request_record(*record)
                .await;
        }
    });
    let observatory_overflowed = Arc::new(AtomicBool::new(false));
    let network_request_overflowed = Arc::new(AtomicBool::new(false));
    if let Err(error) = browser_manager
        .set_observatory_callbacks(rub_cdp::runtime_observatory::ObservatoryCallbacks {
            on_console_error: Some(std::sync::Arc::new({
                let observatory_tx = observatory_tx.clone();
                let observatory_state = state.clone();
                let observatory_overflowed = observatory_overflowed.clone();
                move |event| {
                    enqueue_observatory_mutation(
                        &observatory_tx,
                        ObservatoryMutation::ConsoleError(event),
                        &observatory_state,
                        &observatory_overflowed,
                    );
                }
            })),
            on_page_error: Some(std::sync::Arc::new({
                let observatory_tx = observatory_tx.clone();
                let observatory_state = state.clone();
                let observatory_overflowed = observatory_overflowed.clone();
                move |event| {
                    enqueue_observatory_mutation(
                        &observatory_tx,
                        ObservatoryMutation::PageError(event),
                        &observatory_state,
                        &observatory_overflowed,
                    );
                }
            })),
            on_network_failure: Some(std::sync::Arc::new({
                let observatory_tx = observatory_tx.clone();
                let observatory_state = state.clone();
                let observatory_overflowed = observatory_overflowed.clone();
                move |event| {
                    enqueue_observatory_mutation(
                        &observatory_tx,
                        ObservatoryMutation::NetworkFailure(event),
                        &observatory_state,
                        &observatory_overflowed,
                    );
                }
            })),
            on_request_summary: Some(std::sync::Arc::new({
                let observatory_tx = observatory_tx.clone();
                let observatory_state = state.clone();
                let observatory_overflowed = observatory_overflowed.clone();
                move |event| {
                    enqueue_observatory_mutation(
                        &observatory_tx,
                        ObservatoryMutation::RequestSummary(event),
                        &observatory_state,
                        &observatory_overflowed,
                    );
                }
            })),
            on_request_record: Some(std::sync::Arc::new({
                let observatory_state = state.clone();
                let network_request_tx = network_request_tx.clone();
                let network_request_overflowed = network_request_overflowed.clone();
                move |record| {
                    enqueue_network_request_record(
                        &network_request_tx,
                        record,
                        &observatory_state,
                        &network_request_overflowed,
                    );
                }
            })),
            on_runtime_degraded: Some(std::sync::Arc::new({
                let observatory_state = state.clone();
                move |reason| {
                    let observatory_state = observatory_state.clone();
                    tokio::spawn(async move {
                        observatory_state.mark_observatory_degraded(reason).await;
                    });
                }
            })),
            on_listener_ended: None,
        })
        .await
    {
        state
            .mark_observatory_degraded(callback_install_degraded_reason(
                OBSERVATORY_CALLBACK_INSTALL_FAILED_REASON,
                &error,
            ))
            .await;
    }
}

async fn install_runtime_state_callbacks(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
) {
    let runtime_state = state.clone();
    let runtime_sequence_state = state.clone();
    let runtime_state_browser_manager = browser_manager.clone();
    if let Err(error) = browser_manager
        .set_runtime_state_callbacks(rub_cdp::runtime_state::RuntimeStateCallbacks {
            allocate_sequence: Some(std::sync::Arc::new(move || {
                runtime_sequence_state.allocate_runtime_state_sequence()
            })),
            on_snapshot: Some(std::sync::Arc::new(
                move |sequence, listener_generation, active_target_id, snapshot| {
                    let state = runtime_state.clone();
                    let browser_manager = runtime_state_browser_manager.clone();
                    tokio::spawn(async move {
                        browser_manager
                            .publish_runtime_state_callback_if_active_target(
                                listener_generation,
                                active_target_id.as_deref(),
                                || async move {
                                    state
                                        .publish_runtime_state_snapshot_if(
                                            sequence,
                                            snapshot,
                                            || true,
                                        )
                                        .await
                                },
                            )
                            .await;
                    });
                },
            )),
        })
        .await
    {
        let sequence = state.allocate_runtime_state_sequence();
        state
            .mark_runtime_state_probe_degraded(
                sequence,
                callback_install_degraded_reason(
                    RUNTIME_STATE_CALLBACK_INSTALL_FAILED_REASON,
                    &error,
                ),
            )
            .await;
    }
}

async fn install_dialog_callbacks(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
    browser_event_sink: &rub_daemon::session::BrowserSessionEventSink,
) {
    let runtime_state = state.clone();
    let opened_state = state.clone();
    let closed_state = state.clone();
    let runtime_event_sink = browser_event_sink.clone();
    let opened_event_sink = browser_event_sink.clone();
    let closed_event_sink = browser_event_sink.clone();
    if let Err(error) = browser_manager
        .set_dialog_callbacks(rub_cdp::dialogs::DialogCallbacks {
            on_runtime: Some(std::sync::Arc::new(move |runtime| {
                let browser_sequence = runtime_state.allocate_browser_event_sequence();
                runtime_event_sink.enqueue(
                    rub_daemon::session::BrowserSessionEvent::DialogRuntime {
                        browser_sequence,
                        generation: runtime.generation,
                        runtime: Box::new(runtime.runtime),
                    },
                );
            })),
            on_opened: Some(std::sync::Arc::new(move |event| {
                let browser_sequence = opened_state.allocate_browser_event_sequence();
                opened_event_sink.enqueue(rub_daemon::session::BrowserSessionEvent::DialogOpened {
                    browser_sequence,
                    generation: event.generation,
                    kind: event.kind,
                    message: event.message,
                    url: event.url,
                    tab_target_id: event.tab_target_id,
                    frame_id: event.frame_id,
                    default_prompt: event.default_prompt,
                    has_browser_handler: event.has_browser_handler,
                });
            })),
            on_closed: Some(std::sync::Arc::new(move |event| {
                let browser_sequence = closed_state.allocate_browser_event_sequence();
                closed_event_sink.enqueue(rub_daemon::session::BrowserSessionEvent::DialogClosed {
                    browser_sequence,
                    generation: event.generation,
                    accepted: event.accepted,
                    user_input: event.user_input,
                });
            })),
            on_listener_ended: None,
        })
        .await
    {
        state
            .mark_dialog_runtime_degraded(
                browser_manager.current_listener_generation(),
                callback_install_degraded_reason(DIALOG_CALLBACK_INSTALL_FAILED_REASON, &error),
            )
            .await;
    }
}

async fn install_download_callbacks(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
    browser_event_sink: &rub_daemon::session::BrowserSessionEventSink,
) {
    let runtime_event_sink = browser_event_sink.clone();
    let started_event_sink = browser_event_sink.clone();
    let progress_event_sink = browser_event_sink.clone();
    if let Err(error) = browser_manager
        .set_download_callbacks(rub_cdp::downloads::DownloadCallbacks {
            on_runtime: Some(std::sync::Arc::new(move |runtime| {
                runtime_event_sink.enqueue_download_runtime(runtime.generation, runtime.runtime);
            })),
            on_started: Some(std::sync::Arc::new(move |event| {
                started_event_sink.enqueue_download_started(
                    event.generation,
                    event.guid,
                    event.url,
                    event.suggested_filename,
                    event.frame_id,
                );
            })),
            on_progress: Some(std::sync::Arc::new(move |event| {
                progress_event_sink.enqueue_download_progress(
                    event.generation,
                    event.guid,
                    event.state,
                    event.received_bytes,
                    event.total_bytes,
                    event.final_path,
                );
            })),
        })
        .await
    {
        state
            .mark_download_runtime_degraded(
                browser_manager.current_listener_generation(),
                callback_install_degraded_reason(DOWNLOAD_CALLBACK_INSTALL_FAILED_REASON, &error),
            )
            .await;
    }
}

fn callback_install_degraded_reason(
    default_reason: &'static str,
    _error: &rub_core::error::RubError,
) -> &'static str {
    default_reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_install_degraded_reason_is_stable() {
        let error = rub_core::error::RubError::Internal("callback hook rebuild failed".to_string());

        assert_eq!(
            callback_install_degraded_reason(OBSERVATORY_CALLBACK_INSTALL_FAILED_REASON, &error),
            OBSERVATORY_CALLBACK_INSTALL_FAILED_REASON
        );
        assert_eq!(
            callback_install_degraded_reason(RUNTIME_STATE_CALLBACK_INSTALL_FAILED_REASON, &error),
            RUNTIME_STATE_CALLBACK_INSTALL_FAILED_REASON
        );
        assert_eq!(
            callback_install_degraded_reason(DIALOG_CALLBACK_INSTALL_FAILED_REASON, &error),
            DIALOG_CALLBACK_INSTALL_FAILED_REASON
        );
        assert_eq!(
            callback_install_degraded_reason(DOWNLOAD_CALLBACK_INSTALL_FAILED_REASON, &error),
            DOWNLOAD_CALLBACK_INSTALL_FAILED_REASON
        );
    }
}
