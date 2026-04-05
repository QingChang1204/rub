use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use rub_core::error::RubError;
use std::sync::Arc;
use tokio::time::Duration;

use crate::humanize::HumanizeConfig;

pub(crate) async fn dispatch_click(
    page: &Arc<Page>,
    x: f64,
    y: f64,
    button: MouseButton,
    click_count: i64,
) -> Result<(), RubError> {
    let params = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MousePressed)
        .x(x)
        .y(y)
        .button(button.clone())
        .click_count(click_count)
        .build()
        .map_err(|e| RubError::Internal(format!("Build mousePressed failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("mousePressed failed: {e}")))?;

    let params = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseReleased)
        .x(x)
        .y(y)
        .button(button)
        .click_count(click_count)
        .build()
        .map_err(|e| RubError::Internal(format!("Build mouseReleased failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("mouseReleased failed: {e}")))?;

    Ok(())
}

pub(crate) async fn move_to(
    page: &Arc<Page>,
    x: f64,
    y: f64,
    humanize: &HumanizeConfig,
) -> Result<(), RubError> {
    if !humanize.enabled {
        return dispatch_move(page, x, y).await;
    }

    let steps = humanize.speed.mouse_move_steps();
    let duration_ms = humanize.speed.mouse_move_duration();
    let delay_per_step = duration_ms / u64::from(steps).max(1);
    let start_x = x - 50.0;
    let start_y = y - 30.0;
    let path = crate::humanize::bezier_mouse_path(start_x, start_y, x, y, steps);

    for (px, py) in path {
        dispatch_move(page, px, py).await?;
        if delay_per_step > 0 {
            tokio::time::sleep(Duration::from_millis(delay_per_step)).await;
        }
    }

    Ok(())
}

pub(crate) async fn dispatch_move(page: &Arc<Page>, x: f64, y: f64) -> Result<(), RubError> {
    let params = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseMoved)
        .x(x)
        .y(y)
        .build()
        .map_err(|e| RubError::Internal(format!("Build mouseMoved failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("mouseMoved failed: {e}")))?;
    Ok(())
}
