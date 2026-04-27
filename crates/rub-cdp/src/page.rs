//! Page navigation and load strategies (AUTH.CdpPageDomain).

mod display;
mod navigation;

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{EventResponseReceived, ResourceType};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::model::{LoadStrategy, Page as RubPage, ScrollDirection, ScrollPosition};

pub use self::display::{cleanup_highlights, highlight_elements, viewport_dimensions};
#[cfg(test)]
pub(super) use self::navigation::{
    HistoryDirection, NavigateCommitKind, classify_navigate_commit, committed_navigation_frame,
    history_boundary_from_history_state, history_navigation_deadline, optional_history_budget,
    required_history_budget, wait_for_lifecycle_event_from_listener,
    wait_for_same_document_navigation_from_listener,
};
pub use self::navigation::{back, back_with_boundary, forward, forward_with_boundary, reload};

type FrameId = chromiumoxide::cdp::browser_protocol::page::FrameId;

/// Navigate to URL with the specified load strategy.
pub async fn navigate(
    page: &Arc<Page>,
    url: &str,
    strategy: LoadStrategy,
    timeout: std::time::Duration,
) -> Result<RubPage, RubError> {
    let mut response_listener = page.event_listener::<EventResponseReceived>().await.ok();
    let main_frame = navigation::resolve_navigation_main_frame(page, timeout).await?;
    let navigation_warning = match strategy {
        LoadStrategy::Load => {
            navigation::navigate_with_lifecycle(page, url, "load", main_frame.clone(), timeout)
                .await?
        }
        LoadStrategy::DomContentLoaded => {
            navigation::navigate_with_lifecycle(
                page,
                url,
                "DOMContentLoaded",
                main_frame.clone(),
                timeout,
            )
            .await?
        }
        LoadStrategy::NetworkIdle => {
            navigation::navigate_with_lifecycle(
                page,
                url,
                "networkIdle",
                main_frame.clone(),
                timeout,
            )
            .await?
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

pub(super) async fn current_page_summary(page: &Arc<Page>) -> Result<RubPage, RubError> {
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

pub(super) async fn current_page_summary_from_runtime(
    page: &Arc<Page>,
) -> Result<RubPage, RubError> {
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
    context_id: Option<ExecutionContextId>,
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
            evaluate_scroll_script(page, format!("window.scrollBy(0, {y});"), context_id)
                .await
                .map_err(|e| {
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
        evaluate_scroll_script(page, format!("window.scrollBy(0, {y});"), context_id)
            .await
            .map_err(|e| RubError::Internal(format!("Scroll actuation failed: {e}")))?;
    }

    let result = evaluate_scroll_script(
        page,
        r#"
            JSON.stringify({
                x: window.pageXOffset || document.documentElement.scrollLeft,
                y: window.pageYOffset || document.documentElement.scrollTop,
                at_bottom: (window.innerHeight + window.pageYOffset) >= (document.documentElement.scrollHeight - 2)
            })
        "#
        .to_string(),
        context_id,
    )
    .await
    .map_err(|e| RubError::Internal(format!("Scroll position failed: {e}")))?;
    parse_scroll_position_result(result)
}

async fn evaluate_scroll_script(
    page: &Arc<Page>,
    expression: String,
    context_id: Option<ExecutionContextId>,
) -> Result<chromiumoxide::js::EvaluationResult, chromiumoxide::error::CdpError> {
    page.evaluate(build_scroll_evaluate_params(expression, context_id))
        .await
}

fn build_scroll_evaluate_params(
    expression: String,
    context_id: Option<ExecutionContextId>,
) -> EvaluateParams {
    let mut builder = EvaluateParams::builder().expression(expression);
    if let Some(context_id) = context_id {
        builder = builder.context_id(context_id);
    }
    builder
        .build()
        .expect("scroll evaluate params should build")
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

#[cfg(test)]
mod tests;
