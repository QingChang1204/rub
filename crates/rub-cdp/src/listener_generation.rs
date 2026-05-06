use futures::{Stream, StreamExt};
use tokio::sync::watch;

pub(crate) type ListenerGeneration = u64;
pub(crate) type ListenerGenerationTx = watch::Sender<ListenerGeneration>;
pub(crate) type ListenerGenerationRx = watch::Receiver<ListenerGeneration>;

pub(crate) fn new_listener_generation_channel() -> (ListenerGenerationTx, ListenerGenerationRx) {
    watch::channel(0)
}

pub(crate) fn is_current_generation(
    receiver: &ListenerGenerationRx,
    generation: ListenerGeneration,
) -> bool {
    *receiver.borrow() == generation
}

pub(crate) async fn next_listener_event<S, T>(
    listener: &mut S,
    generation: ListenerGeneration,
    generation_rx: &mut ListenerGenerationRx,
) -> Option<T>
where
    S: Stream<Item = T> + Unpin,
{
    enum NextEvent<T> {
        Listener(Option<T>),
        CurrentGenerationChange,
    }

    loop {
        if !is_current_generation(generation_rx, generation) {
            return None;
        }

        let event = tokio::select! {
            event = listener.next() => NextEvent::Listener(event),
            changed = generation_rx.changed() => {
                match changed {
                    Ok(()) if is_current_generation(generation_rx, generation) => NextEvent::CurrentGenerationChange,
                    Ok(()) | Err(_) => return None,
                }
            }
        };

        match event {
            NextEvent::CurrentGenerationChange => continue,
            NextEvent::Listener(event) => {
                if !is_current_generation(generation_rx, generation) {
                    return None;
                }
                return event;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_current_generation, new_listener_generation_channel, next_listener_event};
    use futures::stream;
    use std::time::Duration;

    #[test]
    fn listener_generation_channel_advances_monotonically() {
        let (tx, rx) = new_listener_generation_channel();
        assert!(is_current_generation(&rx, 0));

        tx.send(1).expect("generation update should succeed");

        assert!(is_current_generation(&rx, 1));
        assert!(!is_current_generation(&rx, 0));
    }

    #[tokio::test]
    async fn stale_event_is_dropped_after_generation_bump() {
        let (tx, mut rx) = new_listener_generation_channel();
        let mut stream = stream::poll_fn(move |_| {
            tx.send(1).expect("generation update should succeed");
            std::task::Poll::Ready(Some("stale"))
        });

        let event = next_listener_event(&mut stream, 0, &mut rx).await;
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn pending_listener_exits_when_generation_bumps_after_unconsumed_current_change() {
        let (tx, mut rx) = new_listener_generation_channel();
        tx.send(1).expect("generation update should succeed");
        let mut stream = stream::pending::<&'static str>();
        let bump = tokio::spawn(async move {
            tokio::task::yield_now().await;
            tx.send(2).expect("generation update should succeed");
        });

        let result = tokio::time::timeout(Duration::from_millis(100), async {
            next_listener_event(&mut stream, 1, &mut rx).await
        })
        .await
        .expect("stale listener should not wait for a browser event");
        bump.await.expect("generation bump task should finish");

        assert!(result.is_none());
    }
}
