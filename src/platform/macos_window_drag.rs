use super::window_movement::{OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use gpui::Window;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSView};
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
        let mtm = MainThreadMarker::new().ok_or(OperatingSystemWindowDragError::NativeWindow)?;
        // SAFETY: GPUI owns this live NSView for the duration of the synchronous drag callback.
        let native_view = unsafe { &*native_handle.ns_view.as_ptr().cast::<NSView>() };
        let native_window = native_view
            .window()
            .ok_or(OperatingSystemWindowDragError::NativeWindow)?;
        let event_window = event
            .0
            .window(mtm)
            .ok_or(OperatingSystemWindowDragError::NativeWindow)?;
        if !std::ptr::eq(&*native_window, &*event_window) {
            return Err(OperatingSystemWindowDragError::NativeWindow);
        }
        native_window.performWindowDragWithEvent(&event.0);
        Ok(())
    }

    fn interaction_finished(&self) {
        self.mouse_down.borrow_mut().take();
    }
}

struct RetainedMouseDownEvent(Retained<NSEvent>);

impl RetainedMouseDownEvent {
    fn current() -> Result<Self, OperatingSystemWindowDragError> {
        let mtm = MainThreadMarker::new().ok_or(OperatingSystemWindowDragError::Application)?;
        let event = NSApplication::sharedApplication(mtm)
            .currentEvent()
            .ok_or(OperatingSystemWindowDragError::MouseDownEvent)?;
        if event.r#type() != NSEventType::LeftMouseDown {
            return Err(OperatingSystemWindowDragError::MouseDownEvent);
        }
        Ok(Self(event))
    }
}

pub(super) struct WindowMovementFactory;
impl super::window_movement::WindowMovementFactory for WindowMovementFactory {
    fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
        Rc::new(MacosOperatingSystemWindowDragPlatform::default())
    }
}
