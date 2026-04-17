use rub_core::model::{
    InteractionConfirmation, InteractionConfirmationKind, InteractionConfirmationStatus,
};
use serde_json::Value;
use tokio::time::{Duration, sleep};

pub(super) const OBSERVATION_WINDOW: Duration = Duration::from_millis(1_500);
pub(super) const OBSERVATION_INTERVAL: Duration = Duration::from_millis(25);
const OBSERVATION_BACKOFF_STEP: Duration = Duration::from_millis(25);
pub(super) const OBSERVATION_BACKOFF_CEILING: Duration = Duration::from_millis(100);

pub(super) fn observation_poll_delay(poll_count: u32) -> Duration {
    let delay =
        OBSERVATION_INTERVAL.saturating_add(OBSERVATION_BACKOFF_STEP.saturating_mul(poll_count));
    delay.min(OBSERVATION_BACKOFF_CEILING)
}

pub(super) async fn sleep_observation_step(poll_count: &mut u32) {
    sleep(observation_poll_delay(*poll_count)).await;
    *poll_count = poll_count.saturating_add(1);
}

pub(super) fn confirmed(
    kind: InteractionConfirmationKind,
    details: Value,
) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Confirmed,
        kind: Some(kind),
        details: Some(details),
    }
}

pub(super) fn unconfirmed(details: Value) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Unconfirmed,
        kind: None,
        details: Some(details),
    }
}

pub(super) fn contradicted(
    kind: InteractionConfirmationKind,
    details: Value,
) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Contradicted,
        kind: Some(kind),
        details: Some(details),
    }
}

pub(super) fn degraded(
    kind: Option<InteractionConfirmationKind>,
    details: Value,
) -> InteractionConfirmation {
    InteractionConfirmation {
        status: InteractionConfirmationStatus::Degraded,
        kind,
        details: Some(details),
    }
}
