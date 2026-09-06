use super::window_movement::{OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use cocoa::appkit::{NSApp, NSEvent, NSEventType};
use cocoa::base::{id, nil};
use gpui::Window;
use objc::runtime::Object;
use objc::{msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub(crate) struct MacosOperatingSystemWindowDragPlatform {
    mouse_down: RefCell<Option<RetainedMouseDownEvent>>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl Default for MacosOperatingSystemWindowDragPlatform {
    fn default() -> Self {
        Self {
            mouse_down: RefCell::new(None),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl OperatingSystemWindowDragPlatform for MacosOperatingSystemWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError> {
        *self.mouse_down.borrow_mut() = Some(RetainedMouseDownEvent::current()?);
        Ok(())
    }

    fn start_window_move(&self, window: &Window) -> Result<(), OperatingSystemWindowDragError> {
        let event = self
            .mouse_down
            .borrow_mut()
            .take()
            .ok_or(OperatingSystemWindowDragError::MouseDownEvent)?;
        let native_handle = HasWindowHandle::window_handle(window)
            .map_err(|_| OperatingSystemWindowDragError::NativeView)?;
        let RawWindowHandle::AppKit(native_handle) = native_handle.as_raw() else {
            return Err(OperatingSystemWindowDragError::NativeView);
        };
        let native_view = native_handle.ns_view.as_ptr().cast::<Object>();

        // SAFETY: GPUI supplies a live NSView for this synchronous AppKit-thread call. AppKit owns
        // the NSWindow returned by the view, and `event` retains the original primary mouse-down
        // through the complete `performWindowDragWithEvent:` handoff.
        unsafe {
            let native_window: id = msg_send![native_view, window];
            let event_window: id = msg_send![event.0, window];
            if native_window == nil || native_window != event_window {
                return Err(OperatingSystemWindowDragError::NativeWindow);
            }
            let _: () = msg_send![native_window, performWindowDragWithEvent: event.0];
        }
        Ok(())
    }

    fn interaction_finished(&self) {
        self.mouse_down.borrow_mut().take();
    }
}

struct RetainedMouseDownEvent(id);

impl RetainedMouseDownEvent {
    fn current() -> Result<Self, OperatingSystemWindowDragError> {
        // SAFETY: WindowDragRegion invokes this synchronously on GPUI's AppKit thread while AppKit
        // dispatches the corresponding NSEvent. The explicit retain balances this type's Drop.
        unsafe {
            let application = NSApp();
            if application == nil {
                return Err(OperatingSystemWindowDragError::Application);
            }
            let event: id = msg_send![application, currentEvent];
            if event == nil || event.eventType() != NSEventType::NSLeftMouseDown {
                return Err(OperatingSystemWindowDragError::MouseDownEvent);
            }
            let event: id = msg_send![event, retain];
            Ok(Self(event))
        }
    }
}

impl Drop for RetainedMouseDownEvent {
    fn drop(&mut self) {
        // SAFETY: `current` stored one owned Objective-C retain and Drop runs on the GPUI thread
        // because the platform adapter is application UI state and is deliberately !Send/!Sync.
        unsafe {
            let _: () = msg_send![self.0, release];
        }
    }
}

pub(super) struct WindowMovementFactory;
impl super::window_movement::WindowMovementFactory for WindowMovementFactory {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(MacosOperatingSystemWindowDragPlatform::default())
    }
}
