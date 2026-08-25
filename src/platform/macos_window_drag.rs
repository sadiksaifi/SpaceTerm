#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use cocoa::appkit::{NSApp, NSEvent, NSEventType};
use cocoa::base::{id, nil};
use gpui::Window;
use objc::runtime::Object;
use objc::{msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum OperatingSystemWindowDragError {
    #[error("the AppKit application is unavailable")]
    Application,
    #[error("the current AppKit event is not a primary mouse-down")]
    MouseDownEvent,
    #[error("the GPUI Operating-System Window view is unavailable")]
    NativeView,
    #[error("the AppKit Operating-System Window is unavailable")]
    NativeWindow,
}

pub(crate) trait OperatingSystemWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError>;
    fn start_window_move(&self, window: &Window) -> Result<(), OperatingSystemWindowDragError>;
    fn interaction_finished(&self);
    fn double_activation_requested(&self, window: &Window);
}

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
            if native_window == nil {
                return Err(OperatingSystemWindowDragError::NativeWindow);
            }
            let _: () = msg_send![native_window, performWindowDragWithEvent: event.0];
        }
        Ok(())
    }

    fn interaction_finished(&self) {
        self.mouse_down.borrow_mut().take();
    }

    fn double_activation_requested(&self, window: &Window) {
        window.titlebar_double_click();
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

#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingOperatingSystemWindowDragPlatform {
    interaction_starts: Cell<usize>,
    move_requests: Cell<usize>,
    interaction_finishes: Cell<usize>,
    double_activations: Cell<usize>,
}

#[cfg(test)]
impl RecordingOperatingSystemWindowDragPlatform {
    pub(crate) fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.interaction_starts.get(),
            self.move_requests.get(),
            self.interaction_finishes.get(),
            self.double_activations.get(),
        )
    }
}

#[cfg(test)]
impl OperatingSystemWindowDragPlatform for RecordingOperatingSystemWindowDragPlatform {
    fn interaction_started(&self) -> Result<(), OperatingSystemWindowDragError> {
        self.interaction_starts
            .set(self.interaction_starts.get() + 1);
        Ok(())
    }

    fn start_window_move(&self, _: &Window) -> Result<(), OperatingSystemWindowDragError> {
        self.move_requests.set(self.move_requests.get() + 1);
        Ok(())
    }

    fn interaction_finished(&self) {
        self.interaction_finishes
            .set(self.interaction_finishes.get() + 1);
    }

    fn double_activation_requested(&self, _: &Window) {
        self.double_activations
            .set(self.double_activations.get() + 1);
    }
}
