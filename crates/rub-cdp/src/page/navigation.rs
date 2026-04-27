use std::sync::Arc;

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::{
    EventLifecycleEvent, EventNavigatedWithinDocument, GetNavigationHistoryParams, NavigateParams,
    NavigateToHistoryEntryParams, ReloadParams, StopLoadingParams,
};
use futures::{Stream, StreamExt};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{HistoryNavigationResult, LoadStrategy, Page as RubPage};

use super::{FrameId, current_page_summary, current_page_summary_from_runtime};

pub(crate) enum HistoryDirection {
    Back,
    Forward,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum NavigateCommitKind {
    Download { warning: String },
    SameDocument,
    Lifecycle,
}

pub async fn back(page: &Arc<Page>, timeout: std::time::Duration) -> Result<RubPage, RubError> {
    let deadline = history_navigation_deadline(timeout, "back")?;
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Back, deadline).await?;
    execute_history_navigation_to_entry(page, target_entry.id, "Back", deadline).await?;
    wait_for_history_entry_commit(page, target_entry.id, deadline).await?;

    current_page_summary_after_history_commit(page, deadline).await
}

pub async fn back_with_boundary(
    page: &Arc<Page>,
    timeout: std::time::Duration,
) -> Result<HistoryNavigationResult, RubError> {
    let deadline = history_navigation_deadline(timeout, "back")?;
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Back, deadline).await?;
    execute_history_navigation_to_entry(page, target_entry.id, "Back", deadline).await?;
    wait_for_history_entry_commit(page, target_entry.id, deadline).await?;

    let page_summary = current_page_summary_after_history_commit(page, deadline).await?;
    let at_boundary = history_boundary_after_commit(page, HistoryDirection::Back, deadline).await;
    Ok(HistoryNavigationResult {
        page: page_summary,
        at_boundary,
    })
}

pub async fn forward(page: &Arc<Page>, timeout: std::time::Duration) -> Result<RubPage, RubError> {
    let deadline = history_navigation_deadline(timeout, "forward")?;
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Forward, deadline).await?;
    execute_history_navigation_to_entry(page, target_entry.id, "Forward", deadline).await?;
    wait_for_history_entry_commit(page, target_entry.id, deadline).await?;

    current_page_summary_after_history_commit(page, deadline).await
}

pub async fn forward_with_boundary(
    page: &Arc<Page>,
    timeout: std::time::Duration,
) -> Result<HistoryNavigationResult, RubError> {
    let deadline = history_navigation_deadline(timeout, "forward")?;
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Forward, deadline).await?;
    execute_history_navigation_to_entry(page, target_entry.id, "Forward", deadline).await?;
    wait_for_history_entry_commit(page, target_entry.id, deadline).await?;

    let page_summary = current_page_summary_after_history_commit(page, deadline).await?;
    let at_boundary =
        history_boundary_after_commit(page, HistoryDirection::Forward, deadline).await;
    Ok(HistoryNavigationResult {
        page: page_summary,
        at_boundary,
    })
}

pub async fn reload(
    page: &Arc<Page>,
    strategy: LoadStrategy,
    timeout: std::time::Duration,
) -> Result<RubPage, RubError> {
    let lifecycle_name = match strategy {
        LoadStrategy::Load => "load",
        LoadStrategy::DomContentLoaded => "DOMContentLoaded",
        LoadStrategy::NetworkIdle => "networkIdle",
    };
    reload_with_lifecycle(page, lifecycle_name, timeout).await?;

    current_page_summary(page).await
}

pub(crate) async fn navigate_with_lifecycle(
    page: &Arc<Page>,
    url: &str,
    lifecycle_name: &str,
    main_frame: FrameId,
    timeout: std::time::Duration,
) -> Result<Option<String>, RubError> {
    let mut lifecycle_listener = prepare_lifecycle_listener_stream(page).await?;
    let mut same_document_listener = prepare_same_document_listener_stream(page).await?;
    let navigate = page.execute(NavigateParams::new(url)).await.map_err(|e| {
        RubError::domain(
            ErrorCode::NavigationFailed,
            format!("Navigation to {url} failed: {e}"),
        )
    })?;
    match classify_navigate_commit(
        navigate.error_text.as_deref(),
        navigate.is_download.unwrap_or(false),
        navigate.loader_id.is_some(),
        url,
    )? {
        NavigateCommitKind::Download { warning } => Ok(Some(warning)),
        NavigateCommitKind::SameDocument => {
            wait_for_same_document_navigation_from_listener(
                committed_navigation_frame(main_frame, navigate.frame_id.clone()),
                &mut same_document_listener,
                timeout,
            )
            .await?;
            Ok(None)
        }
        NavigateCommitKind::Lifecycle => {
            wait_for_navigation_lifecycle_or_stop_loading(
                page,
                committed_navigation_frame(main_frame, navigate.frame_id.clone()),
                &mut lifecycle_listener,
                lifecycle_name,
                timeout,
            )
            .await?;
            Ok(None)
        }
    }
}

pub(crate) fn committed_navigation_frame(
    pre_command_main_frame: FrameId,
    navigate_frame: FrameId,
) -> FrameId {
    if navigate_frame != pre_command_main_frame {
        return navigate_frame;
    }
    pre_command_main_frame
}

pub(crate) fn classify_navigate_commit(
    error_text: Option<&str>,
    is_download: bool,
    has_loader_id: bool,
    url: &str,
) -> Result<NavigateCommitKind, RubError> {
    if let Some(error_text) = error_text {
        return Err(RubError::domain(
            ErrorCode::NavigationFailed,
            format!("Navigation to {url} failed: {error_text}"),
        ));
    }
    if is_download {
        return Ok(NavigateCommitKind::Download {
            warning: format!(
                "Navigation to {url} triggered a browser download; the active page remained on the current document"
            ),
        });
    }
    if has_loader_id {
        Ok(NavigateCommitKind::Lifecycle)
    } else {
        Ok(NavigateCommitKind::SameDocument)
    }
}

async fn reload_with_lifecycle(
    page: &Arc<Page>,
    lifecycle_name: &str,
    timeout: std::time::Duration,
) -> Result<(), RubError> {
    let (main_frame, mut listener) = prepare_lifecycle_listener(page, timeout).await?;
    page.execute(ReloadParams::default()).await.map_err(|e| {
        RubError::domain(ErrorCode::NavigationFailed, format!("Reload failed: {e}"))
    })?;
    wait_for_navigation_lifecycle_or_stop_loading(
        page,
        main_frame,
        &mut listener,
        lifecycle_name,
        timeout,
    )
    .await
}

pub(crate) fn history_navigation_deadline(
    timeout: std::time::Duration,
    action: &str,
) -> Result<tokio::time::Instant, RubError> {
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            format!(
                "Browser {action} navigation exhausted the authoritative timeout budget before the command could begin"
            ),
        ));
    }
    Ok(tokio::time::Instant::now() + timeout)
}

pub(crate) fn required_history_budget(
    deadline: tokio::time::Instant,
    exhausted_message: &'static str,
) -> Result<std::time::Duration, RubError> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            exhausted_message,
        ));
    }
    Ok(remaining)
}

pub(crate) fn optional_history_budget(
    deadline: tokio::time::Instant,
) -> Option<std::time::Duration> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

async fn execute_history_navigation_to_entry(
    page: &Arc<Page>,
    target_entry_id: i64,
    action: &str,
    deadline: tokio::time::Instant,
) -> Result<(), RubError> {
    let remaining = required_history_budget(
        deadline,
        "Browser history navigation exhausted the authoritative timeout budget before the navigation request could commit",
    )?;
    tokio::time::timeout(remaining, page.execute(NavigateToHistoryEntryParams::new(target_entry_id)))
        .await
        .map_err(|_| {
            RubError::domain(
                ErrorCode::PageLoadTimeout,
                format!(
                    "Browser {action} navigation exhausted the authoritative timeout budget before the navigation request could commit"
                ),
            )
        })?
        .map_err(|e| {
            RubError::domain(
                ErrorCode::NavigationFailed,
                format!("{action} navigation failed: {e}"),
            )
        })?;
    Ok(())
}

async fn wait_for_history_entry_commit(
    page: &Arc<Page>,
    target_entry_id: i64,
    deadline: tokio::time::Instant,
) -> Result<(), RubError> {
    loop {
        let Some(remaining) = optional_history_budget(deadline) else {
            break;
        };
        if let Ok(Ok(history)) = tokio::time::timeout(
            remaining.min(std::time::Duration::from_millis(100)),
            page.execute(GetNavigationHistoryParams::default()),
        )
        .await
            && let Ok(current_index) = usize::try_from(history.current_index)
            && let Some(current_entry) = history.entries.get(current_index)
            && current_entry.id == target_entry_id
        {
            return Ok(());
        }
        let sleep_for = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_millis(25));
        if sleep_for.is_zero() {
            break;
        }
        tokio::time::sleep(sleep_for).await;
    }
    Err(RubError::domain(
        ErrorCode::PageLoadTimeout,
        "Browser history navigation did not commit the requested history entry before the publish fence",
    ))
}

async fn current_page_summary_after_history_commit(
    page: &Arc<Page>,
    deadline: tokio::time::Instant,
) -> Result<RubPage, RubError> {
    loop {
        let remaining = required_history_budget(
            deadline,
            "Browser history navigation exhausted the authoritative timeout budget before the active page projection could commit",
        )?;
        match tokio::time::timeout(remaining, current_page_summary_from_runtime(page)).await {
            Ok(Ok(summary)) => return Ok(summary),
            Ok(Err(error)) => {
                let message = error.to_string();
                if !message.contains(
                    "Cannot capture runtime page summary: Error -32000: Cannot find context with specified id",
                ) && !message.contains(
                    "Cannot capture runtime page summary: Error -32000: Execution context was destroyed.",
                ) {
                    return Err(error);
                }
            }
            Err(_) => {
                return Err(RubError::domain(
                    ErrorCode::PageLoadTimeout,
                    "Browser history navigation exhausted the authoritative timeout budget before the active page projection could commit",
                ));
            }
        }
        let sleep_for = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_millis(25));
        if sleep_for.is_zero() {
            return Err(RubError::domain(
                ErrorCode::PageLoadTimeout,
                "Browser history navigation exhausted the authoritative timeout budget before the active page projection could commit",
            ));
        }
        tokio::time::sleep(sleep_for).await;
    }
}

async fn history_boundary_after_commit(
    page: &Arc<Page>,
    direction: HistoryDirection,
    deadline: tokio::time::Instant,
) -> Option<bool> {
    let remaining = optional_history_budget(deadline)?;
    let history = tokio::time::timeout(
        remaining,
        page.execute(GetNavigationHistoryParams::default()),
    )
    .await
    .ok()?
    .ok()?;
    history_boundary_from_history_state(direction, history.current_index, history.entries.len())
}

pub(crate) fn history_boundary_from_history_state(
    direction: HistoryDirection,
    current_index: i64,
    entry_count: usize,
) -> Option<bool> {
    let current_index = usize::try_from(current_index).ok()?;
    if current_index >= entry_count {
        return None;
    }
    Some(match direction {
        HistoryDirection::Back => current_index == 0,
        HistoryDirection::Forward => current_index + 1 >= entry_count,
    })
}

async fn resolve_history_navigation_target(
    page: &Arc<Page>,
    direction: HistoryDirection,
    deadline: tokio::time::Instant,
) -> Result<chromiumoxide::cdp::browser_protocol::page::NavigationEntry, RubError> {
    let remaining = required_history_budget(
        deadline,
        "Browser history navigation exhausted the authoritative timeout budget before a history target could be resolved",
    )?;
    let history = tokio::time::timeout(
        remaining,
        page.execute(GetNavigationHistoryParams::default()),
    )
    .await
    .map_err(|_| {
        RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Timed out while resolving browser history authority",
        )
    })?
    .map_err(|error| {
        RubError::domain(
            ErrorCode::NavigationFailed,
            format!("Failed to resolve browser history authority: {error}"),
        )
    })?;

    let current_index = usize::try_from(history.current_index).map_err(|_| {
        RubError::domain(
            ErrorCode::NavigationFailed,
            "Browser returned an invalid negative current history index",
        )
    })?;
    let target_index = match direction {
        HistoryDirection::Back => current_index.checked_sub(1),
        HistoryDirection::Forward => current_index.checked_add(1),
    }
    .ok_or_else(|| {
        RubError::domain(
            ErrorCode::NavigationFailed,
            match direction {
                HistoryDirection::Back => {
                    "Browser history has no previous committed entry for back navigation"
                }
                HistoryDirection::Forward => {
                    "Browser history has no next committed entry for forward navigation"
                }
            },
        )
    })?;

    history.entries.get(target_index).cloned().ok_or_else(|| {
        RubError::domain(
            ErrorCode::NavigationFailed,
            "Browser history target entry is no longer available",
        )
    })
}

async fn prepare_lifecycle_listener(
    page: &Arc<Page>,
    timeout: std::time::Duration,
) -> Result<
    (
        FrameId,
        impl Stream<Item = Arc<EventLifecycleEvent>> + Unpin,
    ),
    RubError,
> {
    let main_frame = resolve_navigation_main_frame(page, timeout).await?;
    let listener = prepare_lifecycle_listener_stream(page).await?;
    Ok((main_frame, listener))
}

async fn prepare_lifecycle_listener_stream(
    page: &Arc<Page>,
) -> Result<impl Stream<Item = Arc<EventLifecycleEvent>> + Unpin, RubError> {
    page.event_listener::<EventLifecycleEvent>()
        .await
        .map_err(|e| RubError::Internal(format!("Failed to subscribe to lifecycle events: {e}")))
}

async fn prepare_same_document_listener_stream(
    page: &Arc<Page>,
) -> Result<impl Stream<Item = Arc<EventNavigatedWithinDocument>> + Unpin, RubError> {
    page.event_listener::<EventNavigatedWithinDocument>()
        .await
        .map_err(|e| {
            RubError::Internal(format!(
                "Failed to subscribe to same-document navigation events: {e}"
            ))
        })
}

pub(crate) async fn resolve_navigation_main_frame(
    page: &Arc<Page>,
    timeout: std::time::Duration,
) -> Result<FrameId, RubError> {
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Timed out waiting for main-frame navigation authority",
        ));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(main_frame) = page.mainframe().await.ok().flatten() {
            return Ok(main_frame);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RubError::domain(
                ErrorCode::NavigationFailed,
                "Main-frame authority was unavailable before the navigation commit fence",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

pub(crate) async fn wait_for_lifecycle_event_from_listener<S>(
    main_frame: FrameId,
    listener: &mut S,
    lifecycle_name: &str,
    timeout: std::time::Duration,
) -> Result<(), RubError>
where
    S: Stream<Item = Arc<EventLifecycleEvent>> + Unpin,
{
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            format!("Timed out waiting for page lifecycle event '{lifecycle_name}'"),
        ));
    }
    tokio::time::timeout(timeout, async {
        while let Some(event) = listener.next().await {
            if event.frame_id != main_frame {
                continue;
            }
            if event.name == lifecycle_name
                || (lifecycle_name == "networkIdle" && event.name == "networkAlmostIdle")
            {
                return Ok(());
            }
        }
        Err(RubError::domain(
            ErrorCode::NavigationFailed,
            format!(
                "Lifecycle listener ended before page lifecycle event '{lifecycle_name}' crossed the publish fence"
            ),
        ))
    })
    .await
    .map_err(|_| {
        RubError::domain(
            ErrorCode::PageLoadTimeout,
            format!("Timed out waiting for page lifecycle event '{lifecycle_name}'"),
        )
    })?
}

pub(crate) async fn wait_for_same_document_navigation_from_listener<S>(
    main_frame: FrameId,
    listener: &mut S,
    timeout: std::time::Duration,
) -> Result<(), RubError>
where
    S: Stream<Item = Arc<EventNavigatedWithinDocument>> + Unpin,
{
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Timed out waiting for same-document navigation commit",
        ));
    }
    tokio::time::timeout(timeout, async {
        while let Some(event) = listener.next().await {
            if event.frame_id == main_frame {
                return Ok(());
            }
        }
        Err(RubError::domain(
            ErrorCode::NavigationFailed,
            "Same-document navigation listener ended before the navigation commit fence",
        ))
    })
    .await
    .map_err(|_| {
        RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Timed out waiting for same-document navigation commit",
        )
    })?
}

async fn wait_for_navigation_lifecycle_or_stop_loading<S>(
    page: &Arc<Page>,
    main_frame: FrameId,
    listener: &mut S,
    lifecycle_name: &str,
    timeout: std::time::Duration,
) -> Result<(), RubError>
where
    S: Stream<Item = Arc<EventLifecycleEvent>> + Unpin,
{
    match wait_for_lifecycle_event_from_listener(main_frame, listener, lifecycle_name, timeout)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            if matches!(&error, RubError::Domain(envelope) if envelope.code == ErrorCode::PageLoadTimeout)
                && let Err(stop_error) = page.execute(StopLoadingParams::default()).await
            {
                tracing::warn!(
                    error = %stop_error,
                    "StopLoading failed after navigation timeout"
                );
                return Err(navigation_timeout_with_stop_loading_failure(
                    error,
                    stop_error.to_string(),
                ));
            }
            Err(error)
        }
    }
}

fn navigation_timeout_with_stop_loading_failure(error: RubError, stop_error: String) -> RubError {
    let RubError::Domain(mut envelope) = error else {
        return error;
    };

    let mut context = envelope
        .context
        .take()
        .unwrap_or_else(|| serde_json::json!({}));
    if !context.is_object() {
        context = serde_json::json!({ "previous_context": context });
    }
    if let Some(object) = context.as_object_mut() {
        object.insert(
            "stop_loading_attempted".to_string(),
            serde_json::Value::Bool(true),
        );
        object.insert(
            "stop_loading_error".to_string(),
            serde_json::Value::String(stop_error),
        );
    }
    envelope.context = Some(context);
    RubError::Domain(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_timeout_preserves_stop_loading_failure_context() {
        let error = RubError::domain(ErrorCode::PageLoadTimeout, "navigation timed out");
        let envelope = navigation_timeout_with_stop_loading_failure(
            error,
            "target closed before StopLoading".to_string(),
        )
        .into_envelope();
        let context = envelope
            .context
            .expect("stop loading failure should be caller-visible context");

        assert_eq!(
            context.get("stop_loading_attempted"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            context.get("stop_loading_error"),
            Some(&serde_json::Value::String(
                "target closed before StopLoading".to_string()
            ))
        );
    }
}
