//! Hides ordinary content from assistive technology while a modal transient is presented.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window, accesskit,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

thread_local! {
    static CURRENT_SCOPE: RefCell<Option<Rc<Cell<bool>>>> = const { RefCell::new(None) };
}

struct ScopeGuard(Option<Rc<Cell<bool>>>);
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT_SCOPE.replace(self.0.take());
    }
}

/// Records that a modal transient, such as an open Command Palette, rendered in this frame.
///
/// Call it while rendering the presented transient. Outside a [`crate::ModalLayer`] transient it
/// does nothing.
pub(crate) fn present_modal_transient() {
    CURRENT_SCOPE.with_borrow(|scope| {
        if let Some(presented) = scope {
            presented.set(true);
        }
    });
}

/// Lays out one transient owner while it can report a modal presentation.
pub(super) struct TransientScope {
    pub(super) content: AnyElement,
    pub(super) presented: Rc<Cell<bool>>,
}

impl IntoElement for TransientScope {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TransientScope {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let _guard = ScopeGuard(CURRENT_SCOPE.replace(Some(self.presented.clone())));
        (self.content.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
    }
}

/// Ordinary content that leaves the accessibility tree while a modal transient is presented.
///
/// Every transient lays out before any element publishes its node, so the decision covers the
/// frame in which the transient opens or closes.
pub(super) struct UnderlayContent {
    pub(super) content: AnyElement,
    pub(super) transient_presented: Rc<Cell<bool>>,
}

impl IntoElement for UnderlayContent {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for UnderlayContent {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some("spaceterm-modal-content".into())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn a11y_role(&self) -> Option<accesskit::Role> {
        self.transient_presented
            .get()
            .then_some(accesskit::Role::Group)
    }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        node.set_hidden();
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.content.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
    }
}
