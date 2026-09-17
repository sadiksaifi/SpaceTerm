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
    /// Whether the user asks application motion to be reduced.
    fn prefers_reduced_motion(&self) -> bool {
        false
    }
    /// Includes native support and the user's accessibility display preferences.
    fn supports_transparency(&self) -> bool {
        false
    }
    /// Installs or removes the native backdrop behind one Operating-System Window's content.
    ///
    /// The window owner calls this once per effective composition change, including the change
    /// back to an opaque window, so no window keeps a backdrop it no longer presents.
    fn apply_window_backdrop(&self, window: &gpui::Window, blurred: bool) {
        let _ = (window, blurred);
    }
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
        reduced_motion: Rc<Cell<bool>>,
        transparency: Rc<Cell<bool>>,
        pub(crate) backdrops: Rc<RefCell<Vec<bool>>>,
        notifications: Rc<RefCell<Vec<async_channel::Sender<()>>>>,
        pub(crate) applied: Rc<RefCell<Vec<Appearance>>>,
    }

    impl RecordingAppearancePlatform {
        pub(crate) fn set_transparency_supported(&self, supported: bool) {
            self.transparency.set(supported);
            self.set_system_appearance(self.fact.get());
        }
        pub(crate) fn set_reduced_motion(&self, reduced: bool) {
            self.reduced_motion.set(reduced);
            self.set_system_appearance(self.fact.get());
        }
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
        fn prefers_reduced_motion(&self) -> bool {
            self.reduced_motion.get()
        }
        fn supports_transparency(&self) -> bool {
            self.transparency.get()
        }
        fn apply_window_backdrop(&self, _: &gpui::Window, blurred: bool) {
            self.backdrops.borrow_mut().push(blurred);
        }
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
