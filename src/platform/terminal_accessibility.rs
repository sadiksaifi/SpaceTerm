//! Application-selected Terminal Accessibility construction and publication boundary.
use crate::terminal::{
    AccessibilityDemandSender, AccessibilityNotifications, AccessibilitySelectionSender,
    TerminalAccessibilityModel,
};
use gpui::{Bounds, Div, Pixels, Stateful, Window};
use spaceterm_client::{pane_channel::EffectPermit, projection::FrameLease};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Authority for exactly the document handed to a native accessibility adapter.
/// Native reads and callbacks validate it again after resolving their result.
#[derive(Clone)]
pub(crate) struct AccessibilityPublication(Arc<PublicationAuthority>);

struct PublicationAuthority {
    permit: EffectPermit,
    frame: FrameLease,
    generation: crate::terminal::PresentationGeneration,
    revoked: AtomicBool,
}

impl AccessibilityPublication {
    pub(crate) fn new(
        permit: EffectPermit,
        frame: FrameLease,
        generation: crate::terminal::PresentationGeneration,
    ) -> Self {
        Self(Arc::new(PublicationAuthority {
            permit,
            frame,
            generation,
            revoked: AtomicBool::new(false),
        }))
    }

    pub(crate) fn is_current(&self) -> bool {
        !self.0.revoked.load(Ordering::Acquire)
            && self.0.permit.is_current()
            && self.0.frame.permits_native_content_use()
    }

    pub(crate) fn permits(&self, model: &TerminalAccessibilityModel) -> bool {
        model.generation() == self.0.generation && self.is_current()
    }

    pub(crate) fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
    }
}

impl std::fmt::Debug for AccessibilityPublication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccessibilityPublication")
    }
}

pub(crate) fn empty_accessibility_model() -> TerminalAccessibilityModel {
    TerminalAccessibilityModel::empty()
}

/// Owns one Pane's native accessibility resources. Dropping it retires those resources.
pub(crate) trait TerminalAccessibilityAdapter {
    /// True only when retained native elements validate authority at the final text read.
    /// Publication-time validation alone cannot revoke a platform's cached text nodes.
    fn supports_generation_bound_content(&self) -> bool {
        false
    }
    /// Updates layout membership synchronously. Hidden elements must reject Selection requests.
    fn set_hierarchy(&mut self, presented: bool);
    /// Publishes current facts and returns notifications that could not yet be delivered.
    fn update(&mut self, update: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications;
    /// Describes this Pane inside the Window's portable accessibility tree.
    /// Adapters whose native elements GPUI does not build attach them to this element's node.
    fn decorate(&self, pane: Stateful<Div>) -> Stateful<Div> {
        pane
    }
    /// Describes the element that presents this Pane's terminal text, inside the Pane's element.
    /// Its descendants publish no accessibility nodes, so adapters may replace its children.
    fn decorate_text(&self, text: Stateful<Div>) -> Stateful<Div> {
        text
    }
}

/// Selected by application composition and invoked once per Pane.
pub(crate) trait TerminalAccessibilityAdapterFactory {
    fn create(
        &self,
        window: &Window,
        model: TerminalAccessibilityModel,
        font: &crate::appearance::ResolvedFontDescriptor,
        font_size: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter>;
}

pub(crate) struct TerminalAccessibilityUpdate<'a> {
    pub(crate) window: &'a Window,
    pub(crate) model: &'a TerminalAccessibilityModel,
    pub(crate) publication: Option<AccessibilityPublication>,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) cell_width: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) font: &'a crate::appearance::ResolvedFontDescriptor,
    pub(crate) font_size: Pixels,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "portable accessibility uses GPUI's tracked focus handle"
        )
    )]
    pub(crate) focused: bool,
    pub(crate) notifications: AccessibilityNotifications,
    pub(crate) selection_sender: Option<AccessibilitySelectionSender>,
    pub(crate) demand_sender: Option<AccessibilityDemandSender>,
    pub(crate) focus_sender: Option<AccessibilityFocusSender>,
}

/// Asks one Pane to take focus on behalf of an accessibility client. Requests carry no content
/// and coalesce until the Pane handles them.
#[derive(Clone, Debug)]
pub(crate) struct AccessibilityFocusSender {
    sender: async_channel::Sender<Option<AccessibilityPublication>>,
    publication: Option<AccessibilityPublication>,
}

impl AccessibilityFocusSender {
    pub(crate) fn channel() -> (
        Self,
        async_channel::Receiver<Option<AccessibilityPublication>>,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        (
            Self {
                sender,
                publication: None,
            },
            receiver,
        )
    }

    pub(crate) fn bound(&self, publication: AccessibilityPublication) -> Self {
        Self {
            sender: self.sender.clone(),
            publication: Some(publication),
        }
    }

    pub(crate) fn request(&self) {
        if self
            .publication
            .as_ref()
            .is_none_or(AccessibilityPublication::is_current)
        {
            let _ = self.sender.try_send(self.publication.clone());
        }
    }
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
        pub(crate) publication: Option<AccessibilityPublication>,
        pub(crate) font_family: String,
        pub(crate) font: crate::appearance::ResolvedFontDescriptor,
        pub(crate) font_size: Pixels,
        pub(crate) bounds: Option<Bounds<Pixels>>,
        pub(crate) cell_width: Pixels,
        pub(crate) line_height: Pixels,
        pub(crate) hierarchy: Vec<bool>,
        pub(crate) presented: bool,
        pub(crate) visible: bool,
        pub(crate) focused: bool,
        pub(crate) delivered: AccessibilityNotifications,
        pub(crate) selection_sender: Option<AccessibilitySelectionSender>,
        pub(crate) demand_sender: Option<AccessibilityDemandSender>,
        pub(crate) focus_sender: Option<AccessibilityFocusSender>,
        pub(crate) dropped: bool,
    }

    impl TerminalAccessibilityAdapterFactory for RecordingAccessibilityFactory {
        fn create(
            &self,
            _: &Window,
            model: TerminalAccessibilityModel,
            font: &crate::appearance::ResolvedFontDescriptor,
            font_size: Pixels,
        ) -> Box<dyn TerminalAccessibilityAdapter> {
            let record = Rc::new(RefCell::new(AccessibilityRecord {
                model,
                publication: None,
                font_family: font.primary_family.clone(),
                font: font.clone(),
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
                demand_sender: None,
                focus_sender: None,
                dropped: false,
            }));
            self.records.borrow_mut().push(Rc::clone(&record));
            Box::new(RecordingAccessibilityAdapter(record))
        }
    }

    struct RecordingAccessibilityAdapter(Rc<RefCell<AccessibilityRecord>>);

    impl TerminalAccessibilityAdapter for RecordingAccessibilityAdapter {
        fn supports_generation_bound_content(&self) -> bool {
            true
        }
        fn set_hierarchy(&mut self, presented: bool) {
            let mut record = self.0.borrow_mut();
            record.hierarchy.push(presented);
            record.presented = presented;
            record.visible &= presented;
            record.focused &= presented;
            if !presented {
                if let Some(publication) = record.publication.take() {
                    publication.revoke();
                }
                record.model = empty_accessibility_model();
                record.selection_sender = None;
                record.demand_sender = None;
                record.focus_sender = None;
            }
        }

        fn update(
            &mut self,
            update: TerminalAccessibilityUpdate<'_>,
        ) -> AccessibilityNotifications {
            let _ = update.window;
            let mut record = self.0.borrow_mut();
            let was_focused = record.focused;
            record.publication = update.publication;
            record.model = if record.presented
                && record
                    .publication
                    .as_ref()
                    .is_none_or(|publication| publication.permits(update.model))
            {
                update.model.clone()
            } else {
                empty_accessibility_model()
            };
            record.font_family = update.font.primary_family.clone();
            record.font = update.font.clone();
            record.font_size = update.font_size;
            record.bounds = update.bounds;
            record.cell_width = update.cell_width;
            record.line_height = update.line_height;
            record.selection_sender = update.selection_sender.filter(|_| record.presented);
            record.demand_sender = update.demand_sender.filter(|_| record.presented);
            record.focus_sender = update.focus_sender.filter(|_| record.presented);
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
            if let Some(publication) = record.publication.take() {
                publication.revoke();
            }
            record.model = empty_accessibility_model();
            record.dropped = true;
            record.presented = false;
            record.visible = false;
            record.focused = false;
            record.selection_sender = None;
            record.demand_sender = None;
            record.focus_sender = None;
        }
    }
}
