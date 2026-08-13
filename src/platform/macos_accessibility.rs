use std::ops::Range;

use gpui::{Bounds, Pixels, Window};

#[cfg(all(target_os = "macos", not(test)))]
use crate::terminal::AccessibilitySelectionSender;
use crate::terminal::{
    AccessibilityGeometry, AccessibilityNotification, TerminalAccessibilityModel,
};

pub(crate) const TEXT_AREA_ROLE: &str = "AXTextArea";
pub(crate) const VALUE_CHANGED: &str = "AXValueChanged";
pub(crate) const SELECTION_CHANGED: &str = "AXSelectedTextChanged";
pub(crate) const FOCUS_CHANGED: &str = "AXFocusedUIElementChanged";

pub(crate) fn notification_name(notification: AccessibilityNotification) -> &'static str {
    match notification {
        AccessibilityNotification::Value => VALUE_CHANGED,
        AccessibilityNotification::Selection => SELECTION_CHANGED,
        AccessibilityNotification::Focus => FOCUS_CHANGED,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ScreenRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ScreenRect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Clone, Debug)]
struct AccessibilityElementState {
    model: TerminalAccessibilityModel,
    frame: ScreenRect,
    grid: ScreenRect,
    cell_width: f32,
    line_height: f32,
    focused: bool,
    visible: bool,
    #[cfg(all(target_os = "macos", not(test)))]
    presented: bool,
    #[cfg(all(target_os = "macos", not(test)))]
    registered: bool,
    #[cfg(all(target_os = "macos", not(test)))]
    order: usize,
    #[cfg(all(target_os = "macos", not(test)))]
    selection_sender: Option<AccessibilitySelectionSender>,
    #[cfg(all(target_os = "macos", not(test)))]
    parent: cocoa::base::id,
}

pub(crate) struct MacosAccessibilityUpdate<'a> {
    pub(crate) window: &'a Window,
    pub(crate) model: &'a TerminalAccessibilityModel,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) cell_width: Pixels,
    pub(crate) line_height: Pixels,
    pub(crate) focused: bool,
    pub(crate) notifications: &'a [AccessibilityNotification],
    #[cfg(all(target_os = "macos", not(test)))]
    pub(crate) selection_sender: Option<AccessibilitySelectionSender>,
}

impl AccessibilityElementState {
    fn selected_range(&self) -> Range<usize> {
        self.model.selected_or_cursor_range()
    }

    fn selected_text(&self) -> Option<String> {
        self.model.text_for_range(self.selected_range())
    }

    fn screen_bounds_for_range(&self, range: Range<usize>) -> Option<ScreenRect> {
        let geometry = AccessibilityGeometry::new(0.0, 0.0, self.cell_width, self.line_height)?;
        let (x, y, width, height) = self.model.bounds_for_range(range, geometry)?;
        Some(ScreenRect {
            x: self.grid.x + f64::from(x),
            y: self.grid.y + self.grid.height - f64::from(y + height),
            width: f64::from(width),
            height: f64::from(height),
        })
    }

    fn range_for_screen_point(&self, x: f64, y: f64) -> Option<Range<usize>> {
        if !self.grid.contains(x, y) {
            return None;
        }
        let local_x = (x - self.grid.x) as f32;
        let local_y = (self.grid.y + self.grid.height - y) as f32;
        let geometry = AccessibilityGeometry::new(0.0, 0.0, self.cell_width, self.line_height)?;
        self.model.range_for_point(local_x, local_y, geometry)
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod native {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::ops::Range;
    use std::sync::OnceLock;

    use cocoa::base::{id, nil};
    use cocoa::foundation::{
        NSArray, NSAutoreleasePool, NSInteger, NSPoint, NSRect, NSSize, NSString, NSUInteger,
    };
    use gpui::{Bounds, Pixels, Window};
    use objc::declare::ClassDecl;
    use objc::runtime::{BOOL, Class, NO, Object, Sel, YES};
    use objc::{Encode, Encoding, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        AccessibilityElementState, AccessibilityNotification, MacosAccessibilityUpdate, ScreenRect,
        TEXT_AREA_ROLE, TerminalAccessibilityModel, notification_name,
    };

    const STATE_IVAR: &str = "spacetermAccessibilityState";
    const LAYOUT_CHANGED: &str = "AXLayoutChanged";

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct NSRange {
        location: NSUInteger,
        length: NSUInteger,
    }

    unsafe impl Encode for NSRange {
        fn encode() -> Encoding {
            let encoding = format!(
                "{{NSRange={}{}}}",
                NSUInteger::encode().as_str(),
                NSUInteger::encode().as_str()
            );
            // SAFETY: This is the platform ABI encoding for two consecutive NSUInteger fields.
            unsafe { Encoding::from_str(&encoding) }
        }
    }

    thread_local! {
        static CHILDREN: RefCell<HashMap<usize, Vec<Child>>> = RefCell::new(HashMap::new());
    }

    #[derive(Clone, Copy)]
    struct Child {
        element: id,
        order: usize,
    }

    pub(crate) struct MacosAccessibilityElement {
        element: id,
        state: Box<AccessibilityElementState>,
    }

    impl MacosAccessibilityElement {
        pub(crate) fn new(window: &Window, model: TerminalAccessibilityModel) -> Self {
            let parent = native_view(window).unwrap_or(nil);
            retain(parent);
            let mut state = Box::new(AccessibilityElementState {
                model,
                frame: ScreenRect::default(),
                grid: ScreenRect::default(),
                cell_width: 1.0,
                line_height: 1.0,
                focused: false,
                visible: false,
                presented: false,
                registered: false,
                order: 0,
                selection_sender: None,
                parent,
            });
            // SAFETY: The registered Objective-C class uses the same pointer-sized ivar. The Box
            // keeps the state at a stable address until Drop first removes the element from its
            // parent, clears the ivar, and releases the Objective-C object.
            let element = unsafe {
                let element: id = msg_send![accessibility_class(), alloc];
                let element: id = msg_send![element, init];
                (*element).set_ivar(
                    STATE_IVAR,
                    state.as_mut() as *mut AccessibilityElementState as *mut c_void,
                );
                element
            };
            Self { element, state }
        }

        pub(crate) fn set_hierarchy(&mut self, presented: bool, order: usize) {
            if self.state.presented == presented && self.state.order == order {
                return;
            }
            self.state.presented = presented;
            self.state.order = order;
            self.state.visible &= presented;
            self.state.focused &= presented;
            if self.state.registered && !presented {
                self.state.registered = false;
                unregister_child(self.state.parent, self.element);
            } else if self.state.registered {
                register_child(self.state.parent, self.element, order);
            }
        }

        pub(crate) fn update(&mut self, update: MacosAccessibilityUpdate<'_>) {
            let MacosAccessibilityUpdate {
                window,
                model,
                bounds,
                cell_width,
                line_height,
                focused,
                notifications,
                selection_sender,
            } = update;
            let was_focused = self.state.focused;
            let parent = native_view(window).unwrap_or(nil);
            if parent != self.state.parent {
                if self.state.registered {
                    self.state.registered = false;
                    unregister_child(self.state.parent, self.element);
                }
                retain(parent);
                release(self.state.parent);
                self.state.parent = parent;
            }
            if !self.state.model.shares_snapshot(model) {
                self.state.model = model.clone();
            }
            self.state.selection_sender = selection_sender;
            self.state.cell_width = f32::from(cell_width);
            self.state.line_height = f32::from(line_height);
            let bounds = bounds.and_then(|bounds| screen_rect(parent, bounds));
            self.state.visible = self.state.presented && bounds.is_some();
            self.state.focused = self.state.visible && focused;
            if let Some(bounds) = bounds.filter(|_| self.state.visible) {
                self.state.frame = bounds;
                self.state.grid = bounds;
            } else {
                self.state.frame = ScreenRect::default();
                self.state.grid = ScreenRect::default();
            }
            if self.state.visible && !self.state.registered {
                self.state.registered = true;
                register_child(self.state.parent, self.element, self.state.order);
            } else if !self.state.visible && self.state.registered {
                self.state.registered = false;
                unregister_child(self.state.parent, self.element);
            }

            let focus_gained = !was_focused && self.state.focused;
            if self.state.visible
                && (focus_gained
                    || notifications
                        .iter()
                        .any(|notification| *notification != AccessibilityNotification::Focus))
            {
                let mut native_notifications = notifications
                    .iter()
                    .copied()
                    .filter(|notification| *notification != AccessibilityNotification::Focus)
                    .collect::<Vec<_>>();
                if focus_gained {
                    native_notifications.push(AccessibilityNotification::Focus);
                }
                post_notifications(self.element, native_notifications.into_iter());
            }
        }
    }

    impl Drop for MacosAccessibilityElement {
        fn drop(&mut self) {
            if self.state.registered {
                self.state.registered = false;
                unregister_child(self.state.parent, self.element);
            }
            release(self.state.parent);
            // SAFETY: The element was allocated and retained by this handle. It is no longer
            // reachable through its parent before the borrowed Rust state pointer is cleared.
            unsafe {
                (*self.element).set_ivar(STATE_IVAR, std::ptr::null_mut::<c_void>());
                let _: () = msg_send![self.element, release];
            }
        }
    }

    fn accessibility_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            let mut class = ClassDecl::new(
                "SpaceTermPaneAccessibilityElement",
                Class::get("NSAccessibilityElement").expect("AppKit accessibility class exists"),
            )
            .expect("SpaceTerm accessibility class is registered once");
            class.add_ivar::<*mut c_void>(STATE_IVAR);
            // SAFETY: Every registered function uses the Objective-C ABI and the exact argument
            // and return representation declared by its AppKit selector.
            unsafe {
                class.add_method(
                    sel!(isAccessibilityElement),
                    is_accessibility_element as extern "C" fn(&Object, Sel) -> BOOL,
                );
                class.add_method(
                    sel!(accessibilityRole),
                    accessibility_role as extern "C" fn(&Object, Sel) -> id,
                );
                class.add_method(
                    sel!(accessibilityLabel),
                    accessibility_label as extern "C" fn(&Object, Sel) -> id,
                );
                class.add_method(
                    sel!(accessibilityValue),
                    accessibility_value as extern "C" fn(&Object, Sel) -> id,
                );
                class.add_method(
                    sel!(accessibilityFrame),
                    accessibility_frame as extern "C" fn(&Object, Sel) -> NSRect,
                );
                class.add_method(
                    sel!(accessibilityParent),
                    accessibility_parent as extern "C" fn(&Object, Sel) -> id,
                );
                class.add_method(
                    sel!(isAccessibilityFocused),
                    is_accessibility_focused as extern "C" fn(&Object, Sel) -> BOOL,
                );
                class.add_method(
                    sel!(accessibilityNumberOfCharacters),
                    accessibility_number_of_characters as extern "C" fn(&Object, Sel) -> NSInteger,
                );
                class.add_method(
                    sel!(accessibilityVisibleCharacterRange),
                    accessibility_visible_character_range as extern "C" fn(&Object, Sel) -> NSRange,
                );
                class.add_method(
                    sel!(accessibilitySelectedTextRange),
                    accessibility_selected_text_range as extern "C" fn(&Object, Sel) -> NSRange,
                );
                class.add_method(
                    sel!(setAccessibilitySelectedTextRange:),
                    set_accessibility_selected_text_range as extern "C" fn(&Object, Sel, NSRange),
                );
                class.add_method(
                    sel!(accessibilitySelectedText),
                    accessibility_selected_text as extern "C" fn(&Object, Sel) -> id,
                );
                class.add_method(
                    sel!(accessibilityStringForRange:),
                    accessibility_string_for_range as extern "C" fn(&Object, Sel, NSRange) -> id,
                );
                class.add_method(
                    sel!(accessibilityRangeForLine:),
                    accessibility_range_for_line
                        as extern "C" fn(&Object, Sel, NSInteger) -> NSRange,
                );
                class.add_method(
                    sel!(accessibilityLineForIndex:),
                    accessibility_line_for_index
                        as extern "C" fn(&Object, Sel, NSInteger) -> NSInteger,
                );
                class.add_method(
                    sel!(accessibilityRangeForIndex:),
                    accessibility_range_for_index
                        as extern "C" fn(&Object, Sel, NSInteger) -> NSRange,
                );
                class.add_method(
                    sel!(accessibilityRangeForPosition:),
                    accessibility_range_for_position
                        as extern "C" fn(&Object, Sel, NSPoint) -> NSRange,
                );
                class.add_method(
                    sel!(accessibilityFrameForRange:),
                    accessibility_frame_for_range as extern "C" fn(&Object, Sel, NSRange) -> NSRect,
                );
                class.add_method(
                    sel!(accessibilityHitTest:),
                    accessibility_hit_test as extern "C" fn(&Object, Sel, NSPoint) -> id,
                );
            }
            class.register()
        })
    }

    fn native_view(window: &Window) -> Option<id> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        Some(handle.ns_view.as_ptr().cast())
    }

    fn screen_rect(view: id, bounds: Bounds<Pixels>) -> Option<ScreenRect> {
        if view == nil {
            return None;
        }
        // SAFETY: `view` comes from GPUI's live AppKit WindowHandle. Coordinate conversion is
        // synchronous on AppKit's main thread, where TerminalPane renders.
        unsafe {
            let view_bounds: NSRect = msg_send![view, bounds];
            let view_rect = NSRect::new(
                NSPoint::new(
                    f64::from(bounds.origin.x),
                    view_bounds.size.height
                        - f64::from(bounds.origin.y)
                        - f64::from(bounds.size.height),
                ),
                NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
            );
            let window_rect: NSRect = msg_send![view, convertRect:view_rect toView:nil];
            let window: id = msg_send![view, window];
            if window == nil {
                return None;
            }
            let screen: NSRect = msg_send![window, convertRectToScreen:window_rect];
            Some(ScreenRect {
                x: screen.origin.x,
                y: screen.origin.y,
                width: screen.size.width,
                height: screen.size.height,
            })
        }
    }

    fn retain(object: id) {
        if object == nil {
            return;
        }
        // SAFETY: The AppKit object is live when obtained from GPUI's WindowHandle. Retaining it
        // keeps the parent valid until the Pane element unregisters itself.
        unsafe {
            let _: id = msg_send![object, retain];
        }
    }

    fn release(object: id) {
        if object == nil {
            return;
        }
        // SAFETY: Every non-nil parent stored in state owns exactly one retain from this handle.
        unsafe {
            let _: () = msg_send![object, release];
        }
    }

    fn register_child(parent: id, child: id, order: usize) {
        if parent == nil {
            return;
        }
        let siblings = CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            let siblings = children.entry(parent as usize).or_default();
            if let Some(existing) = siblings.iter_mut().find(|entry| entry.element == child) {
                existing.order = order;
            } else {
                siblings.push(Child {
                    element: child,
                    order,
                });
            }
            siblings.sort_by_key(|entry| (entry.order, entry.element as usize));
            siblings.clone()
        });
        reconcile_children(parent, &siblings);
    }

    fn unregister_child(parent: id, child: id) {
        if parent == nil {
            return;
        }
        let siblings = CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            if let Some(siblings) = children.get_mut(&(parent as usize)) {
                siblings.retain(|candidate| candidate.element != child);
                let snapshot = siblings.clone();
                if siblings.is_empty() {
                    children.remove(&(parent as usize));
                }
                snapshot
            } else {
                Vec::new()
            }
        });
        reconcile_children(parent, &siblings);
    }

    fn reconcile_children(parent: id, children: &[Child]) {
        if parent == nil {
            return;
        }
        // SAFETY: All Objective-C calls are synchronous on AppKit's main thread. The registry's
        // RefCell borrow ended before this function, so setters may reenter without a Rust panic.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let current: id = msg_send![parent, accessibilityChildren];
            let navigation_setter = sel!(setAccessibilityChildrenInNavigationOrder:);
            let supports_navigation_order: BOOL =
                msg_send![parent, respondsToSelector:navigation_setter];
            let current_navigation: id = if !matches!(supports_navigation_order, NO) {
                msg_send![parent, accessibilityChildrenInNavigationOrder]
            } else {
                nil
            };
            let count: NSUInteger = if current == nil {
                0
            } else {
                msg_send![current, count]
            };
            let mut reconciled = Vec::with_capacity(count as usize + children.len());
            for index in 0..count {
                let candidate: id = msg_send![current, objectAtIndex:index];
                let managed: BOOL = msg_send![candidate, isKindOfClass:accessibility_class()];
                if matches!(managed, NO) {
                    reconciled.push(candidate);
                }
            }
            reconciled.extend(children.iter().map(|child| child.element));
            let navigation = if !matches!(supports_navigation_order, NO) {
                let navigation_source = if current_navigation == nil {
                    current
                } else {
                    current_navigation
                };
                let navigation_count: NSUInteger = if navigation_source == nil {
                    0
                } else {
                    msg_send![navigation_source, count]
                };
                let mut navigation = Vec::with_capacity(navigation_count as usize + children.len());
                for index in 0..navigation_count {
                    let candidate: id = msg_send![navigation_source, objectAtIndex:index];
                    let managed: BOOL = msg_send![candidate, isKindOfClass:accessibility_class()];
                    if matches!(managed, NO) {
                        navigation.push(candidate);
                    }
                }
                navigation.extend(children.iter().map(|child| child.element));
                Some(navigation)
            } else {
                None
            };
            let children_array = NSArray::arrayWithObjects(nil, &reconciled);
            let navigation_array = navigation
                .as_ref()
                .map(|navigation| NSArray::arrayWithObjects(nil, navigation));
            let _: () = msg_send![parent, setAccessibilityChildren:children_array];
            if let Some(navigation_array) = navigation_array {
                let _: () =
                    msg_send![parent, setAccessibilityChildrenInNavigationOrder:navigation_array];
            }
            post_native_notification(parent, LAYOUT_CHANGED);
            pool.drain();
        }
    }

    fn post_native_notification(element: id, name: &str) {
        #[link(name = "AppKit", kind = "framework")]
        unsafe extern "C" {
            fn NSAccessibilityPostNotification(element: id, notification: id);
        }
        // SAFETY: The caller owns an autorelease pool and `element` remains retained throughout
        // the synchronous accessibility notification.
        unsafe {
            let name = NSString::alloc(nil).init_str(name).autorelease();
            NSAccessibilityPostNotification(element, name);
        }
    }

    fn post_notifications(
        element: id,
        notifications: impl Iterator<Item = AccessibilityNotification>,
    ) {
        // SAFETY: The element is retained by MacosAccessibilityElement and every notification
        // name is an autoreleased NSString used only by the synchronous AppKit call.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            for notification in notifications {
                post_native_notification(element, notification_name(notification));
            }
            pool.drain();
        }
    }

    fn state(this: &Object) -> Option<&AccessibilityElementState> {
        // SAFETY: The ivar is installed from a live boxed state before the element is registered
        // with AppKit and cleared only after the element is removed from its parent.
        unsafe {
            let state = *this.get_ivar::<*mut c_void>(STATE_IVAR);
            state.cast::<AccessibilityElementState>().as_ref()
        }
    }

    fn ns_range(range: Range<usize>) -> NSRange {
        NSRange {
            location: range.start as NSUInteger,
            length: range.len() as NSUInteger,
        }
    }

    fn rust_range(range: NSRange) -> Option<Range<usize>> {
        let start = usize::try_from(range.location).ok()?;
        let length = usize::try_from(range.length).ok()?;
        Some(start..start.checked_add(length)?)
    }

    fn ns_rect(rect: ScreenRect) -> NSRect {
        NSRect::new(
            NSPoint::new(rect.x, rect.y),
            NSSize::new(rect.width, rect.height),
        )
    }

    fn empty_rect() -> NSRect {
        ns_rect(ScreenRect::default())
    }

    fn invalid_range() -> NSRange {
        NSRange {
            location: NSUInteger::MAX,
            length: 0,
        }
    }

    fn ns_string(value: &str) -> id {
        // SAFETY: The autoreleased NSString survives the synchronous accessibility query under
        // AppKit's surrounding autorelease pool.
        unsafe { NSString::alloc(nil).init_str(value).autorelease() }
    }

    extern "C" fn is_accessibility_element(this: &Object, _: Sel) -> BOOL {
        if state(this).is_some_and(|state| state.visible) {
            YES
        } else {
            NO
        }
    }

    extern "C" fn accessibility_role(_: &Object, _: Sel) -> id {
        ns_string(TEXT_AREA_ROLE)
    }

    extern "C" fn accessibility_label(_: &Object, _: Sel) -> id {
        ns_string("Terminal Pane")
    }

    extern "C" fn accessibility_value(this: &Object, _: Sel) -> id {
        state(this).map_or(nil, |state| ns_string(state.model.text()))
    }

    extern "C" fn accessibility_frame(this: &Object, _: Sel) -> NSRect {
        state(this)
            .filter(|state| state.visible)
            .map_or_else(empty_rect, |state| ns_rect(state.frame))
    }

    extern "C" fn accessibility_parent(this: &Object, _: Sel) -> id {
        state(this)
            .filter(|state| state.visible && state.registered)
            .map_or(nil, |state| state.parent)
    }

    extern "C" fn is_accessibility_focused(this: &Object, _: Sel) -> BOOL {
        if state(this).is_some_and(|state| state.focused) {
            YES
        } else {
            NO
        }
    }

    extern "C" fn accessibility_number_of_characters(this: &Object, _: Sel) -> NSInteger {
        state(this).map_or(0, |state| {
            NSInteger::try_from(state.model.len_utf16()).unwrap_or(NSInteger::MAX)
        })
    }

    extern "C" fn accessibility_visible_character_range(this: &Object, _: Sel) -> NSRange {
        state(this).map_or_else(invalid_range, |state| ns_range(state.model.visible_range()))
    }

    extern "C" fn accessibility_selected_text_range(this: &Object, _: Sel) -> NSRange {
        state(this).map_or_else(invalid_range, |state| ns_range(state.selected_range()))
    }

    extern "C" fn set_accessibility_selected_text_range(this: &Object, _: Sel, range: NSRange) {
        let Some((sender, request)) = state(this)
            .filter(|state| state.visible && state.registered)
            .and_then(|state| {
                Some((
                    state.selection_sender.as_ref()?,
                    state.model.selection_request(rust_range(range)?)?,
                ))
            })
        else {
            return;
        };
        sender.request(request);
    }

    extern "C" fn accessibility_selected_text(this: &Object, _: Sel) -> id {
        state(this)
            .and_then(AccessibilityElementState::selected_text)
            .map_or(nil, |text| ns_string(&text))
    }

    extern "C" fn accessibility_string_for_range(this: &Object, _: Sel, range: NSRange) -> id {
        state(this)
            .and_then(|state| state.model.text_for_range(rust_range(range)?))
            .map_or(nil, |text| ns_string(&text))
    }

    extern "C" fn accessibility_range_for_line(this: &Object, _: Sel, line: NSInteger) -> NSRange {
        state(this)
            .and_then(|state| state.model.range_for_line(usize::try_from(line).ok()?))
            .map_or_else(invalid_range, ns_range)
    }

    extern "C" fn accessibility_line_for_index(
        this: &Object,
        _: Sel,
        index: NSInteger,
    ) -> NSInteger {
        state(this)
            .and_then(|state| state.model.line_for_index(usize::try_from(index).ok()?))
            .and_then(|line| NSInteger::try_from(line).ok())
            .unwrap_or(-1)
    }

    extern "C" fn accessibility_range_for_index(
        this: &Object,
        _: Sel,
        index: NSInteger,
    ) -> NSRange {
        state(this)
            .and_then(|state| state.model.range_for_index(usize::try_from(index).ok()?))
            .map_or_else(invalid_range, ns_range)
    }

    extern "C" fn accessibility_range_for_position(
        this: &Object,
        _: Sel,
        point: NSPoint,
    ) -> NSRange {
        state(this)
            .and_then(|state| state.range_for_screen_point(point.x, point.y))
            .map_or_else(invalid_range, ns_range)
    }

    extern "C" fn accessibility_frame_for_range(this: &Object, _: Sel, range: NSRange) -> NSRect {
        state(this)
            .and_then(|state| state.screen_bounds_for_range(rust_range(range)?))
            .map_or_else(empty_rect, ns_rect)
    }

    extern "C" fn accessibility_hit_test(this: &Object, _: Sel, point: NSPoint) -> id {
        if state(this).is_some_and(|state| state.visible && state.frame.contains(point.x, point.y))
        {
            this as *const Object as id
        } else {
            nil
        }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
pub(crate) use native::MacosAccessibilityElement;

#[cfg(any(not(target_os = "macos"), test))]
pub(crate) struct MacosAccessibilityElement;

#[cfg(any(not(target_os = "macos"), test))]
impl MacosAccessibilityElement {
    pub(crate) fn new(_: &gpui::Window, _: TerminalAccessibilityModel) -> Self {
        Self
    }

    pub(crate) fn set_hierarchy(&mut self, _: bool, _: usize) {}

    pub(crate) fn update(&mut self, update: MacosAccessibilityUpdate<'_>) {
        let _ = (
            update.window,
            update.model,
            update.bounds,
            update.cell_width,
            update.line_height,
            update.focused,
            update.notifications,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{AccessibilityCell, AccessibilityLine};

    fn state() -> AccessibilityElementState {
        AccessibilityElementState {
            model: TerminalAccessibilityModel::new(
                vec![AccessibilityLine::new(
                    vec![
                        AccessibilityCell::new("A", 1, false),
                        AccessibilityCell::new("😀", 2, true),
                    ],
                    false,
                )],
                0..1,
                Some((0, 3)),
            ),
            frame: ScreenRect {
                x: 100.0,
                y: 200.0,
                width: 30.0,
                height: 20.0,
            },
            grid: ScreenRect {
                x: 100.0,
                y: 200.0,
                width: 30.0,
                height: 20.0,
            },
            cell_width: 10.0,
            line_height: 20.0,
            focused: true,
            visible: true,
        }
    }

    #[test]
    fn pane_state_exposes_utf16_selection_and_screen_geometry() {
        let state = state();

        assert!(state.visible);
        assert!(state.focused);
        assert_eq!(state.frame, state.grid);
        assert_eq!(state.selected_range(), 1..3);
        assert_eq!(state.selected_text(), Some("😀".to_owned()));
        assert_eq!(
            state.screen_bounds_for_range(1..3),
            Some(ScreenRect {
                x: 110.0,
                y: 200.0,
                width: 20.0,
                height: 20.0,
            })
        );
        assert_eq!(state.range_for_screen_point(120.0, 210.0), Some(1..3));
        assert_eq!(state.range_for_screen_point(130.0, 210.0), None);
        assert_eq!(state.range_for_screen_point(120.0, 220.0), None);
    }

    #[test]
    fn typed_notifications_map_to_native_accessibility_names_without_text() {
        assert_eq!(
            [
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
                AccessibilityNotification::Focus,
            ]
            .map(notification_name),
            [VALUE_CHANGED, SELECTION_CHANGED, FOCUS_CHANGED]
        );
        assert_eq!(TEXT_AREA_ROLE, "AXTextArea");
    }
}
