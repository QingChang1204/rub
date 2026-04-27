use crate::commands::{Commands, EffectiveCli};
use crate::main_support::command_timeout_error;
use rub_cdp::humanize::HumanizeSpeed;
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::protocol::MAX_IPC_TIMEOUT_MS;
use serde_json::Value;
use std::future::Future;
use std::time::{Duration, Instant};

use super::helpers::wait_after_is_configured;

pub(crate) const WAIT_IPC_BUFFER_MS: u64 = 5_000;

pub(crate) fn command_timeout_ms(cli: &EffectiveCli) -> u64 {
    match &cli.command {
        Commands::Wait { timeout, .. } => timeout.saturating_add(WAIT_IPC_BUFFER_MS),
        _ => cli
            .timeout
            .saturating_add(humanize_budget_ms(cli))
            .saturating_add(wait_after_budget_ms(cli)),
    }
}

pub(crate) fn deadline_from_start(started_at: Instant, timeout_ms: u64) -> Instant {
    deadline_from_start_checked(started_at, timeout_ms).unwrap_or(started_at)
}

pub(crate) fn deadline_from_start_checked(started_at: Instant, timeout_ms: u64) -> Option<Instant> {
    started_at.checked_add(Duration::from_millis(timeout_ms.max(1)))
}

pub(crate) fn validate_timeout_budget(timeout_ms: u64) -> Result<(), RubError> {
    if timeout_ms == 0 || timeout_ms > MAX_IPC_TIMEOUT_MS {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!("IPC request timeout_ms must be between 1 and {MAX_IPC_TIMEOUT_MS}"),
            serde_json::json!({
                "reason": "invalid_ipc_request_contract",
                "field": "timeout_ms",
                "max_timeout_ms": MAX_IPC_TIMEOUT_MS,
                "actual_timeout_ms": timeout_ms,
            }),
        ));
    }
    Ok(())
}

pub(crate) fn remaining_budget_duration(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

pub(crate) fn ensure_remaining_budget(
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<(), RubError> {
    if remaining_budget_duration(deadline).is_none() {
        return Err(command_timeout_error(timeout_ms, phase));
    }
    Ok(())
}

pub(crate) async fn run_with_remaining_budget<T, Fut>(
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
    future: Fut,
) -> Result<T, RubError>
where
    Fut: Future<Output = Result<T, RubError>>,
{
    let remaining = remaining_budget_duration(deadline)
        .ok_or_else(|| command_timeout_error(timeout_ms, phase))?;
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| command_timeout_error(timeout_ms, phase))?
}

fn humanize_budget_ms(cli: &EffectiveCli) -> u64 {
    if !cli.humanize {
        return 0;
    }

    match &cli.command {
        Commands::Click { double, .. } => humanize_budget_ms_for_command_args(
            "click",
            &serde_json::json!({
                "gesture": if *double { Some("double") } else { None },
            }),
            cli.humanize,
            &cli.humanize_speed,
        ),
        Commands::Hover { .. } => humanize_budget_ms_for_command_args(
            "hover",
            &serde_json::json!({}),
            cli.humanize,
            &cli.humanize_speed,
        ),
        Commands::Scroll { amount, y, .. } => humanize_budget_ms_for_command_args(
            "scroll",
            &serde_json::json!({
                "amount": amount,
                "y": y,
            }),
            cli.humanize,
            &cli.humanize_speed,
        ),
        Commands::Type {
            text, text_flag, ..
        } => humanize_budget_ms_for_command_args(
            "type",
            &serde_json::json!({
                "text": text.as_deref().or(text_flag.as_deref()),
            }),
            cli.humanize,
            &cli.humanize_speed,
        ),
        _ => 0,
    }
}

fn wait_after_budget_ms(cli: &EffectiveCli) -> u64 {
    cli.command
        .wait_after_args()
        .filter(|wait_after| wait_after_is_configured(wait_after))
        .map(|wait_after| wait_after_timeout_ms(wait_after.timeout_ms))
        .unwrap_or(0)
}

pub(crate) fn wait_after_timeout_ms(timeout_ms: Option<u64>) -> u64 {
    rub_core::automation_timeout::wait_after_timeout_ms(timeout_ms)
}

pub(crate) fn humanize_budget_ms_for_command_args(
    command: &str,
    args: &Value,
    humanize: bool,
    humanize_speed: &str,
) -> u64 {
    if !humanize {
        return 0;
    }

    let speed = HumanizeSpeed::from_str_opt(humanize_speed).unwrap_or_default();
    match command {
        "click" => {
            if args.get("gesture").and_then(Value::as_str) == Some("double") {
                speed.mouse_move_duration().saturating_add(800)
            } else {
                speed.mouse_move_duration().saturating_add(500)
            }
        }
        "hover" => speed.mouse_move_duration().saturating_add(500),
        "scroll" => {
            let pixels = args
                .get("amount")
                .and_then(Value::as_u64)
                .or_else(|| args.get("y").and_then(Value::as_i64).map(i64::unsigned_abs))
                .unwrap_or(600);
            let steps = (pixels / 80).max(1);
            let (_, scroll_delay_max) = speed.scroll_delay_range();
            steps.saturating_mul(scroll_delay_max)
        }
        "type" => {
            let text_len = args
                .get("text")
                .and_then(Value::as_str)
                .map(|text| text.chars().count() as u64)
                .unwrap_or(0);
            let (_, typing_delay_max) = speed.typing_delay_range();
            let focus_pause_max = typing_delay_max.max(50);
            focus_pause_max.saturating_add(text_len.saturating_mul(typing_delay_max))
        }
        _ => 0,
    }
}
