//! Data-free change hints from a worker thread to the views that read its state.
use std::sync::Mutex;

/// Coalesced hints that some state changed. A reader receives a hint, then reads the state. The
/// state is written before each hint, so a change after the read leaves a hint pending and none
/// is missed.
#[derive(Default)]
pub(crate) struct Changes {
    observers: Mutex<Vec<async_channel::Sender<()>>>,
}

impl Changes {
    /// A receiver of hints, starting with one now so its owner reads the current state.
    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        self.observe(sender);
        receiver
    }

    /// Hints `sender`, now and after each later change. Several sources may share one sender,
    /// so one reader follows them all. A closed sender is dropped at the next publication.
    pub(crate) fn observe(&self, sender: async_channel::Sender<()>) {
        let _ = sender.try_send(());
        let mut observers = self.observers.lock().unwrap_or_else(|e| e.into_inner());
        observers.retain(|observer| !observer.is_closed());
        observers.push(sender);
    }

    /// Hints every observer. Call it after the change is visible to readers.
    pub(crate) fn publish(&self) {
        self.observers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|observer| {
                !matches!(
                    observer.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                )
            });
    }
}

#[cfg(test)]
mod tests {
    use super::Changes;

    #[test]
    fn a_change_after_the_read_leaves_a_hint_pending() {
        let changes = Changes::default();
        let hints = changes.subscribe();
        assert!(
            hints.try_recv().is_ok(),
            "the first hint reads the current state"
        );
        assert!(hints.try_recv().is_err());

        changes.publish();
        changes.publish();
        assert!(hints.try_recv().is_ok(), "hints coalesce");
        // The reader read the state here; this change follows that read.
        changes.publish();
        assert!(hints.try_recv().is_ok(), "the later change is not missed");
        assert!(hints.try_recv().is_err());
    }

    #[test]
    fn one_reader_follows_several_sources() {
        let (first, second) = (Changes::default(), Changes::default());
        let (sender, hints) = async_channel::bounded(1);
        first.observe(sender.clone());
        second.observe(sender);
        while hints.try_recv().is_ok() {}

        second.publish();
        assert!(hints.try_recv().is_ok());
        drop(hints);
        first.publish();
        assert!(
            first.observers.lock().unwrap().is_empty(),
            "a closed reader is dropped"
        );
    }
}
