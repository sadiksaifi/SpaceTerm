#[cfg(any(not(test), feature = "native-tests"))]
use std::ops::Range;

#[cfg(any(not(test), feature = "native-tests"))]
use super::terminal_accessibility::TerminalAccessibilityUpdate;
use super::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory,
};
#[cfg(any(not(test), feature = "native-tests"))]
use crate::terminal::AccessibilityNotifications;
use gpui::{Pixels, Window};

use crate::terminal::TerminalAccessibilityModel;
#[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
use super::terminal_accessibility::AccessibilityFocusSender;
#[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
use crate::terminal::{AccessibilityDemandSender, AccessibilitySelectionSender};
#[cfg(any(not(test), feature = "native-tests"))]
use crate::terminal::{AccessibilityGeometry, AccessibilityNotification};

#[cfg(any(not(test), feature = "native-tests"))]
const TEXT_AREA_ROLE: &str = "AXTextArea";
#[cfg(any(not(test), feature = "native-tests"))]
const VALUE_CHANGED: &str = "AXValueChanged";
#[cfg(any(not(test), feature = "native-tests"))]
const SELECTION_CHANGED: &str = "AXSelectedTextChanged";
#[cfg(any(not(test), feature = "native-tests"))]
const FOCUS_CHANGED: &str = "AXFocusedUIElementChanged";

#[cfg(any(not(test), feature = "native-tests"))]
fn notification_name(notification: AccessibilityNotification) -> &'static str {
    match notification {
        AccessibilityNotification::Value => VALUE_CHANGED,
        AccessibilityNotification::Selection => SELECTION_CHANGED,
        AccessibilityNotification::Focus => FOCUS_CHANGED,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg(any(not(test), feature = "native-tests"))]
struct ScreenRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg(any(not(test), feature = "native-tests"))]
impl ScreenRect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Clone, Debug)]
#[cfg(any(not(test), feature = "native-tests"))]
struct AccessibilityElementState {
    model: TerminalAccessibilityModel,
    font: Option<AccessibilityFontMetadata>,
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "native accessibility frame callbacks are excluded from the test build"
        )
    )]
    frame: ScreenRect,
    grid: ScreenRect,
    cell_width: f32,
    line_height: f32,
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "native accessibility focus callbacks are excluded from the test build"
        )
    )]
    focused: bool,
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "native accessibility visibility callbacks are excluded from the test build"
        )
    )]
    visible: bool,
    #[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
    presented: bool,
    #[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
    selection_sender: Option<AccessibilitySelectionSender>,
    #[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
    demand_sender: Option<AccessibilityDemandSender>,
    #[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
    focus_sender: Option<AccessibilityFocusSender>,
    /// The GPUI view whose coordinates place this element on screen.
    #[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
    view: Option<objc2::rc::Retained<objc2_app_kit::NSView>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg(any(not(test), feature = "native-tests"))]
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
#[cfg(any(not(test), feature = "native-tests"))]
struct AccessibilityAttributedText<'a> {
    text: String,
    font: &'a AccessibilityFontMetadata,
}

#[cfg(any(not(test), feature = "native-tests"))]
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

#[cfg(all(target_os = "macos", any(not(test), feature = "native-tests")))]
mod native {
    use std::cell::Cell;
    use std::ops::Range;
    use std::ptr::NonNull;
    use std::rc::Rc;

    use gpui::{
        Bounds, Div, NativeAccessibilityElement, Pixels, Stateful, StatefulInteractiveElement,
        Window, accesskit::Role,
    };
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{
        AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
    };
    use objc2_app_kit::{
        NSAccessibilityElement, NSAccessibilityFontFamilyKey, NSAccessibilityFontNameKey,
        NSAccessibilityFontSizeKey, NSAccessibilityFontTextAttribute,
        NSAccessibilityPostNotification, NSAccessibilityVisibleNameKey, NSView,
    };
    use objc2_foundation::{
        NSAttributedString, NSDictionary, NSInteger, NSNumber, NSObjectProtocol, NSPoint, NSRange,
        NSRect, NSSize, NSString,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        AccessibilityAttributedText, AccessibilityElementState, AccessibilityNotification,
        AccessibilityNotifications, ScreenRect, TEXT_AREA_ROLE, TerminalAccessibilityModel,
        TerminalAccessibilityUpdate, normalized_font_family, notification_name,
        resolve_font_metadata,
    };

    const ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";

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

            #[unsafe(method(isAccessibilityFocused))]
            fn is_accessibility_focused(&self) -> bool {
                state(self).is_some_and(|state| state.focused)
            }

            /// Asks the Pane for focus, which moves Terminal Input Focus as a pointer press does.
            #[unsafe(method(setAccessibilityFocused:))]
            fn set_accessibility_focused(&self, focused: bool) {
                if focused
                    && let Some(sender) = state(self)
                        .filter(|state| state.visible)
                        .and_then(|state| state.focus_sender.as_ref())
                {
                    sender.request();
                }
            }

            #[unsafe(method(isAccessibilitySelectorAllowed:))]
            fn is_accessibility_selector_allowed(&self, selector: Sel) -> bool {
                if selector == sel!(setAccessibilityFocused:) {
                    state(self).is_some_and(|state| state.visible && state.focus_sender.is_some())
                } else {
                    // SAFETY: NSAccessibilityElement implements this NSAccessibility method.
                    unsafe { msg_send![super(self), isAccessibilitySelectorAllowed: selector] }
                }
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
                    .filter(|state| state.visible)
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

    /// Publishes one Pane as a native text area, attached to the Pane's node in GPUI's
    /// accessibility tree while the Pane is presented with bounds.
    pub(crate) struct MacosAccessibilityElement {
        element: Rc<Retained<PaneAccessibilityElement>>,
        state: Box<AccessibilityElementState>,
        #[cfg(all(test, feature = "native-tests"))]
        assumed_on_screen: bool,
    }

    impl MacosAccessibilityElement {
        pub(crate) fn new(
            window: &Window,
            model: TerminalAccessibilityModel,
            font: &crate::appearance::ResolvedFontDescriptor,
            font_size: Pixels,
        ) -> Self {
            let view = native_view(window);
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
                selection_sender: None,
                demand_sender: None,
                focus_sender: None,
                view,
            });
            let pointer = state.as_mut() as *mut AccessibilityElementState;
            let mtm = MainThreadMarker::new()
                .expect("GPUI must create native accessibility on the main thread");
            let element = Rc::new(PaneAccessibilityElement::new(mtm, pointer));
            Self {
                element,
                state,
                #[cfg(all(test, feature = "native-tests"))]
                assumed_on_screen: false,
            }
        }

        /// Hierarchy order follows the Pane's position in GPUI's tree, so only presentation
        /// matters here.
        pub(crate) fn set_hierarchy(&mut self, presented: bool) {
            self.state.presented = presented;
            if !presented {
                self.state.selection_sender = None;
                self.state.demand_sender = None;
                self.state.focus_sender = None;
            }
            self.state.visible &= presented;
            self.state.focused &= presented;
        }

        /// Test windows have no native view to place the Pane on screen, so tests place the
        /// Pane's window bounds on screen unchanged.
        #[cfg(all(test, feature = "native-tests"))]
        pub(super) fn assume_on_screen(&mut self) {
            self.assumed_on_screen = true;
        }

        #[cfg(all(test, feature = "native-tests"))]
        pub(super) fn native_element(&self) -> Retained<AnyObject> {
            Retained::clone(&self.element)
                .into_super()
                .into_super()
                .into_super()
        }

        /// Gives the Pane its node and attaches the native text area while it is visible.
        pub(crate) fn decorate(&self, pane: Stateful<Div>) -> Stateful<Div> {
            if !self.state.presented {
                return pane;
            }
            let element = Rc::clone(&self.element);
            // The Pane's node groups the text area the way an unlabeled scroll area groups
            // Terminal's. Focus moves to the text area while the Pane holds Terminal Input Focus.
            pane.role(Role::Group)
                .a11y_synthetic_children(move |builder| {
                    // Updates run while the Pane prepaints, before GPUI collects its children.
                    if !state(&element).is_some_and(|state| state.visible) {
                        return;
                    }
                    let pointer = NonNull::from(&***element).cast();
                    // SAFETY: `owner` retains this NSAccessibility object while GPUI holds it.
                    let native = unsafe { NativeAccessibilityElement::new(pointer, element) };
                    builder.attach_native_children([native]);
                })
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
                focus_sender,
            } = update;
            let was_focused = self.state.focused;
            self.state.view = native_view(window);
            if !self.state.model.shares_snapshot(model) {
                self.state.model = model.clone();
            }
            self.state.selection_sender = selection_sender.filter(|_| self.state.presented);
            self.state.demand_sender = demand_sender.filter(|_| self.state.presented);
            self.state.focus_sender = focus_sender.filter(|_| self.state.presented);
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
                #[cfg(all(test, feature = "native-tests"))]
                if self.assumed_on_screen {
                    return Some(ScreenRect {
                        x: f64::from(bounds.origin.x),
                        y: f64::from(bounds.origin.y),
                        width: f64::from(bounds.size.width),
                        height: f64::from(bounds.size.height),
                    });
                }
                self.state
                    .view
                    .as_deref()
                    .and_then(|view| screen_rect(view, bounds))
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
            let attached = self.state.visible;
            self.element.ivars().state.set(std::ptr::null_mut());
            if attached {
                post_native_notification(&self.element, ELEMENT_DESTROYED);
            }
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
        // SAFETY: The owner keeps its Box stable until Drop clears the pointer, and native
        // callbacks run on the main thread that owns both.
        unsafe { this.ivars().state.get().as_ref() }
    }

    fn semantic_state(this: &PaneAccessibilityElement) -> Option<&AccessibilityElementState> {
        let state = state(this)?;
        if state.visible
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
}
#[cfg(any(not(test), feature = "native-tests"))]
fn resolve_font_metadata(
    descriptor: &crate::appearance::ResolvedFontDescriptor,
    point_size: f32,
) -> Option<AccessibilityFontMetadata> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSFont, NSFontManager, NSFontTraitMask};
    use objc2_foundation::NSString;

    let requested_family = normalized_font_family(&descriptor.primary_family);
    let point_size = normalized_font_point_size(point_size);
    if requested_family == crate::bundled_font::FAMILY {
        let (name, display_name) = bundled_face_metadata(
            descriptor.weight,
            descriptor.style == crate::appearance::FontStyle::Italic,
        );
        return Some(AccessibilityFontMetadata {
            requested_descriptor: descriptor.clone(),
            requested_family: requested_family.to_owned(),
            requested_point_size: point_size,
            name: name.to_owned(),
            family: Some(requested_family.to_owned()),
            visible_name: Some(display_name.to_owned()),
            point_size,
        });
    }
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

#[cfg(any(not(test), feature = "native-tests"))]
fn normalized_font_family(family: &str) -> &str {
    if family.trim().is_empty() {
        "Menlo"
    } else {
        family
    }
}

#[cfg(any(not(test), feature = "native-tests"))]
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

#[cfg(any(not(test), feature = "native-tests"))]
impl TerminalAccessibilityAdapter for native::MacosAccessibilityElement {
    fn set_hierarchy(&mut self, presented: bool) {
        self.set_hierarchy(presented);
    }
    fn update(&mut self, update: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications {
        self.update(update)
    }
    fn decorate(&self, pane: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
        self.decorate(pane)
    }
}

/// Return the bundled family's PostScript and display names, matching the renderer's CSS
/// weight search.
#[cfg(any(not(test), feature = "native-tests"))]
fn bundled_face_metadata(weight: u16, italic: bool) -> (&'static str, &'static str) {
    match (weight > 500, italic) {
        (false, false) => ("SpaceTermDefault-Regular", "SpaceTerm Default Regular"),
        (true, false) => ("SpaceTermDefault-Bold", "SpaceTerm Default Bold"),
        (false, true) => ("SpaceTermDefault-Italic", "SpaceTerm Default Italic"),
        (true, true) => (
            "SpaceTermDefault-BoldItalic",
            "SpaceTerm Default Bold Italic",
        ),
    }
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) mod tests {
    use super::*;
    use crate::terminal::{AccessibilityCell, AccessibilityLine};

    pub(crate) fn bundled_font_metadata_does_not_require_system_installation() {
        use objc2_app_kit::NSFont;
        use objc2_foundation::NSString;

        assert!(
            NSFont::fontWithName_size(&NSString::from_str("SpaceTermDefault-Regular"), 18.0)
                .is_none()
        );
        let mut descriptor = crate::terminal::test_terminal_appearance_update()
            .appearance
            .typography
            .regular
            .clone();
        descriptor.primary_family = "SpaceTerm Default".to_owned();
        for (weight, style, name) in [
            (
                400,
                crate::appearance::FontStyle::Normal,
                "SpaceTermDefault-Regular",
            ),
            (
                700,
                crate::appearance::FontStyle::Normal,
                "SpaceTermDefault-Bold",
            ),
            (
                400,
                crate::appearance::FontStyle::Italic,
                "SpaceTermDefault-Italic",
            ),
            (
                700,
                crate::appearance::FontStyle::Italic,
                "SpaceTermDefault-BoldItalic",
            ),
        ] {
            descriptor.weight = weight;
            descriptor.style = style;
            let metadata = resolve_font_metadata(&descriptor, 18.0).unwrap();
            assert_eq!(metadata.name, name);
            assert_eq!(metadata.family.as_deref(), Some("SpaceTerm Default"));
            assert_eq!(metadata.point_size, 18.0);
        }
    }

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
            presented: true,
            selection_sender: None,
            demand_sender: None,
            focus_sender: None,
            view: None,
        }
    }

    #[test]
    fn pane_state_exposes_utf16_selection_and_screen_geometry() {
        let state = state();

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
    fn font_request_comparison_uses_normalized_family_and_logical_point_size() {
        let mut state = state();
        state.font.as_mut().unwrap().point_size = 13.5;

        assert!(!state.font_request_changed("JetBrainsMono Nerd Font", 14.0));
        assert!(state.font_request_changed("JetBrainsMono Nerd Font", 15.0));
        assert!(state.font_request_changed("Menlo", 14.0));
        state.cell_width = 20.0;
        state.line_height = 40.0;
        assert!(!state.font_request_changed("JetBrainsMono Nerd Font", 14.0));
        state.font = None;
        assert!(state.font_request_changed("JetBrainsMono Nerd Font", 14.0));
        let mut state = super::tests::state();
        let font = state.font.as_mut().unwrap();
        font.requested_family = "Menlo".to_owned();
        font.requested_point_size = 1.0;
        for family in ["", " \t", "Menlo"] {
            for size in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0] {
                assert!(
                    !state.font_request_changed(family, size),
                    "{family:?}, {size}"
                );
            }
        }
        assert!(state.font_request_changed("Menlo", 2.0));
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
            [
                "AXValueChanged",
                "AXSelectedTextChanged",
                "AXFocusedUIElementChanged"
            ]
        );
        assert_eq!(TEXT_AREA_ROLE, "AXTextArea");
    }

    /// Builds native text areas that the test window places on screen, and keeps each one so
    /// tests can call it the way an accessibility client does.
    #[derive(Default)]
    struct OnScreenAccessibilityFactory {
        elements: std::cell::RefCell<Vec<objc2::rc::Retained<objc2::runtime::AnyObject>>>,
    }

    impl TerminalAccessibilityAdapterFactory for OnScreenAccessibilityFactory {
        fn create(
            &self,
            window: &Window,
            model: TerminalAccessibilityModel,
            font: &crate::appearance::ResolvedFontDescriptor,
            font_size: Pixels,
        ) -> Box<dyn TerminalAccessibilityAdapter> {
            let mut element = native::MacosAccessibilityElement::new(window, model, font, font_size);
            element.assume_on_screen();
            self.elements.borrow_mut().push(element.native_element());
            Box::new(element)
        }
    }

    fn terminal_pane(
        factory: &OnScreenAccessibilityFactory,
        window: &mut Window,
        cx: &mut gpui::Context<crate::ui::TerminalPane>,
    ) -> crate::ui::TerminalPane {
        let session_factory = crate::terminal::WorkspaceTerminalSessionFactory::new_local(
            std::rc::Rc::new(
                crate::terminal::testing::TestTerminalSessionFactory::new(
                    crate::terminal::testing::TestTerminalSessionRecords::default(),
                )
                .with_start_failure("terminal session unavailable in accessibility test"),
            ),
            crate::terminal::testing::test_local_directory(std::path::PathBuf::from(
                "/tmp/spaceterm-accessibility-test",
            )),
        );
        crate::ui::TerminalPane::new_with_prepared_launch(
            session_factory.clone(),
            session_factory.prepare_child_launch().unwrap(),
            crate::terminal::testing::test_terminal_key_input_adapter(),
            factory,
            crate::terminal::native_services::testing::adapters(),
            crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
            window,
            cx,
        )
    }

    fn native_children(cx: &mut gpui::VisualTestContext) -> Vec<(String, u64)> {
        cx.run_until_parked();
        let tree: serde_json::Value = cx.update(|window, _| {
            serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap()
        });
        tree["nodes"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|node| {
                Some((
                    node["aria"]["role"].as_str()?.to_owned(),
                    node["native_children"].as_u64()?,
                ))
            })
            .collect()
    }

    pub(crate) fn presented_pane_attaches_its_text_area_to_the_pane_node(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let factory = OnScreenAccessibilityFactory::default();
        let (pane, cx) = cx.add_window_view(|window, cx| terminal_pane(&factory, window, cx));
        cx.activate_accessibility();
        assert!(native_children(cx).is_empty());

        pane.update(cx, |pane, cx| {
            pane.set_accessibility_hierarchy(true);
            cx.notify();
        });
        assert_eq!(native_children(cx), [("Group".to_owned(), 1)]);

        pane.update(cx, |pane, cx| {
            pane.set_accessibility_hierarchy(false);
            cx.notify();
        });
        assert!(native_children(cx).is_empty());

        pane.update(cx, |pane, cx| {
            pane.set_accessibility_hierarchy(true);
            cx.notify();
        });
        assert_eq!(native_children(cx), [("Group".to_owned(), 1)]);
    }

    struct SplitPanes([gpui::Entity<crate::ui::TerminalPane>; 2]);

    impl gpui::Render for SplitPanes {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            gpui::div().flex().size_full().children(
                self.0
                    .iter()
                    .map(|pane| gpui::div().flex_1().h_full().child(pane.clone())),
            )
        }
    }

    pub(crate) fn text_area_focus_request_focuses_its_pane(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use objc2::{msg_send, sel};

        cx.update(crate::ui::init).unwrap();
        let factory = OnScreenAccessibilityFactory::default();
        let (split, cx) = cx.add_window_view(|window, cx| {
            SplitPanes([0, 1].map(|_| cx.new(|cx| terminal_pane(&factory, window, cx))))
        });
        let panes = split.read_with(cx, |split, _| split.0.clone());
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|window, cx| {
            for (index, pane) in panes.iter().enumerate() {
                let requests = std::rc::Rc::clone(&requests);
                cx.subscribe(pane, move |_, event, _| {
                    if *event == crate::ui::TerminalPaneEvent::FocusRequested {
                        requests.borrow_mut().push(index);
                    }
                })
                .detach();
                pane.update(cx, |pane, cx| {
                    pane.set_accessibility_hierarchy(true);
                    cx.notify();
                });
            }
            panes[0].update(cx, |pane, cx| pane.focus(window, cx));
        });
        cx.run_until_parked();
        let second = factory.elements.borrow()[1].clone();
        let focused = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| panes.each_ref().map(|pane| pane.read(cx).is_focused(window)))
        };
        assert_eq!(focused(cx), [true, false]);
        // SAFETY: The selector is part of NSAccessibility and the element is live.
        let allowed: bool = unsafe {
            msg_send![&*second, isAccessibilitySelectorAllowed: sel!(setAccessibilityFocused:)]
        };
        assert!(allowed);

        // SAFETY: The selector is part of NSAccessibility and the element is live.
        let _: () = unsafe { msg_send![&*second, setAccessibilityFocused: false] };
        cx.run_until_parked();
        assert!(requests.borrow().is_empty());
        assert_eq!(focused(cx), [true, false]);

        // SAFETY: The selector is part of NSAccessibility and the element is live.
        let _: () = unsafe { msg_send![&*second, setAccessibilityFocused: true] };
        cx.run_until_parked();
        assert_eq!(*requests.borrow(), [1]);
        assert_eq!(focused(cx), [false, true]);

        let first = factory.elements.borrow()[0].clone();
        panes[0].update(cx, |pane, cx| {
            pane.set_accessibility_hierarchy(false);
            cx.notify();
        });
        cx.run_until_parked();
        // SAFETY: The selector is part of NSAccessibility and the element is live.
        let _: () = unsafe { msg_send![&*first, setAccessibilityFocused: true] };
        cx.run_until_parked();
        assert_eq!(*requests.borrow(), [1]);
        assert_eq!(focused(cx), [false, true]);
    }
}
