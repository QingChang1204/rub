use super::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

fn test_router() -> Arc<DaemonRouter> {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        },
    ));
    let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        rub_cdp::humanize::HumanizeConfig {
            enabled: false,
            speed: rub_cdp::humanize::HumanizeSpeed::Normal,
        },
    ));
    Arc::new(DaemonRouter::new(adapter))
}

fn temp_home(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rub-queue-{label}-{}", Uuid::now_v7()))
}

#[tokio::test]
async fn automation_transactions_share_fifo_authority_with_default_budget() {
    let router = test_router();
    let state = Arc::new(SessionState::new("default", temp_home("fairness"), None));
    let foreground_hold = Duration::from_millis(
        AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS
            .saturating_mul(3)
            .min(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_sub(50)),
    );

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let queued = router.begin_automation_transaction_with_wait_budget(
        &state,
        "queued_automation",
        AUTOMATION_QUEUE_WAIT_BUDGET,
        Duration::from_millis(5),
    );
    tokio::pin!(queued);

    tokio::select! {
        _ = tokio::time::sleep(foreground_hold) => {}
        _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
    }
    drop(held);

    let guard = queued
        .await
        .expect("queued automation should acquire after the first transaction releases");
    drop(guard);
}

#[tokio::test]
async fn queued_automation_is_rejected_after_shutdown_request() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("shutdown-fence"),
        None,
    ));
    let foreground_hold = Duration::from_millis(
        AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS
            .saturating_mul(3)
            .min(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_sub(50)),
    );

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let queued = router.begin_automation_transaction_with_wait_budget(
        &state,
        "queued_automation",
        AUTOMATION_QUEUE_WAIT_BUDGET,
        Duration::from_millis(5),
    );
    tokio::pin!(queued);

    tokio::select! {
        _ = tokio::time::sleep(foreground_hold) => {}
        _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
    }
    state.request_shutdown();
    drop(held);

    let error = match queued.await {
        Ok(_) => panic!("queued automation should be fenced out during shutdown"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error.context,
        Some(serde_json::json!({
            "command": "queued_automation",
            "reason": "session_shutting_down_after_queue_wait",
        }))
    );
}

#[tokio::test]
async fn automation_transaction_returns_timeout_once_wait_budget_expires() {
    let router = test_router();
    let state = Arc::new(SessionState::new("default", temp_home("wait-budget"), None));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let error = match router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "queued_automation",
            Duration::from_millis(20),
            Duration::from_millis(5),
        )
        .await
    {
        Ok(_) => panic!("queue wait should time out once the worker budget expires"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::IpcTimeout);
    assert_eq!(
        error.context,
        Some(serde_json::json!({
            "command": "queued_automation",
            "reason": "automation_queue_wait_budget_exceeded",
            "wait_budget_ms": 20,
        }))
    );
    drop(held);
}

#[tokio::test(start_paused = true)]
async fn automation_transaction_wait_budget_is_a_hard_upper_bound() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("wait-budget-hard-cap"),
        None,
    ));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let queued = tokio::spawn({
        let router = router.clone();
        let state = state.clone();
        async move {
            match router
                .begin_automation_transaction_with_wait_budget(
                    &state,
                    "queued_automation",
                    Duration::from_millis(20),
                    Duration::from_millis(50),
                )
                .await
            {
                Ok(guard) => {
                    drop(guard);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(20)).await;
    tokio::task::yield_now().await;

    assert!(
        queued.is_finished(),
        "queue wait budget should expire at the deadline instead of overshooting to the next poll interval"
    );

    let error = match queued.await.expect("queued task should complete") {
        Ok(_) => panic!("queue wait should time out once the deadline is reached"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::IpcTimeout);
    drop(held);
}

#[tokio::test]
async fn active_automation_transactions_can_wait_longer_than_worker_cycle_budget() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("active-step-budget"),
        None,
    ));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let queued = router.begin_automation_transaction_with_wait_budget(
        &state,
        "orchestration_source_materialization",
        Duration::from_millis(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_add(250)),
        Duration::from_millis(5),
    );
    tokio::pin!(queued);

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_add(25))) => {}
        _ = &mut queued => panic!("active-step reservation should keep waiting past the worker fairness budget"),
    }
    drop(held);

    let guard = queued
        .await
        .expect("active-step reservation should still acquire once the foreground slot releases");
    drop(guard);
}

#[tokio::test]
async fn waiting_automation_acquires_when_foreground_releases_within_wait_budget() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("within-budget"),
        None,
    ));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let queued = router.begin_automation_transaction_with_wait_budget(
        &state,
        "queued_automation",
        AUTOMATION_QUEUE_WAIT_BUDGET,
        Duration::from_millis(5),
    );
    tokio::pin!(queued);

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(30)) => {}
        _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
    }
    drop(held);

    let guard = queued.await.expect(
        "queued automation should eventually acquire once the foreground transaction releases",
    );
    drop(guard);
}

#[tokio::test]
async fn queued_automation_keeps_fifo_priority_over_later_foreground_arrivals() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("fifo-priority"),
        None,
    ));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let (order_tx, mut order_rx) = mpsc::unbounded_channel();
    let (release_automation_tx, release_automation_rx) = oneshot::channel();
    let (release_foreground_tx, release_foreground_rx) = oneshot::channel();

    let automation_router = router.clone();
    let automation_state = state.clone();
    let automation_order_tx = order_tx.clone();
    let automation_task = tokio::spawn(async move {
        let guard = automation_router
            .begin_automation_transaction_with_wait_budget(
                &automation_state,
                "queued_automation",
                AUTOMATION_QUEUE_WAIT_BUDGET,
                Duration::from_millis(5),
            )
            .await
            .expect("queued automation should eventually acquire");
        automation_order_tx
            .send("automation")
            .expect("automation acquisition order should send");
        let _ = release_automation_rx.await;
        drop(guard);
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    let foreground_router = router.clone();
    let foreground_state = state.clone();
    let foreground_order_tx = order_tx.clone();
    let foreground_task = tokio::spawn(async move {
        let guard = foreground_router
            .begin_request_transaction(
                "later_foreground",
                "req-later-foreground",
                TransactionDeadline::new(1_000),
                &foreground_state,
            )
            .await
            .expect("later foreground request should eventually acquire");
        foreground_order_tx
            .send("foreground")
            .expect("foreground acquisition order should send");
        let _ = release_foreground_rx.await;
        drop(guard);
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(5), order_rx.recv())
            .await
            .is_err(),
        "no waiter should acquire while the initial guard is still held"
    );

    drop(held);

    let first = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
        .await
        .expect("first queued waiter should acquire after the held guard releases")
        .expect("first queued waiter label should be present");
    assert_eq!(first, "automation");

    release_automation_tx
        .send(())
        .expect("automation release signal should send");

    let second = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
        .await
        .expect("second queued waiter should acquire after automation releases")
        .expect("second queued waiter label should be present");
    assert_eq!(second, "foreground");

    release_foreground_tx
        .send(())
        .expect("foreground release signal should send");

    automation_task
        .await
        .expect("automation task should complete cleanly");
    foreground_task
        .await
        .expect("foreground task should complete cleanly");
}

#[tokio::test]
async fn bounded_automation_reservation_yields_fifo_priority_after_worker_cycle_budget() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        temp_home("persistent-fifo-priority"),
        None,
    ));

    let held = router
        .begin_automation_transaction_with_wait_budget(
            &state,
            "hold_foreground_slot",
            Duration::from_millis(5),
            Duration::from_millis(5),
        )
        .await
        .expect("first automation transaction should acquire immediately");

    let (order_tx, mut order_rx) = mpsc::unbounded_channel();
    let (release_foreground_tx, release_foreground_rx) = oneshot::channel();

    let automation_router = router.clone();
    let automation_state = state.clone();
    let automation_task = tokio::spawn(async move {
        automation_router
            .begin_automation_reservation_transaction_owned(&automation_state, "queued_automation")
            .await
    });

    let automation_error = automation_task
        .await
        .expect("bounded automation reservation task should complete");
    let automation_error = match automation_error {
        Ok(_) => panic!("automation reservation should yield once its worker-cycle budget expires"),
        Err(error) => error,
    };
    assert_eq!(automation_error.code, ErrorCode::IpcTimeout);
    assert_eq!(
        automation_error.context,
        Some(serde_json::json!({
            "command": "queued_automation",
            "reason": "automation_queue_wait_budget_exceeded",
            "wait_budget_ms": AUTOMATION_QUEUE_WAIT_BUDGET_MS,
        }))
    );

    let foreground_router = router.clone();
    let foreground_state = state.clone();
    let foreground_order_tx = order_tx.clone();
    let foreground_task = tokio::spawn(async move {
        let guard = foreground_router
            .begin_request_transaction(
                "later_foreground",
                "req-later-foreground",
                TransactionDeadline::new(1_000),
                &foreground_state,
            )
            .await
            .expect("later foreground request should eventually acquire");
        foreground_order_tx
            .send("foreground")
            .expect("foreground acquisition order should send");
        let _ = release_foreground_rx.await;
        drop(guard);
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(5), order_rx.recv())
            .await
            .is_err(),
        "persistent automation contender should still be queued while the foreground hold remains active"
    );

    drop(held);

    let first = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
        .await
        .expect("first queued waiter should acquire after the held guard releases")
        .expect("first queued waiter label should be present");
    assert_eq!(first, "foreground");

    release_foreground_tx
        .send(())
        .expect("foreground release signal should send");

    foreground_task
        .await
        .expect("foreground task should complete cleanly");
}
