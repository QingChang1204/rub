use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{KeyCombo, Modifier};
use std::sync::Arc;
use tokio::time::Duration;

use crate::humanize::{HumanizeConfig, random_delay};

const INPUT_RELEASE_GUARD_DELAY: Duration = Duration::from_millis(600);

pub(crate) async fn focus_pause(humanize: &HumanizeConfig) {
    if !humanize.enabled {
        return;
    }
    let (delay_min, delay_max) = humanize.speed.typing_delay_range();
    let pause_ms = random_delay(delay_min.max(50), delay_max.max(delay_min.max(50)));
    tokio::time::sleep(Duration::from_millis(pause_ms)).await;
}

pub(crate) async fn send_keys(page: &Arc<Page>, combo: &KeyCombo) -> Result<(), RubError> {
    let key_def = crate::keys::lookup(&combo.key).ok_or_else(|| {
        let msg = if crate::keys::looks_like_plain_text(&combo.key) {
            format!(
                "Unknown key name: '{}'. This looks like plain text — use 'rub type \"{}\"' instead",
                combo.key, combo.key
            )
        } else {
            format!(
                "Unknown key name: '{}'. Check spelling or see W3C UIEvents key values",
                combo.key
            )
        };
        RubError::domain(ErrorCode::InvalidKeyName, msg)
    })?;

    let modifier_flags = combo.modifiers.iter().fold(0u32, |acc, m| {
        acc | match m {
            Modifier::Alt => crate::keys::modifiers::ALT,
            Modifier::Control => crate::keys::modifiers::CONTROL,
            Modifier::Meta => crate::keys::modifiers::META,
            Modifier::Shift => crate::keys::modifiers::SHIFT,
        }
    });

    let mut modifier_release_guards = Vec::new();
    for modifier in &combo.modifiers {
        let release_params = build_modifier_event(DispatchKeyEventType::KeyUp, modifier, 0)?;
        let params = build_modifier_event(DispatchKeyEventType::KeyDown, modifier, modifier_flags)?;
        let modifier_down_result = page
            .execute(params)
            .await
            .map_err(|e| RubError::Internal(format!("keyDown for modifier failed: {e}")));
        modifier_down_result?;
        let release_guard = spawn_best_effort_key_release(
            page.clone(),
            release_params.clone(),
            "send_keys.modifier_guard",
        );
        modifier_release_guards.push(release_guard);
    }

    let mut key_down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(modifier_flags as i64);
    if let Some(text) = key_def.text {
        key_down = key_down.text(text);
    }
    let params = key_down
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyDown params failed: {e}")))?;
    let params_up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(modifier_flags as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyUp params failed: {e}")))?;
    execute_key_down_then_release_with_guard(page, params, params_up, "send_keys.main").await?;

    for (modifier, release_guard) in combo
        .modifiers
        .iter()
        .rev()
        .zip(modifier_release_guards.into_iter().rev())
    {
        let params = build_modifier_event(DispatchKeyEventType::KeyUp, modifier, 0)?;
        let release_result =
            execute_key_release_with_guard(page, params, "send_keys.modifier").await;
        if release_result.is_ok() {
            release_guard.abort();
        }
        release_result?;
    }

    Ok(())
}

pub(crate) async fn type_text(
    page: &Arc<Page>,
    text: &str,
    humanize: &HumanizeConfig,
) -> Result<(), RubError> {
    type_text_with_pre_dispatch_guard(page, text, humanize, || async { Ok(()) }).await
}

pub(crate) async fn type_text_with_pre_dispatch_guard<G, Fut>(
    page: &Arc<Page>,
    text: &str,
    humanize: &HumanizeConfig,
    mut guard: G,
) -> Result<(), RubError>
where
    G: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), RubError>>,
{
    let (delay_min, delay_max) = if humanize.enabled {
        humanize.speed.typing_delay_range()
    } else {
        (0, 0)
    };

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        guard().await?;
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            dispatch_named_key(page, "Enter").await?;
            if delay_max > 0 {
                tokio::time::sleep(Duration::from_millis(random_delay(delay_min, delay_max))).await;
            }
            continue;
        }

        if ch == '\n' {
            dispatch_named_key(page, "Enter").await?;
            if delay_max > 0 {
                tokio::time::sleep(Duration::from_millis(random_delay(delay_min, delay_max))).await;
            }
            continue;
        }

        let ch_str = ch.to_string();

        if ch.is_ascii_uppercase() {
            type_uppercase_ascii(page, ch).await?;
        } else if let Some(key_def) = crate::keys::lookup(&ch_str) {
            dispatch_key_definition(page, key_def).await?;
        } else {
            let params = DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::Char)
                .text(&ch_str)
                .build()
                .map_err(|e| RubError::Internal(format!("Build char event failed: {e}")))?;
            page.execute(params)
                .await
                .map_err(|e| RubError::Internal(format!("Char event failed: {e}")))?;
        }

        if delay_max > 0 {
            tokio::time::sleep(Duration::from_millis(random_delay(delay_min, delay_max))).await;
        }
    }

    Ok(())
}

async fn dispatch_named_key(page: &Arc<Page>, key_name: &str) -> Result<(), RubError> {
    let key_def = crate::keys::lookup(key_name)
        .ok_or_else(|| RubError::Internal(format!("Missing key definition for {key_name}")))?;
    dispatch_key_definition(page, key_def).await
}

async fn dispatch_key_definition(
    page: &Arc<Page>,
    key_def: &crate::keys::KeyDefinition,
) -> Result<(), RubError> {
    let mut key_down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64);
    if let Some(text) = key_def.text {
        key_down = key_down.text(text);
    }
    let params = key_down
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyDown failed: {e}")))?;
    let params_up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyUp failed: {e}")))?;
    execute_key_down_then_release_with_guard(page, params, params_up, "type_text.key").await?;

    Ok(())
}

async fn type_uppercase_ascii(page: &Arc<Page>, ch: char) -> Result<(), RubError> {
    let lower = ch.to_ascii_lowercase().to_string();
    let key_def = crate::keys::lookup(&lower)
        .ok_or_else(|| RubError::Internal(format!("Missing key definition for uppercase {ch}")))?;

    let shift_down = build_modifier_event(
        DispatchKeyEventType::KeyDown,
        &Modifier::Shift,
        crate::keys::modifiers::SHIFT,
    )?;
    let shift_down_result = page
        .execute(shift_down)
        .await
        .map_err(|e| RubError::Internal(format!("Shift keyDown failed: {e}")));
    shift_down_result?;
    let shift_release_guard = spawn_best_effort_key_release(
        page.clone(),
        build_modifier_event(DispatchKeyEventType::KeyUp, &Modifier::Shift, 0)?,
        "type_text.uppercase_shift_guard",
    );

    let params_down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(ch.to_string())
        .code(key_def.code)
        .text(ch.to_string())
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(crate::keys::modifiers::SHIFT as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build uppercase keyDown failed: {e}")))?;
    let params_up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(ch.to_string())
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(crate::keys::modifiers::SHIFT as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build uppercase keyUp failed: {e}")))?;
    execute_key_down_then_release_with_guard(
        page,
        params_down,
        params_up,
        "type_text.uppercase_key",
    )
    .await?;

    let shift_up = build_modifier_event(DispatchKeyEventType::KeyUp, &Modifier::Shift, 0)?;
    let shift_release_result =
        execute_key_release_with_guard(page, shift_up, "type_text.uppercase_shift").await;
    if shift_release_result.is_ok() {
        shift_release_guard.abort();
    }
    shift_release_result?;

    Ok(())
}

fn build_modifier_event(
    event_type: DispatchKeyEventType,
    modifier: &Modifier,
    modifier_flags: u32,
) -> Result<DispatchKeyEventParams, RubError> {
    let (key, code, key_code) = match modifier {
        Modifier::Control => ("Control", "ControlLeft", 17u32),
        Modifier::Shift => ("Shift", "ShiftLeft", 16),
        Modifier::Alt => ("Alt", "AltLeft", 18),
        Modifier::Meta => ("Meta", "MetaLeft", 91),
    };
    DispatchKeyEventParams::builder()
        .r#type(event_type)
        .key(key)
        .code(code)
        .windows_virtual_key_code(key_code as i64)
        .modifiers(modifier_flags as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build key event params failed: {e}")))
}

async fn execute_key_release_with_guard(
    page: &Arc<Page>,
    params: DispatchKeyEventParams,
    label: &'static str,
) -> Result<(), RubError> {
    let release_guard = spawn_best_effort_key_release(page.clone(), params.clone(), label);
    let release_result = page
        .execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("keyUp failed: {e}")));
    if release_result.is_ok() {
        release_guard.abort();
    }
    release_result.map(|_| ())
}

async fn execute_key_down_then_release_with_guard(
    page: &Arc<Page>,
    down_params: DispatchKeyEventParams,
    release_params: DispatchKeyEventParams,
    label: &'static str,
) -> Result<(), RubError> {
    let down_result = page
        .execute(down_params)
        .await
        .map_err(|e| RubError::Internal(format!("keyDown failed: {e}")));
    down_result?;
    let release_guard = spawn_best_effort_key_release(page.clone(), release_params.clone(), label);
    let release_result = page
        .execute(release_params)
        .await
        .map_err(|e| RubError::Internal(format!("keyUp failed: {e}")));
    if release_result.is_ok() {
        release_guard.abort();
    }
    release_result.map(|_| ())
}

fn spawn_best_effort_key_release(
    page: Arc<Page>,
    params: DispatchKeyEventParams,
    label: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(INPUT_RELEASE_GUARD_DELAY).await;
        if let Err(error) = page.execute(params).await {
            tracing::warn!(
                actuation = label,
                error = %error,
                "Best-effort key release failed after interrupted input transaction"
            );
        }
    })
}
