//! Operating-System appearance facts remain independent of forced application presentation.

use crate::appearance::Appearance;

/// Keeps the native observation alive until its application owner is destroyed.
pub(crate) trait SystemAppearanceSubscription {}

pub(crate) struct SystemAppearanceObservation {
    pub(crate) changed: async_channel::Receiver<()>,
    pub(crate) subscription: Box<dyn SystemAppearanceSubscription>,
}

/// Selected at startup; only this Adapter queries or changes native appearance.
pub(crate) trait AppearancePlatform {
    fn system_appearance(&self) -> Option<Appearance>;
    fn observe(&self) -> Option<SystemAppearanceObservation>;
    fn apply_native_appearance(&self, appearance: Appearance);
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    pub(crate) struct RecordingAppearancePlatform {
        fact: Rc<Cell<Option<Appearance>>>,
        notifications: Rc<RefCell<Vec<async_channel::Sender<()>>>>,
        pub(crate) applied: Rc<RefCell<Vec<Appearance>>>,
    }

    impl RecordingAppearancePlatform {
        pub(crate) fn set_system_appearance(&self, appearance: Option<Appearance>) {
            self.fact.set(appearance);
            self.notifications.borrow_mut().retain(|sender| {
                !matches!(
                    sender.try_send(()),
                    Err(async_channel::TrySendError::Closed(_))
                )
            });
        }
    }

    struct RecordingSubscription;
    impl SystemAppearanceSubscription for RecordingSubscription {}

    impl AppearancePlatform for RecordingAppearancePlatform {
        fn system_appearance(&self) -> Option<Appearance> {
            self.fact.get()
        }

        fn observe(&self) -> Option<SystemAppearanceObservation> {
            let (sender, changed) = async_channel::bounded(1);
            self.notifications.borrow_mut().push(sender);
            Some(SystemAppearanceObservation {
                changed,
                subscription: Box::new(RecordingSubscription),
            })
        }

        fn apply_native_appearance(&self, appearance: Appearance) {
            self.applied.borrow_mut().push(appearance);
        }
    }
}
