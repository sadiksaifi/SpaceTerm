//! Pointer-only window management regions inside a modal root.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels, Point, Window, point,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone, Default)]
pub(super) struct ChromeFrame {
    regions: Rc<RefCell<Vec<Hitbox>>>,
    controls: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    pub(super) pointer: Rc<Cell<Option<MouseButton>>>,
    blocker: Rc<RefCell<Option<super::render::ModalPointerBlocker>>>,
}

thread_local! {
    static CURRENT_FRAME: RefCell<Option<ChromeFrame>> = const { RefCell::new(None) };
}

struct FrameGuard(Option<ChromeFrame>);
impl Drop for FrameGuard {
    fn drop(&mut self) {
        CURRENT_FRAME.replace(self.0.take());
    }
}

pub(super) struct ChromeScope {
    pub(super) content: AnyElement,
    pub(super) frame: ChromeFrame,
}

impl IntoElement for ChromeScope {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ChromeScope {
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
        self.frame.regions.borrow_mut().clear();
        self.frame.controls.borrow_mut().clear();
        self.frame.blocker.borrow_mut().take();
        let _guard = FrameGuard(CURRENT_FRAME.replace(Some(self.frame.clone())));
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
        if let Some(blocker) = self.frame.blocker.borrow_mut().take() {
            blocker.register(window);
        }
        self.content.paint(window, cx);
    }
}

pub(super) struct ChromeRegion {
    pub(super) content: AnyElement,
    pub(super) protect_from_resize: bool,
}
impl IntoElement for ChromeRegion {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for ChromeRegion {
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
        (self.content.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
        if self.protect_from_resize {
            CURRENT_FRAME.with_borrow(|frame| {
                if let Some(frame) = frame {
                    frame
                        .controls
                        .borrow_mut()
                        .push(bounds.intersect(&window.content_mask().bounds));
                }
            });
        }
        // Register after this region's descendants, before later siblings. A titlebar
        // tracker remains behind application controls, which can occlude its marker.
        if CURRENT_FRAME.with_borrow(Option::is_some) {
            insert_hitbox(bounds, HitboxBehavior::Normal, window);
        }
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

pub(super) struct ChromeRouting {
    regions: Vec<Hitbox>,
    pub(super) pointer: Rc<Cell<Option<MouseButton>>>,
}

impl ChromeRouting {
    pub(super) fn contains(&self, position: Point<Pixels>, window: &Window) -> bool {
        self.regions
            .iter()
            .any(|region| region.is_hovered_at(position, window))
    }
}

pub(super) fn insert_hitbox(
    bounds: Bounds<Pixels>,
    behavior: HitboxBehavior,
    window: &mut Window,
) -> Hitbox {
    let hitbox = window.insert_hitbox(bounds, behavior);
    CURRENT_FRAME.with_borrow(|frame| {
        if let Some(frame) = frame {
            frame.regions.borrow_mut().push(hitbox.clone());
        }
    });
    hitbox
}

pub(super) fn insert_resize_hitboxes(bounds: Bounds<Pixels>, window: &mut Window) -> Vec<Hitbox> {
    let regions = CURRENT_FRAME.with_borrow(|frame| {
        let mut regions = vec![bounds];
        if let Some(frame) = frame {
            for control in frame.controls.borrow().iter() {
                regions = regions
                    .into_iter()
                    .flat_map(|bounds| subtract_region(bounds, *control))
                    .collect();
            }
        }
        regions
    });
    regions
        .into_iter()
        .map(|bounds| insert_hitbox(bounds, HitboxBehavior::BlockMouse, window))
        .collect()
}

pub(super) fn prepaint_blocker(bounds: Bounds<Pixels>, window: &mut Window) -> ChromeRouting {
    let frame = CURRENT_FRAME.with_borrow(Clone::clone).unwrap_or_default();
    let regions = frame.regions.borrow().clone();
    let mut blocked = vec![bounds];
    for region in &regions {
        let hole = region.bounds.intersect(&region.content_mask.bounds);
        blocked = blocked
            .into_iter()
            .flat_map(|bounds| subtract_region(bounds, hole))
            .collect();
    }
    for bounds in blocked {
        window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
    }
    ChromeRouting {
        regions,
        pointer: frame.pointer,
    }
}

fn subtract_region(bounds: Bounds<Pixels>, hole: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
    let hole = bounds.intersect(&hole);
    if hole.is_empty() {
        return vec![bounds];
    }
    [
        Bounds::from_corners(bounds.origin, point(bounds.right(), hole.top())),
        Bounds::from_corners(point(bounds.left(), hole.bottom()), bounds.bottom_right()),
        Bounds::from_corners(
            point(bounds.left(), hole.top()),
            point(hole.left(), hole.bottom()),
        ),
        Bounds::from_corners(
            point(hole.right(), hole.top()),
            point(bounds.right(), hole.bottom()),
        ),
    ]
    .into_iter()
    .filter(|bounds| !bounds.is_empty())
    .collect()
}

pub(super) fn set_blocker(blocker: super::render::ModalPointerBlocker) {
    CURRENT_FRAME.with_borrow(|frame| {
        if let Some(frame) = frame {
            *frame.blocker.borrow_mut() = Some(blocker);
        }
    });
}
