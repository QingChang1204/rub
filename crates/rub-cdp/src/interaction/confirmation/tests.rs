use super::support::{OBSERVATION_BACKOFF_CEILING, OBSERVATION_INTERVAL, observation_poll_delay};
use std::time::Duration;

#[test]
fn observation_poll_delay_uses_bounded_backoff() {
    assert_eq!(observation_poll_delay(0), OBSERVATION_INTERVAL);
    assert_eq!(observation_poll_delay(1), Duration::from_millis(50));
    assert_eq!(observation_poll_delay(2), Duration::from_millis(75));
    assert_eq!(observation_poll_delay(3), OBSERVATION_BACKOFF_CEILING);
    assert_eq!(observation_poll_delay(8), OBSERVATION_BACKOFF_CEILING);
}
