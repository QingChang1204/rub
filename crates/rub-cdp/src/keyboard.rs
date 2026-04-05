use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{KeyCombo, Modifier};
use std::sync::Arc;
use tokio::time::Duration;

use crate::humanize::{HumanizeConfig, random_delay};

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

    for modifier in &combo.modifiers {
        let params = build_modifier_event(DispatchKeyEventType::KeyDown, modifier, modifier_flags)?;
        page.execute(params)
            .await
            .map_err(|e| RubError::Internal(format!("keyDown for modifier failed: {e}")))?;
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
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("keyDown failed: {e}")))?;

    let params = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(modifier_flags as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyUp params failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("keyUp failed: {e}")))?;

    for modifier in combo.modifiers.iter().rev() {
        let params = build_modifier_event(DispatchKeyEventType::KeyUp, modifier, 0)?;
        page.execute(params)
            .await
            .map_err(|e| RubError::Internal(format!("keyUp for modifier failed: {e}")))?;
    }

    Ok(())
}

pub(crate) async fn type_text(
    page: &Arc<Page>,
    text: &str,
    humanize: &HumanizeConfig,
) -> Result<(), RubError> {
    let (delay_min, delay_max) = if humanize.enabled {
        humanize.speed.typing_delay_range()
    } else {
        (0, 0)
    };

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
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
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("keyDown failed: {e}")))?;

    let params = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key_def.key)
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build keyUp failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("keyUp failed: {e}")))?;

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
    page.execute(shift_down)
        .await
        .map_err(|e| RubError::Internal(format!("Shift keyDown failed: {e}")))?;

    let params = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(ch.to_string())
        .code(key_def.code)
        .text(ch.to_string())
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(crate::keys::modifiers::SHIFT as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build uppercase keyDown failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("Uppercase keyDown failed: {e}")))?;

    let params = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(ch.to_string())
        .code(key_def.code)
        .windows_virtual_key_code(key_def.key_code as i64)
        .modifiers(crate::keys::modifiers::SHIFT as i64)
        .build()
        .map_err(|e| RubError::Internal(format!("Build uppercase keyUp failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("Uppercase keyUp failed: {e}")))?;

    let shift_up = build_modifier_event(DispatchKeyEventType::KeyUp, &Modifier::Shift, 0)?;
    page.execute(shift_up)
        .await
        .map_err(|e| RubError::Internal(format!("Shift keyUp failed: {e}")))?;

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
