//! Application-selected Terminal Accessibility construction and publication boundary.
use crate::terminal::{
    AccessibilityNotifications, AccessibilitySelectionSender, TerminalAccessibilityModel,
};
use gpui::{Bounds, Pixels, Window};

/// Owns one Pane's native accessibility resources. Dropping it retires those resources.
pub(crate) trait TerminalAccessibilityAdapter {
    /// Updates layout membership synchronously. Hidden elements must reject Selection requests.
    fn set_hierarchy(&mut self, presented: bool, order: usize);
    /// Publishes current facts and returns notifications that could not yet be delivered.
    fn update(&mut self, update: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications;
}

/// Selected by application composition and invoked once per Pane.
pub(crate) trait TerminalAccessibilityAdapterFactory {
    fn create(
        &self,
        window: &Window,
        model: TerminalAccessibilityModel,
        font_family: &str,
        font_size: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter>;
}

pub(crate) struct TerminalAccessibilityUpdate<'a> {
    pub(crate) window: &'a Window,
    pub(crate) model: &'a TerminalAccessibilityModel,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) cell_width: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) font_family: &'a str,
    pub(crate) font_size: Pixels,
    pub(crate) focused: bool,
    pub(crate) notifications: AccessibilityNotifications,
    pub(crate) selection_sender: Option<AccessibilitySelectionSender>,
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::terminal::AccessibilityNotification;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Default)]
    pub(crate) struct RecordingAccessibilityFactory {
        pub(crate) records: Rc<RefCell<Vec<Rc<RefCell<AccessibilityRecord>>>>>,
    }

    pub(crate) struct AccessibilityRecord {
        pub(crate) model: TerminalAccessibilityModel,
        pub(crate) font_family: String,
        pub(crate) font_size: Pixels,
        pub(crate) bounds: Option<Bounds<Pixels>>,
        pub(crate) cell_width: Pixels,
        pub(crate) line_height: Pixels,
        pub(crate) hierarchy: Vec<(bool, usize)>,
        pub(crate) presented: bool,
        pub(crate) visible: bool,
        pub(crate) focused: bool,
        pub(crate) delivered: AccessibilityNotifications,
        pub(crate) selection_sender: Option<AccessibilitySelectionSender>,
        pub(crate) dropped: bool,
    }

    impl TerminalAccessibilityAdapterFactory for RecordingAccessibilityFactory {
        fn create(
            &self,
            _: &Window,
            model: TerminalAccessibilityModel,
            font_family: &str,
            font_size: Pixels,
        ) -> Box<dyn TerminalAccessibilityAdapter> {
            let record = Rc::new(RefCell::new(AccessibilityRecord {
                model,
                font_family: font_family.to_owned(),
                font_size,
                bounds: None,
                cell_width: gpui::px(1.0),
                line_height: gpui::px(1.0),
                hierarchy: Vec::new(),
                presented: false,
                visible: false,
                focused: false,
                delivered: AccessibilityNotifications::default(),
                selection_sender: None,
                dropped: false,
            }));
            self.records.borrow_mut().push(Rc::clone(&record));
            Box::new(RecordingAccessibilityAdapter(record))
        }
    }

    struct RecordingAccessibilityAdapter(Rc<RefCell<AccessibilityRecord>>);

    impl TerminalAccessibilityAdapter for RecordingAccessibilityAdapter {
        fn set_hierarchy(&mut self, presented: bool, order: usize) {
            let mut record = self.0.borrow_mut();
            record.hierarchy.push((presented, order));
            record.presented = presented;
            record.visible &= presented;
            record.focused &= presented;
            if !presented {
                record.selection_sender = None;
            }
        }

        fn update(
            &mut self,
            update: TerminalAccessibilityUpdate<'_>,
        ) -> AccessibilityNotifications {
            let _ = update.window;
            let mut record = self.0.borrow_mut();
            let was_focused = record.focused;
            record.model = update.model.clone();
            record.font_family = update.font_family.to_owned();
            record.font_size = update.font_size;
            record.bounds = update.bounds;
            record.cell_width = update.cell_width;
            record.line_height = update.line_height;
            record.selection_sender = update.selection_sender;
            record.visible = record.presented && update.bounds.is_some();
            record.focused = record.visible && update.focused;
            record.delivered = AccessibilityNotifications::default();
            if !record.visible {
                return update.notifications;
            }
            record.delivered = update
                .notifications
                .without(AccessibilityNotification::Focus);
            if record.focused
                && (!was_focused
                    || update
                        .notifications
                        .contains(AccessibilityNotification::Focus))
            {
                record.delivered.insert(AccessibilityNotification::Focus);
            }
            AccessibilityNotifications::default()
        }
    }

    impl Drop for RecordingAccessibilityAdapter {
        fn drop(&mut self) {
            let mut record = self.0.borrow_mut();
            record.dropped = true;
            record.presented = false;
            record.visible = false;
            record.focused = false;
            record.selection_sender = None;
        }
    }
}
