#[cfg(any(not(test), feature = "macos-native-tests"))]
use std::ops::Range;

#[cfg(not(test))]
use super::terminal_accessibility::TerminalAccessibilityUpdate;
use super::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory,
};
#[cfg(not(test))]
use crate::terminal::AccessibilityNotifications;
use gpui::{Pixels, Window};

use crate::terminal::TerminalAccessibilityModel;
#[cfg(all(target_os = "macos", not(test)))]
use crate::terminal::{AccessibilityDemandSender, AccessibilitySelectionSender};
#[cfg(any(not(test), feature = "macos-native-tests"))]
use crate::terminal::{AccessibilityGeometry, AccessibilityNotification};

#[cfg(any(not(test), feature = "macos-native-tests"))]
const TEXT_AREA_ROLE: &str = "AXTextArea";
#[cfg(any(not(test), feature = "macos-native-tests"))]
const VALUE_CHANGED: &str = "AXValueChanged";
#[cfg(any(not(test), feature = "macos-native-tests"))]
const SELECTION_CHANGED: &str = "AXSelectedTextChanged";
#[cfg(any(not(test), feature = "macos-native-tests"))]
const FOCUS_CHANGED: &str = "AXFocusedUIElementChanged";

#[cfg(any(not(test), feature = "macos-native-tests"))]
fn notification_name(notification: AccessibilityNotification) -> &'static str {
    match notification {
        AccessibilityNotification::Value => VALUE_CHANGED,
        AccessibilityNotification::Selection => SELECTION_CHANGED,
        AccessibilityNotification::Focus => FOCUS_CHANGED,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg(any(not(test), feature = "macos-native-tests"))]
struct ScreenRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg(any(not(test), feature = "macos-native-tests"))]
impl ScreenRect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Clone, Debug)]
#[cfg(any(not(test), feature = "macos-native-tests"))]
struct AccessibilityElementState {
    model: TerminalAccessibilityModel,
    font: Option<AccessibilityFontMetadata>,
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
    demand_sender: Option<AccessibilityDemandSender>,
    #[cfg(all(target_os = "macos", not(test)))]
    parent: Option<objc2::rc::Retained<objc2_app_kit::NSView>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg(any(not(test), feature = "macos-native-tests"))]
struct AccessibilityFontMetadata {
    requested_descriptor: crate::appearance::ResolvedFontDescriptor,
    requested_family: String,
    requested_point_size: f32,
    name: String,
    family: Option<String>,
    visible_name: Option<String>,
    point_size: f32,
}

#[derive(Debug, PartialEq)]
#[cfg(any(not(test), feature = "macos-native-tests"))]
struct AccessibilityAttributedText<'a> {
    text: String,
    font: &'a AccessibilityFontMetadata,
}

#[cfg(any(not(test), feature = "macos-native-tests"))]
impl AccessibilityElementState {
    fn font_request_changed(&self, family: &str, point_size: f32) -> bool {
        let family = normalized_font_family(family);
        let point_size = normalized_font_point_size(point_size);
        self.font.as_ref().is_none_or(|font| {
            font.requested_family != family || font.requested_point_size != point_size
        })
    }

    fn selected_range(&self) -> Range<usize> {
        self.model.selected_or_cursor_range()
    }

    fn selected_text(&self) -> Option<String> {
        self.model.text_for_range(self.selected_range())
    }

    fn string_for_range(&self, range: Range<usize>) -> Option<String> {
        self.model.text_for_range(range)
    }

    fn attributed_text_for_range(
        &self,
        range: Range<usize>,
    ) -> Option<AccessibilityAttributedText<'_>> {
        Some(AccessibilityAttributedText {
            text: self.string_for_range(range)?,
            font: self.font.as_ref()?,
        })
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
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::ops::Range;

    use gpui::{Bounds, Pixels, Window};
    use objc2::rc::{Retained, Weak};
    use objc2::runtime::AnyObject;
    use objc2::{
        AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
    };
    use objc2_app_kit::{
        NSAccessibilityElement, NSAccessibilityFontFamilyKey, NSAccessibilityFontNameKey,
        NSAccessibilityFontSizeKey, NSAccessibilityFontTextAttribute,
        NSAccessibilityPostNotification, NSAccessibilityVisibleNameKey, NSFont, NSFontManager,
        NSFontTraitMask, NSView,
    };
    use objc2_foundation::{
        NSArray, NSAttributedString, NSDictionary, NSInteger, NSNumber, NSObjectProtocol, NSPoint,
        NSRange, NSRect, NSSize, NSString,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        AccessibilityAttributedText, AccessibilityElementState, AccessibilityFontMetadata,
        AccessibilityNotification, AccessibilityNotifications, ScreenRect, TEXT_AREA_ROLE,
        TerminalAccessibilityModel, TerminalAccessibilityUpdate, normalized_font_family,
        normalized_font_point_size, notification_name,
    };

    const LAYOUT_CHANGED: &str = "AXLayoutChanged";

    thread_local! {
        static CHILDREN: RefCell<HashMap<usize, Vec<Child>>> = RefCell::new(HashMap::new());
    }

    #[derive(Clone)]
    struct Child {
        element: Weak<PaneAccessibilityElement>,
        identity: usize,
        order: usize,
    }

    struct AccessibilityIvars {
        state: Cell<*mut AccessibilityElementState>,
    }

    define_class!(
        // SAFETY: NSAccessibilityElement has no additional subclassing requirements. The owner
        // clears the borrowed state pointer before releasing this native element.
        #[unsafe(super(NSAccessibilityElement))]
        #[name = "SpaceTermPaneAccessibilityElement"]
        #[thread_kind = MainThreadOnly]
        #[ivars = AccessibilityIvars]
        struct PaneAccessibilityElement;

        impl PaneAccessibilityElement {
            #[unsafe(method(isAccessibilityElement))]
            fn is_accessibility_element(&self) -> bool {
                state(self).is_some_and(|state| state.visible)
            }

            #[unsafe(method_id(accessibilityRole))]
            fn accessibility_role(&self) -> Retained<NSString> {
                NSString::from_str(TEXT_AREA_ROLE)
            }

            #[unsafe(method_id(accessibilityLabel))]
            fn accessibility_label(&self) -> Retained<NSString> {
                NSString::from_str("Terminal Pane")
            }

            #[unsafe(method_id(accessibilityValue))]
            fn accessibility_value(&self) -> Option<Retained<NSString>> {
                semantic_state(self).map(|state| NSString::from_str(state.model.text()))
            }

            #[unsafe(method(accessibilityFrame))]
            fn accessibility_frame(&self) -> NSRect {
                state(self)
                    .filter(|state| state.visible)
                    .map_or_else(empty_rect, |state| ns_rect(state.frame))
            }

            #[unsafe(method_id(accessibilityParent))]
            fn accessibility_parent(&self) -> Option<Retained<NSView>> {
                state(self)
                    .filter(|state| state.visible && state.registered)
                    .and_then(|state| state.parent.clone())
            }

            #[unsafe(method(isAccessibilityFocused))]
            fn is_accessibility_focused(&self) -> bool {
                state(self).is_some_and(|state| state.focused)
            }

            #[unsafe(method(accessibilityNumberOfCharacters))]
            fn accessibility_number_of_characters(&self) -> NSInteger {
                semantic_state(self).map_or(0, |state| {
                    NSInteger::try_from(state.model.len_utf16()).unwrap_or(NSInteger::MAX)
                })
            }

            #[unsafe(method(accessibilityVisibleCharacterRange))]
            fn accessibility_visible_character_range(&self) -> NSRange {
                semantic_state(self)
                    .map_or_else(invalid_range, |state| ns_range(state.model.visible_range()))
            }

            #[unsafe(method(accessibilitySelectedTextRange))]
            fn accessibility_selected_text_range(&self) -> NSRange {
                semantic_state(self).map_or_else(invalid_range, |state| ns_range(state.selected_range()))
            }

            #[unsafe(method(setAccessibilitySelectedTextRange:))]
            fn set_accessibility_selected_text_range(&self, range: NSRange) {
                let Some((sender, request)) = semantic_state(self)
                    .filter(|state| state.visible && state.registered)
                    .and_then(|state| {
                        Some((
                            state.selection_sender.clone()?,
                            state.model.selection_request(rust_range(range)?)?,
                        ))
                    })
                else {
                    return;
                };
                sender.request(request);
            }

            #[unsafe(method_id(accessibilitySelectedText))]
            fn accessibility_selected_text(&self) -> Option<Retained<NSString>> {
                semantic_state(self)
                    .and_then(AccessibilityElementState::selected_text)
                    .map(|text| NSString::from_str(&text))
            }

            #[unsafe(method_id(accessibilityStringForRange:))]
            fn accessibility_string_for_range(&self, range: NSRange) -> Option<Retained<NSString>> {
                semantic_state(self)
                    .and_then(|state| state.string_for_range(rust_range(range)?))
                    .map(|text| NSString::from_str(&text))
            }

            #[unsafe(method_id(accessibilityAttributedStringForRange:))]
            fn accessibility_attributed_string_for_range(
                &self,
                range: NSRange,
            ) -> Option<Retained<NSAttributedString>> {
                semantic_state(self)
                    .and_then(|state| state.attributed_text_for_range(rust_range(range)?))
                    .map(|text| ns_attributed_string(&text))
            }

            #[unsafe(method(accessibilityRangeForLine:))]
            fn accessibility_range_for_line(&self, line: NSInteger) -> NSRange {
                semantic_state(self)
                    .and_then(|state| state.model.range_for_line(usize::try_from(line).ok()?))
                    .map_or_else(invalid_range, ns_range)
            }

            #[unsafe(method(accessibilityLineForIndex:))]
            fn accessibility_line_for_index(&self, index: NSInteger) -> NSInteger {
                semantic_state(self)
                    .and_then(|state| state.model.line_for_index(usize::try_from(index).ok()?))
                    .and_then(|line| NSInteger::try_from(line).ok())
                    .unwrap_or(-1)
            }

            #[unsafe(method(accessibilityRangeForIndex:))]
            fn accessibility_range_for_index(&self, index: NSInteger) -> NSRange {
                semantic_state(self)
                    .and_then(|state| state.model.range_for_index(usize::try_from(index).ok()?))
                    .map_or_else(invalid_range, ns_range)
            }

            #[unsafe(method(accessibilityRangeForPosition:))]
            fn accessibility_range_for_position(&self, point: NSPoint) -> NSRange {
                semantic_state(self)
                    .and_then(|state| state.range_for_screen_point(point.x, point.y))
                    .map_or_else(invalid_range, ns_range)
            }

            #[unsafe(method(accessibilityFrameForRange:))]
            fn accessibility_frame_for_range(&self, range: NSRange) -> NSRect {
                semantic_state(self)
                    .and_then(|state| state.screen_bounds_for_range(rust_range(range)?))
                    .map_or_else(empty_rect, ns_rect)
            }

            #[unsafe(method_id(accessibilityHitTest:))]
            fn accessibility_hit_test(&self, point: NSPoint) -> Option<Retained<AnyObject>> {
                if state(self).is_some_and(|state| state.visible && state.frame.contains(point.x, point.y)) {
                    // SAFETY: The callback receiver remains live throughout this native method.
                    unsafe { Retained::retain((self as *const Self).cast_mut()) }
                        .map(|element| element.into_super().into_super().into_super())
                } else {
                    None
                }
            }
        }

        unsafe impl NSObjectProtocol for PaneAccessibilityElement {}
    );

    impl PaneAccessibilityElement {
        fn new(mtm: MainThreadMarker, state: *mut AccessibilityElementState) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(AccessibilityIvars {
                state: Cell::new(state),
            });
            // SAFETY: NSAccessibilityElement's init is its designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub(crate) struct MacosAccessibilityElement {
        element: Retained<PaneAccessibilityElement>,
        state: Box<AccessibilityElementState>,
    }

    impl MacosAccessibilityElement {
        pub(crate) fn new(
            window: &Window,
            model: TerminalAccessibilityModel,
            font: &crate::appearance::ResolvedFontDescriptor,
            font_size: Pixels,
        ) -> Self {
            let parent = native_view(window);
            let mut state = Box::new(AccessibilityElementState {
                model,
                font: resolve_font_metadata(font, f32::from(font_size)),
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
                demand_sender: None,
                parent,
            });
            let pointer = state.as_mut() as *mut AccessibilityElementState;
            // SAFETY: GPUI creates this adapter on the AppKit main thread.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let element = PaneAccessibilityElement::new(mtm, pointer);
            Self { element, state }
        }

        pub(crate) fn set_hierarchy(&mut self, presented: bool, order: usize) {
            if self.state.presented == presented && self.state.order == order {
                return;
            }
            self.state.presented = presented;
            self.state.order = order;
            if !presented {
                self.state.selection_sender = None;
                self.state.demand_sender = None;
            }
            self.state.visible &= presented;
            self.state.focused &= presented;
            if self.state.registered && !presented {
                self.state.registered = false;
                if let Some(parent) = &self.state.parent {
                    unregister_child(parent, &self.element);
                }
            } else if self.state.registered
                && let Some(parent) = &self.state.parent
            {
                register_child(parent, &self.element, order);
            }
        }

        pub(crate) fn update(
            &mut self,
            update: TerminalAccessibilityUpdate<'_>,
        ) -> AccessibilityNotifications {
            let TerminalAccessibilityUpdate {
                window,
                model,
                bounds,
                cell_width,
                line_height,
                font,
                font_size,
                focused,
                notifications,
                selection_sender,
                demand_sender,
            } = update;
            let was_focused = self.state.focused;
            let parent = native_view(window);
            if !same_parent(&parent, &self.state.parent) {
                if self.state.registered {
                    self.state.registered = false;
                    if let Some(old_parent) = &self.state.parent {
                        unregister_child(old_parent, &self.element);
                    }
                }
                self.state.parent = parent;
            }
            if !self.state.model.shares_snapshot(model) {
                self.state.model = model.clone();
            }
            self.state.selection_sender = selection_sender.filter(|_| self.state.presented);
            self.state.demand_sender = demand_sender.filter(|_| self.state.presented);
            self.state.cell_width = f32::from(cell_width);
            self.state.line_height = f32::from(line_height);
            let point_size = f32::from(font_size);
            let requested_family = normalized_font_family(&font.primary_family);
            if self
                .state
                .font_request_changed(requested_family, point_size)
                || self
                    .state
                    .font
                    .as_ref()
                    .is_none_or(|metadata| metadata.requested_descriptor != *font)
            {
                self.state.font = resolve_font_metadata(font, point_size);
            }
            let bounds = bounds.and_then(|bounds| {
                self.state
                    .parent
                    .as_deref()
                    .and_then(|parent| screen_rect(parent, bounds))
            });
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
                if let Some(parent) = &self.state.parent {
                    register_child(parent, &self.element, self.state.order);
                }
            } else if !self.state.visible && self.state.registered {
                self.state.registered = false;
                if let Some(parent) = &self.state.parent {
                    unregister_child(parent, &self.element);
                }
            }

            let focus_gained = !was_focused && self.state.focused;
            if self.state.visible {
                let mut native_notifications =
                    notifications.without(AccessibilityNotification::Focus);
                if self.state.focused
                    && (focus_gained || notifications.contains(AccessibilityNotification::Focus))
                {
                    native_notifications.insert(AccessibilityNotification::Focus);
                }
                if !native_notifications.is_empty() {
                    post_notifications(&self.element, native_notifications.iter());
                }
                AccessibilityNotifications::default()
            } else {
                notifications
            }
        }
    }

    impl Drop for MacosAccessibilityElement {
        fn drop(&mut self) {
            if self.state.registered {
                self.state.registered = false;
                if let Some(parent) = &self.state.parent {
                    unregister_child(parent, &self.element);
                }
            }
            self.element.ivars().state.set(std::ptr::null_mut());
        }
    }

    fn same_parent(a: &Option<Retained<NSView>>, b: &Option<Retained<NSView>>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => std::ptr::eq(&**a, &**b),
            (None, None) => true,
            _ => false,
        }
    }

    fn native_view(window: &Window) -> Option<Retained<NSView>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: GPUI owns this live NSView through the synchronous WindowHandle call.
        unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
    }

    fn screen_rect(view: &NSView, bounds: Bounds<Pixels>) -> Option<ScreenRect> {
        let view_bounds = view.bounds();
        let view_rect = NSRect::new(
            NSPoint::new(
                f64::from(bounds.origin.x),
                view_bounds.size.height
                    - f64::from(bounds.origin.y)
                    - f64::from(bounds.size.height),
            ),
            NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
        );
        let window_rect = view.convertRect_toView(view_rect, None);
        let screen = view.window()?.convertRectToScreen(window_rect);
        Some(ScreenRect {
            x: screen.origin.x,
            y: screen.origin.y,
            width: screen.size.width,
            height: screen.size.height,
        })
    }

    fn register_child(parent: &NSView, child: &Retained<PaneAccessibilityElement>, order: usize) {
        let identity = Retained::as_ptr(child) as usize;
        let siblings = CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            let siblings = children
                .entry(parent as *const NSView as usize)
                .or_default();
            if let Some(existing) = siblings.iter_mut().find(|entry| entry.identity == identity) {
                existing.order = order;
            } else {
                siblings.push(Child {
                    element: Weak::from_retained(child),
                    identity,
                    order,
                });
            }
            siblings.sort_by_key(|entry| (entry.order, entry.identity));
            siblings.clone()
        });
        reconcile_children(parent, &siblings);
    }

    fn unregister_child(parent: &NSView, child: &Retained<PaneAccessibilityElement>) {
        let identity = Retained::as_ptr(child) as usize;
        let siblings = CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            let key = parent as *const NSView as usize;
            if let Some(siblings) = children.get_mut(&key) {
                siblings.retain(|candidate| candidate.identity != identity);
                let snapshot = siblings.clone();
                if siblings.is_empty() {
                    children.remove(&key);
                }
                snapshot
            } else {
                Vec::new()
            }
        });
        reconcile_children(parent, &siblings);
    }

    fn unmanaged_children(source: Option<&NSArray<AnyObject>>) -> Vec<Retained<AnyObject>> {
        let Some(source) = source else {
            return Vec::new();
        };
        (0..source.count())
            .map(|index| source.objectAtIndex(index))
            .filter(|candidate| {
                candidate
                    .downcast_ref::<PaneAccessibilityElement>()
                    .is_none()
            })
            .collect()
    }

    fn reconcile_children(parent: &NSView, children: &[Child]) {
        // SAFETY: NSView implements these accessibility selectors. objc2 retains the returned arrays.
        let current: Option<Retained<NSArray<AnyObject>>> =
            unsafe { msg_send![parent, accessibilityChildren] };
        let supports_navigation_order =
            parent.respondsToSelector(sel!(setAccessibilityChildrenInNavigationOrder:));
        let current_navigation: Option<Retained<NSArray<AnyObject>>> = if supports_navigation_order
        {
            // SAFETY: The selector is present on this NSView and returns an NSArray or nil.
            unsafe { msg_send![parent, accessibilityChildrenInNavigationOrder] }
        } else {
            None
        };
        let mut reconciled = unmanaged_children(current.as_deref());
        reconciled.extend(
            children
                .iter()
                .filter_map(|child| child.element.load())
                .map(|child| child.into_super().into_super().into_super()),
        );
        let navigation = if supports_navigation_order {
            let source = current_navigation.as_deref().or(current.as_deref());
            let mut navigation = unmanaged_children(source);
            navigation.extend(
                children
                    .iter()
                    .filter_map(|child| child.element.load())
                    .map(|child| child.into_super().into_super().into_super()),
            );
            Some(navigation)
        } else {
            None
        };
        let children_array = NSArray::from_retained_slice(&reconciled);
        // SAFETY: NSView accepts this array of live accessibility child objects.
        let _: () = unsafe { msg_send![parent, setAccessibilityChildren: &*children_array] };
        if let Some(navigation) = navigation {
            let navigation_array = NSArray::from_retained_slice(&navigation);
            // SAFETY: The selector is present and accepts this array of live child objects.
            let _: () = unsafe {
                msg_send![parent, setAccessibilityChildrenInNavigationOrder: &*navigation_array]
            };
        }
        post_native_notification(parent, LAYOUT_CHANGED);
    }

    fn post_native_notification(element: &AnyObject, name: &str) {
        let name = NSString::from_str(name);
        // SAFETY: This is a live AppKit accessibility element and a valid notification name.
        unsafe { NSAccessibilityPostNotification(element, &name) };
    }

    fn post_notifications(
        element: &PaneAccessibilityElement,
        notifications: impl Iterator<Item = AccessibilityNotification>,
    ) {
        for notification in notifications {
            post_native_notification(element, notification_name(notification));
        }
    }

    fn state(this: &PaneAccessibilityElement) -> Option<&AccessibilityElementState> {
        // SAFETY: The owner keeps its Box stable until it unregisters the element and clears the
        // pointer in Drop. Native callbacks use it only during that registered lifetime.
        unsafe { this.ivars().state.get().as_ref() }
    }

    fn semantic_state(this: &PaneAccessibilityElement) -> Option<&AccessibilityElementState> {
        let state = state(this)?;
        if state.visible
            && state.registered
            && let Some(sender) = &state.demand_sender
        {
            sender.request();
        }
        Some(state)
    }

    fn ns_range(range: Range<usize>) -> NSRange {
        NSRange::new(range.start, range.len())
    }

    fn rust_range(range: NSRange) -> Option<Range<usize>> {
        let start = range.location;
        Some(start..start.checked_add(range.length)?)
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
        NSRange::new(NSInteger::MAX as usize, 0)
    }

    fn ns_attributed_string(
        value: &AccessibilityAttributedText<'_>,
    ) -> Retained<NSAttributedString> {
        let font_name = NSString::from_str(&value.font.name);
        let point_size = NSNumber::numberWithDouble(f64::from(value.font.point_size));
        // SAFETY: AppKit exports these immutable accessibility attribute keys.
        let mut font_keys = unsafe { vec![NSAccessibilityFontNameKey, NSAccessibilityFontSizeKey] };
        let mut font_values: Vec<&AnyObject> = vec![&font_name, &point_size];
        let family = value.font.family.as_deref().map(NSString::from_str);
        if let Some(family) = &family {
            // SAFETY: AppKit exports this immutable accessibility attribute key.
            font_keys.push(unsafe { NSAccessibilityFontFamilyKey });
            font_values.push(family);
        }
        let visible_name = value.font.visible_name.as_deref().map(NSString::from_str);
        if let Some(visible_name) = &visible_name {
            // SAFETY: AppKit exports this immutable accessibility attribute key.
            font_keys.push(unsafe { NSAccessibilityVisibleNameKey });
            font_values.push(visible_name);
        }
        let font_attributes = NSDictionary::from_slices(&font_keys, &font_values);
        // SAFETY: AppKit exports this immutable attributed-string key.
        let font_key = unsafe { NSAccessibilityFontTextAttribute };
        let attributes = NSDictionary::from_slices(&[font_key], &[&font_attributes as &AnyObject]);
        let string = NSString::from_str(&value.text);
        // SAFETY: The nested dictionaries contain the types AppKit requires for AX font data.
        unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &string,
                Some(&attributes),
            )
        }
    }

    fn resolve_font_metadata(
        descriptor: &crate::appearance::ResolvedFontDescriptor,
        point_size: f32,
    ) -> Option<AccessibilityFontMetadata> {
        let requested_family = normalized_font_family(&descriptor.primary_family);
        let point_size = normalized_font_point_size(point_size);
        let mtm = MainThreadMarker::new()?;
        let requested = NSString::from_str(requested_family);
        let manager = NSFontManager::sharedFontManager(mtm);
        let traits = match descriptor.style {
            crate::appearance::FontStyle::Normal => NSFontTraitMask::UnitalicFontMask,
            crate::appearance::FontStyle::Italic => NSFontTraitMask::ItalicFontMask,
        };
        let weight = match descriptor.weight {
            100..=199 => 1,
            200..=299 => 2,
            300..=399 => 3,
            400..=499 => 5,
            500..=599 => 6,
            600..=699 => 8,
            700..=799 => 9,
            800..=899 => 10,
            _ => 12,
        };
        let font = manager
            .fontWithFamily_traits_weight_size(&requested, traits, weight, f64::from(point_size))
            .or_else(|| NSFont::fontWithName_size(&requested, f64::from(point_size)))
            .or_else(|| {
                manager.fontWithFamily_traits_weight_size(
                    &NSString::from_str("Menlo"),
                    traits,
                    weight,
                    f64::from(point_size),
                )
            })
            .or_else(|| {
                Some(NSFont::monospacedSystemFontOfSize_weight(
                    f64::from(point_size),
                    0.0,
                ))
            })?;
        Some(AccessibilityFontMetadata {
            requested_descriptor: descriptor.clone(),
            requested_family: requested_family.to_owned(),
            requested_point_size: point_size,
            name: font.fontName().to_string(),
            family: font.familyName().map(|name| name.to_string()),
            visible_name: font.displayName().map(|name| name.to_string()),
            point_size: font.pointSize() as f32,
        })
    }
}
#[cfg(any(not(test), feature = "macos-native-tests"))]
fn normalized_font_family(family: &str) -> &str {
    if family.trim().is_empty() {
        "Menlo"
    } else {
        family
    }
}

#[cfg(any(not(test), feature = "macos-native-tests"))]
fn normalized_font_point_size(point_size: f32) -> f32 {
    if point_size.is_finite() && point_size > 0.0 {
        point_size
    } else {
        1.0
    }
}

pub(crate) struct MacosTerminalAccessibilityAdapterFactory;

impl TerminalAccessibilityAdapterFactory for MacosTerminalAccessibilityAdapterFactory {
    fn create(
        &self,
        window: &Window,
        model: TerminalAccessibilityModel,
        font: &crate::appearance::ResolvedFontDescriptor,
        font_size: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter> {
        #[cfg(not(test))]
        {
            Box::new(native::MacosAccessibilityElement::new(
                window, model, font, font_size,
            ))
        }
        #[cfg(test)]
        {
            super::terminal_accessibility::testing::RecordingAccessibilityFactory::default()
                .create(window, model, font, font_size)
        }
    }
}

#[cfg(not(test))]
impl TerminalAccessibilityAdapter for native::MacosAccessibilityElement {
    fn set_hierarchy(&mut self, presented: bool, order: usize) {
        self.set_hierarchy(presented, order);
    }
    fn update(&mut self, update: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications {
        self.update(update)
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
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
            font: Some(AccessibilityFontMetadata {
                requested_descriptor: crate::terminal::test_terminal_appearance_update()
                    .appearance
                    .typography
                    .regular
                    .clone(),
                requested_family: "JetBrainsMono Nerd Font".to_owned(),
                requested_point_size: 14.0,
                name: "JetBrainsMonoNF-Regular".to_owned(),
                family: Some("JetBrainsMono Nerd Font".to_owned()),
                visible_name: Some("JetBrainsMono NF Regular".to_owned()),
                point_size: 14.0,
            }),
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
    fn attributed_text_uses_the_resolved_font_without_expanding_its_range() {
        let state = state();

        assert_eq!(
            state.attributed_text_for_range(1..3),
            Some(AccessibilityAttributedText {
                text: "😀".to_owned(),
                font: state.font.as_ref().unwrap(),
            })
        );
        assert_eq!(state.attributed_text_for_range(1..2), None);
        assert_eq!(state.attributed_text_for_range(0..4), None);
    }

    #[test]
    fn font_metadata_changes_only_with_the_selected_family_or_logical_point_size() {
        let mut state = state();
        state.font.as_mut().unwrap().point_size = 13.5;

        assert!(!state.font_request_changed("JetBrainsMono Nerd Font", 14.0));
        assert!(state.font_request_changed("JetBrainsMono Nerd Font", 15.0));
        assert!(state.font_request_changed("Menlo", 14.0));
        state.cell_width = 20.0;
        state.line_height = 40.0;
        assert!(!state.font_request_changed("JetBrainsMono Nerd Font", 14.0));
        assert_eq!(state.font.as_ref().unwrap().point_size, 13.5);
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
