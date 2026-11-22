//! Fan-out of [`SessionEvent`]s to subscribed controller connections.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use crate::types::SessionEvent;

#[derive(Debug, Default)]
struct HubInner {
    next_id: u64,
    subscribers: Vec<(u64, SyncSender<SessionEvent>)>,
}

/// A cloneable handle the app uses to publish session events.
///
/// Each subscriber has a bounded queue. `publish` never blocks: a subscriber whose queue
/// is full is dropped (and its connection closed) rather than slowing the app down.
#[derive(Debug, Clone, Default)]
pub struct EventHub {
    inner: Arc<Mutex<HubInner>>,
}

impl EventHub {
    /// A hub with no subscribers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Delivers `event` to every subscriber. Never blocks.
    pub fn publish(&self, event: SessionEvent) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.subscribers.retain(|(_, tx)| match tx.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// The number of live subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).subscribers.len()
    }

    pub(crate) fn subscribe(&self, capacity: usize) -> (u64, Receiver<SessionEvent>) {
        let (tx, rx) = sync_channel(capacity);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id += 1;
        let id = inner.next_id;
        inner.subscribers.push((id, tx));
        (id, rx)
    }

    pub(crate) fn unsubscribe(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.subscribers.retain(|(sub, _)| *sub != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited(n: i32) -> SessionEvent {
        SessionEvent::Exited { session_id: "s".into(), exit_code: Some(n) }
    }

    #[test]
    fn slow_subscriber_is_dropped_not_waited_for() {
        let hub = EventHub::new();
        let (_id, rx) = hub.subscribe(2);
        hub.publish(exited(1));
        hub.publish(exited(2));
        assert_eq!(hub.subscriber_count(), 1);
        hub.publish(exited(3)); // queue full: dropped, publish returns at once
        assert_eq!(hub.subscriber_count(), 0);
        let got: Vec<_> = rx.iter().collect();
        assert_eq!(got, vec![exited(1), exited(2)]);
    }

    #[test]
    fn unsubscribe_ends_the_stream() {
        let hub = EventHub::new();
        let (id, rx) = hub.subscribe(8);
        hub.unsubscribe(id);
        assert!(rx.recv().is_err());
    }
}
