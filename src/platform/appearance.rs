//! Operating-System appearance facts remain independent of forced application presentation.

use crate::appearance::Appearance;

/// Keeps the native observation alive until its application owner is destroyed.
pub(crate) trait SystemAppearanceSubscription {}

pub(crate) struct SystemAppearanceObservation {
    pub(crate) changed: async_channel::Receiver<()>,
    pub(crate) subscription: Box<dyn SystemAppearanceSubscription>,
}

/// Accessibility display choices supplied independently of retained appearance Settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccessibilityDisplayOptions {
    pub(crate) reduce_transparency: bool,
    pub(crate) increase_contrast: bool,
    pub(crate) show_borders: bool,
    pub(crate) differentiate_without_color: bool,
}

/// Selected at startup; only this Adapter queries or changes native appearance.
pub(crate) trait AppearancePlatform {
    fn system_appearance(&self) -> Option<Appearance>;
    /// Whether the user asks application motion to be reduced.
    fn prefers_reduced_motion(&self) -> bool {
        false
    }
    /// Whether this Adapter can present a translucent native Operating-System Window.
    fn supports_native_window_transparency(&self) -> bool {
        false
    }
    /// Accessibility display choices apply to both native and in-window materials.
    fn accessibility_display_options(&self) -> AccessibilityDisplayOptions {
        AccessibilityDisplayOptions::default()
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
        native_window_transparency: Rc<Cell<bool>>,
        accessibility: Rc<Cell<AccessibilityDisplayOptions>>,
        pub(crate) backdrops: Rc<RefCell<Vec<bool>>>,
        notifications: Rc<RefCell<Vec<async_channel::Sender<()>>>>,
        pub(crate) applied: Rc<RefCell<Vec<Appearance>>>,
    }

    impl RecordingAppearancePlatform {
        pub(crate) fn set_native_window_transparency_supported(&self, supported: bool) {
            self.native_window_transparency.set(supported);
            self.set_system_appearance(self.fact.get());
        }
        pub(crate) fn set_reduce_transparency(&self, reduced: bool) {
            let mut options = self.accessibility.get();
            options.reduce_transparency = reduced;
            self.accessibility.set(options);
            self.set_system_appearance(self.fact.get());
        }
        pub(crate) fn set_increase_contrast(&self, increased: bool) {
            let mut options = self.accessibility.get();
            options.increase_contrast = increased;
            self.accessibility.set(options);
            self.set_system_appearance(self.fact.get());
        }
        pub(crate) fn set_show_borders(&self, shown: bool) {
            let mut options = self.accessibility.get();
            options.show_borders = shown;
            self.accessibility.set(options);
            self.set_system_appearance(self.fact.get());
        }
        pub(crate) fn set_differentiate_without_color(&self, differentiate: bool) {
            let mut options = self.accessibility.get();
            options.differentiate_without_color = differentiate;
            self.accessibility.set(options);
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
        fn supports_native_window_transparency(&self) -> bool {
            self.native_window_transparency.get()
        }
        fn accessibility_display_options(&self) -> AccessibilityDisplayOptions {
            self.accessibility.get()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_material_accessibility_facts_do_not_suppress_transparency() {
        let platform = testing::RecordingAppearancePlatform::default();
        platform.set_show_borders(true);
        platform.set_differentiate_without_color(true);
        let options = platform.accessibility_display_options();

        assert!(options.show_borders);
        assert!(options.differentiate_without_color);
        assert!(!options.reduce_transparency);
        assert!(!options.increase_contrast);
    }
}
