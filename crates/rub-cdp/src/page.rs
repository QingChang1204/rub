//! Page navigation and load strategies (AUTH.CdpPageDomain).

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::{
    network::{EventResponseReceived, ResourceType},
    page::{
        EventLifecycleEvent, EventNavigatedWithinDocument, GetNavigationHistoryParams,
        NavigateParams, NavigateToHistoryEntryParams, ReloadParams, StopLoadingParams,
    },
};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{LoadStrategy, Page as RubPage, ScrollDirection, ScrollPosition, Snapshot};

type FrameId = chromiumoxide::cdp::browser_protocol::page::FrameId;

enum HistoryDirection {
    Back,
    Forward,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum NavigateCommitKind {
    Download { warning: String },
    SameDocument,
    Lifecycle,
}

/// Navigate to URL with the specified load strategy.
pub async fn navigate(
    page: &Arc<Page>,
    url: &str,
    strategy: LoadStrategy,
    timeout: std::time::Duration,
) -> Result<RubPage, RubError> {
    let mut response_listener = page.event_listener::<EventResponseReceived>().await.ok();
    let main_frame = resolve_navigation_main_frame(page, timeout).await?;
    let navigation_warning = match strategy {
        LoadStrategy::Load => {
            navigate_with_lifecycle(page, url, "load", main_frame.clone(), timeout).await?
        }
        LoadStrategy::DomContentLoaded => {
            navigate_with_lifecycle(page, url, "DOMContentLoaded", main_frame.clone(), timeout)
                .await?
        }
        LoadStrategy::NetworkIdle => {
            navigate_with_lifecycle(page, url, "networkIdle", main_frame.clone(), timeout).await?
        }
    };

    // Get final URL and title
    let final_url = page
        .url()
        .await
        .map_err(|e| RubError::Internal(format!("Cannot get URL: {e}")))?
        .map(|u| u.to_string())
        .unwrap_or_else(|| url.to_string());

    let title = page
        .get_title()
        .await
        .map_err(|e| RubError::Internal(format!("Cannot get title: {e}")))?
        .unwrap_or_default();

    let http_status = if let Some(listener) = response_listener.as_mut() {
        navigation_http_status(listener, &main_frame, &final_url).await
    } else {
        None
    };

    Ok(RubPage {
        url: url.to_string(),
        title,
        http_status,
        final_url,
        navigation_warning,
    })
}

async fn navigation_http_status<S>(
    listener: &mut S,
    main_frame: &FrameId,
    final_url: &str,
) -> Option<u16>
where
    S: Stream<Item = Arc<EventResponseReceived>> + Unpin,
{
    let mut fallback_status = None;

    loop {
        let event =
            match tokio::time::timeout(std::time::Duration::from_millis(50), listener.next()).await
            {
                Ok(Some(event)) => event,
                Ok(None) | Err(_) => break,
            };

        if event.r#type != ResourceType::Document {
            continue;
        }

        if event.frame_id.as_ref() != Some(main_frame) {
            continue;
        }

        let status = u16::try_from(event.response.status).ok();
        if event.response.url == final_url && status.is_some() {
            return status;
        }

        fallback_status = status;
    }

    fallback_status
}

async fn current_page_summary(page: &Arc<Page>) -> Result<RubPage, RubError> {
    let url = page
        .url()
        .await
        .map_err(|e| RubError::Internal(format!("Cannot get URL: {e}")))?
        .map(|u| u.to_string())
        .unwrap_or_default();

    let title = page
        .get_title()
        .await
        .map_err(|e| RubError::Internal(format!("Cannot get title: {e}")))?
        .unwrap_or_default();

    Ok(RubPage {
        url: url.clone(),
        title,
        http_status: None,
        final_url: url,
        navigation_warning: None,
    })
}

#[derive(Debug, Deserialize)]
struct RuntimePageSummaryProbe {
    url: String,
    title: String,
}

async fn current_page_summary_from_runtime(page: &Arc<Page>) -> Result<RubPage, RubError> {
    let result = page
        .evaluate(
            r#"
            JSON.stringify({
                url: window.location.href,
                title: document.title
            })
        "#,
        )
        .await
        .map_err(|e| RubError::Internal(format!("Cannot capture runtime page summary: {e}")))?;
    let json_str = result.into_value::<String>().map_err(|e| {
        RubError::Internal(format!(
            "Runtime page summary returned non-string payload: {e}"
        ))
    })?;
    let summary: RuntimePageSummaryProbe = serde_json::from_str(&json_str)
        .map_err(|e| RubError::Internal(format!("Parse runtime page summary failed: {e}")))?;

    Ok(RubPage {
        url: summary.url.clone(),
        title: summary.title,
        http_status: None,
        final_url: summary.url,
        navigation_warning: None,
    })
}

/// Capture a viewport screenshot as PNG bytes.
pub async fn screenshot(page: &Arc<Page>, full_page: bool) -> Result<Vec<u8>, RubError> {
    if full_page {
        // Full page screenshot using CDP
        let params = chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams::builder()
            .capture_beyond_viewport(true)
            .build();
        let data = page
            .execute(params)
            .await
            .map_err(|e| RubError::Internal(format!("Screenshot failed: {e}")))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&data.data)
            .map_err(|e| RubError::Internal(format!("Base64 decode failed: {e}")))?;
        Ok(bytes)
    } else {
        let bytes = page
            .screenshot(chromiumoxide::page::ScreenshotParams::builder().build())
            .await
            .map_err(|e| RubError::Internal(format!("Screenshot failed: {e}")))?;
        Ok(bytes)
    }
}

pub async fn scroll(
    page: &Arc<Page>,
    direction: ScrollDirection,
    amount: Option<u32>,
    humanize: &crate::humanize::HumanizeConfig,
) -> Result<ScrollPosition, RubError> {
    let pixels = amount.unwrap_or(600);

    if humanize.enabled {
        let step_size = 80u32;
        let steps = (pixels / step_size).max(1);
        let remainder = pixels - (steps - 1) * step_size;
        let (delay_min, delay_max) = humanize.speed.scroll_delay_range();

        for i in 0..steps {
            let px = if i == steps - 1 { remainder } else { step_size };
            let y = match direction {
                ScrollDirection::Up => -(px as i32),
                ScrollDirection::Down => px as i32,
            };
            let js = format!("window.scrollBy(0, {y});");
            page.evaluate(js).await.map_err(|e| {
                RubError::Internal(format!("Humanized scroll actuation failed: {e}"))
            })?;
            tokio::time::sleep(std::time::Duration::from_millis(
                crate::humanize::random_delay(delay_min, delay_max),
            ))
            .await;
        }
    } else {
        let y = match direction {
            ScrollDirection::Up => -(pixels as i32),
            ScrollDirection::Down => pixels as i32,
        };
        let js = format!("window.scrollBy(0, {y});");
        page.evaluate(js)
            .await
            .map_err(|e| RubError::Internal(format!("Scroll actuation failed: {e}")))?;
    }

    let result = page
        .evaluate(
            r#"
            JSON.stringify({
                x: window.pageXOffset || document.documentElement.scrollLeft,
                y: window.pageYOffset || document.documentElement.scrollTop,
                at_bottom: (window.innerHeight + window.pageYOffset) >= (document.documentElement.scrollHeight - 2)
            })
        "#,
        )
        .await
        .map_err(|e| RubError::Internal(format!("Scroll position failed: {e}")))?;
    parse_scroll_position_result(result)
}

fn parse_scroll_position_result(
    result: chromiumoxide::js::EvaluationResult,
) -> Result<ScrollPosition, RubError> {
    let json_str = result.into_value::<String>().map_err(|e| {
        RubError::Internal(format!("Scroll position returned non-string payload: {e}"))
    })?;
    parse_scroll_position_json(json_str)
}

fn parse_scroll_position_json(json_str: String) -> Result<ScrollPosition, RubError> {
    serde_json::from_str(&json_str)
        .map_err(|e| RubError::Internal(format!("Parse scroll position failed: {e}")))
}

pub async fn back(page: &Arc<Page>, timeout: std::time::Duration) -> Result<RubPage, RubError> {
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Back, timeout).await?;
    page.execute(NavigateToHistoryEntryParams::new(target_entry.id))
        .await
        .map_err(|e| {
            RubError::domain(
                ErrorCode::NavigationFailed,
                format!("Back navigation failed: {e}"),
            )
        })?;
    wait_for_history_entry_commit(page, target_entry.id, timeout).await?;

    current_page_summary_after_history_commit(page, timeout).await
}

pub async fn forward(page: &Arc<Page>, timeout: std::time::Duration) -> Result<RubPage, RubError> {
    let target_entry =
        resolve_history_navigation_target(page, HistoryDirection::Forward, timeout).await?;
    page.execute(NavigateToHistoryEntryParams::new(target_entry.id))
        .await
        .map_err(|e| {
            RubError::domain(
                ErrorCode::NavigationFailed,
                format!("Forward navigation failed: {e}"),
            )
        })?;
    wait_for_history_entry_commit(page, target_entry.id, timeout).await?;

    current_page_summary_after_history_commit(page, timeout).await
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

async fn navigate_with_lifecycle(
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
                main_frame,
                &mut same_document_listener,
                timeout,
            )
            .await?;
            Ok(None)
        }
        NavigateCommitKind::Lifecycle => {
            wait_for_navigation_lifecycle_or_stop_loading(
                page,
                main_frame,
                &mut lifecycle_listener,
                lifecycle_name,
                timeout,
            )
            .await?;
            Ok(None)
        }
    }
}

fn classify_navigate_commit(
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

async fn wait_for_history_entry_commit(
    page: &Arc<Page>,
    target_entry_id: i64,
    timeout: std::time::Duration,
) -> Result<(), RubError> {
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Browser history navigation exhausted the authoritative timeout budget before the target history entry could commit",
        ));
    }

    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if let Ok(history) = page.execute(GetNavigationHistoryParams::default()).await
            && let Ok(current_index) = usize::try_from(history.current_index)
            && let Some(current_entry) = history.entries.get(current_index)
            && current_entry.id == target_entry_id
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(RubError::domain(
        ErrorCode::PageLoadTimeout,
        "Browser history navigation did not commit the requested history entry before the publish fence",
    ))
}

async fn current_page_summary_after_history_commit(
    page: &Arc<Page>,
    timeout: std::time::Duration,
) -> Result<RubPage, RubError> {
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Browser history navigation exhausted the authoritative timeout budget before the active page projection could commit",
        ));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match current_page_summary_from_runtime(page).await {
            Ok(summary) => return Ok(summary),
            Err(error) if tokio::time::Instant::now() < deadline => {
                let message = error.to_string();
                if !message.contains(
                    "Cannot capture runtime page summary: Error -32000: Cannot find context with specified id",
                ) && !message.contains(
                    "Cannot capture runtime page summary: Error -32000: Execution context was destroyed.",
                )
                {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn resolve_history_navigation_target(
    page: &Arc<Page>,
    direction: HistoryDirection,
    timeout: std::time::Duration,
) -> Result<chromiumoxide::cdp::browser_protocol::page::NavigationEntry, RubError> {
    if timeout.is_zero() {
        return Err(RubError::domain(
            ErrorCode::PageLoadTimeout,
            "Browser history navigation exhausted the authoritative timeout budget before a history target could be resolved",
        ));
    }
    let history =
        tokio::time::timeout(timeout, page.execute(GetNavigationHistoryParams::default()))
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

async fn resolve_navigation_main_frame(
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

async fn wait_for_lifecycle_event_from_listener<S>(
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

async fn wait_for_same_document_navigation_from_listener<S>(
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
            {
                let _ = page.execute(StopLoadingParams::default()).await;
            }
            Err(error)
        }
    }
}

pub async fn viewport_dimensions(page: &Arc<Page>) -> Result<(f64, f64), RubError> {
    #[derive(Deserialize)]
    struct Viewport {
        w: f64,
        h: f64,
    }

    let result = page
        .evaluate("JSON.stringify({ w: window.innerWidth, h: window.innerHeight })")
        .await
        .map_err(|e| RubError::Internal(format!("viewport_dimensions failed: {e}")))?;
    let json_str = result
        .into_value::<String>()
        .map_err(|e| RubError::Internal(format!("viewport parse failed: {e}")))?;
    let viewport: Viewport = serde_json::from_str(&json_str)
        .map_err(|e| RubError::Internal(format!("viewport JSON parse failed: {e}")))?;
    Ok((viewport.w, viewport.h))
}

pub async fn highlight_elements(page: &Arc<Page>, snapshot: &Snapshot) -> Result<u32, RubError> {
    let script = crate::dom::highlight_overlay_js(snapshot)?;
    let result = page
        .evaluate(script)
        .await
        .map_err(|e| RubError::Internal(format!("highlight injection failed: {e}")))?;
    Ok(result.into_value::<f64>().unwrap_or(0.0) as u32)
}

pub async fn cleanup_highlights(page: &Arc<Page>) -> Result<(), RubError> {
    page.evaluate(crate::dom::CLEANUP_HIGHLIGHT_JS)
        .await
        .map_err(|e| RubError::Internal(format!("highlight cleanup failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        FrameId, NavigateCommitKind, classify_navigate_commit, parse_scroll_position_json,
        wait_for_lifecycle_event_from_listener, wait_for_same_document_navigation_from_listener,
    };
    use chromiumoxide::cdp::browser_protocol::page::{
        EventNavigatedWithinDocument, NavigatedWithinDocumentNavigationType,
    };
    use rub_core::error::ErrorCode;
    use std::sync::Arc;
    use std::time::Duration;

    fn frame_id(value: &str) -> FrameId {
        serde_json::from_value(serde_json::json!(value)).expect("frame id")
    }

    #[tokio::test]
    async fn lifecycle_listener_ending_before_event_is_navigation_failed() {
        let mut listener = futures::stream::empty();
        let error = wait_for_lifecycle_event_from_listener(
            frame_id("main"),
            &mut listener,
            "networkIdle",
            Duration::from_millis(50),
        )
        .await
        .expect_err("listener EOF before lifecycle event should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::NavigationFailed);
        assert!(envelope.message.contains("Lifecycle listener ended"));
    }

    #[tokio::test]
    async fn lifecycle_wait_honors_caller_timeout_budget() {
        let mut listener = futures::stream::pending();
        let error = tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_lifecycle_event_from_listener(
                frame_id("main"),
                &mut listener,
                "networkIdle",
                Duration::from_millis(30),
            ),
        )
        .await
        .expect("the wait fence should honor the caller timeout instead of sleeping on a hidden multi-second budget")
        .expect_err("pending listener should time out at the caller budget");
        assert_eq!(error.into_envelope().code, ErrorCode::PageLoadTimeout);
    }

    #[tokio::test]
    async fn same_document_navigation_wait_succeeds_for_main_frame_commit() {
        let mut listener = futures::stream::iter(vec![Arc::new(EventNavigatedWithinDocument {
            frame_id: frame_id("main"),
            url: "https://example.com/page#section".to_string(),
            navigation_type: NavigatedWithinDocumentNavigationType::Fragment,
        })]);

        wait_for_same_document_navigation_from_listener(
            frame_id("main"),
            &mut listener,
            Duration::from_millis(50),
        )
        .await
        .expect("same-document commit should satisfy the fence");
    }

    #[tokio::test]
    async fn same_document_navigation_wait_honors_caller_timeout_budget() {
        let mut listener = futures::stream::pending();
        let error = tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_same_document_navigation_from_listener(
                frame_id("main"),
                &mut listener,
                Duration::from_millis(30),
            ),
        )
        .await
        .expect("the same-document fence should honor the caller timeout")
        .expect_err("pending same-document listener should time out at the caller budget");
        assert_eq!(error.into_envelope().code, ErrorCode::PageLoadTimeout);
    }

    #[test]
    fn navigate_commit_classifies_protocol_result_exhaustively() {
        assert_eq!(
            classify_navigate_commit(None, true, true, "https://example.com/file.csv")
                .expect("download navigation should classify"),
            NavigateCommitKind::Download {
                warning: "Navigation to https://example.com/file.csv triggered a browser download; the active page remained on the current document".to_string(),
            }
        );
        assert_eq!(
            classify_navigate_commit(None, false, false, "https://example.com/page#section")
                .expect("same-document navigation should classify"),
            NavigateCommitKind::SameDocument
        );
        assert_eq!(
            classify_navigate_commit(None, false, true, "https://example.com")
                .expect("cross-document navigation should classify"),
            NavigateCommitKind::Lifecycle
        );

        let error = classify_navigate_commit(
            Some("net::ERR_NAME_NOT_RESOLVED"),
            false,
            true,
            "https://missing.invalid",
        )
        .expect_err("protocol error text should fail immediately");
        assert_eq!(error.into_envelope().code, ErrorCode::NavigationFailed);
    }

    #[test]
    fn parse_scroll_position_json_rejects_invalid_probe_payload() {
        let error =
            parse_scroll_position_json("{".to_string()).expect_err("invalid json should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
    }
}
