use super::{page_hook_install_poll_delay, restore_page_hook_installation_baseline};
use crate::tab_projection::PageHookInstallState;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;

#[test]
fn page_hook_install_poll_delay_uses_bounded_backoff() {
    assert_eq!(page_hook_install_poll_delay(0), Duration::from_millis(25));
    assert_eq!(page_hook_install_poll_delay(1), Duration::from_millis(50));
    assert_eq!(page_hook_install_poll_delay(2), Duration::from_millis(75));
    assert_eq!(page_hook_install_poll_delay(3), Duration::from_millis(100));
    assert_eq!(page_hook_install_poll_delay(9), Duration::from_millis(100));
}

#[tokio::test]
async fn stale_generation_restores_page_hook_installation_baseline() {
    let page_hook_states = Mutex::new(HashMap::from([(
        "tab-1".to_string(),
        PageHookInstallState {
            installing: true,
            installation_recorded: true,
            hook_bits: 0b1111,
        },
    )]));

    restore_page_hook_installation_baseline(
        "tab-1",
        PageHookInstallState {
            installing: false,
            installation_recorded: true,
            hook_bits: 0b0011,
        },
        &page_hook_states,
    )
    .await;

    let restored = page_hook_states
        .lock()
        .await
        .get("tab-1")
        .cloned()
        .expect("baseline should be restored");
    assert!(!restored.installing);
    assert_eq!(restored.hook_bits, 0b0011);
    assert!(restored.installation_recorded);
}
