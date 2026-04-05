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
    if !is_current_generation(generation_rx, generation) {
        return None;
    }

    let event = tokio::select! {
        event = listener.next() => event,
        changed = generation_rx.changed() => {
            match changed {
                Ok(()) if is_current_generation(generation_rx, generation) => listener.next().await,
                Ok(()) | Err(_) => None,
            }
        }
    };

    if !is_current_generation(generation_rx, generation) {
        return None;
    }

    event
}

#[cfg(test)]
mod tests {
    use super::{is_current_generation, new_listener_generation_channel, next_listener_event};
    use futures::stream;

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
}
