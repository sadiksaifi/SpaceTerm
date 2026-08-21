#[cfg(not(test))]
use cocoa::appkit::NSApp;
#[cfg(not(test))]
use cocoa::base::{id, nil};
#[cfg(not(test))]
use objc::{msg_send, sel, sel_impl};

const NS_WINDOW_OCCLUSION_STATE_VISIBLE: u64 = 1 << 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeWindowVisibility {
    pub(crate) minimized: bool,
    pub(crate) occluded: bool,
    pub(crate) live_resize: bool,
}

pub(crate) struct NativeWindowVisibilitySource {
    #[cfg(not(test))]
    window: id,
}

impl NativeWindowVisibilitySource {
    pub(crate) fn capture() -> Option<Self> {
        #[cfg(not(test))]
        // SAFETY: observation claims the production Pane during AppKit layout on the main thread.
        // Retaining that Pane's exact native window keeps later minimize/occlusion polls from
        // accidentally following a different main window during activation changes.
        unsafe {
            let window: id = msg_send![NSApp(), mainWindow];
            if window == nil {
                return None;
            }
            let _: id = msg_send![window, retain];
            Some(Self { window })
        }
        #[cfg(test)]
        {
            Some(Self {})
        }
    }

    pub(crate) fn current(&self) -> NativeWindowVisibility {
        #[cfg(not(test))]
        // SAFETY: the source retains this NSWindow and is queried synchronously from GPUI's main
        // thread by the observation-only monitor.
        unsafe {
            let minimized: bool = msg_send![self.window, isMiniaturized];
            let occlusion_state: u64 = msg_send![self.window, occlusionState];
            let live_resize: bool = msg_send![self.window, inLiveResize];
            from_native(minimized, occlusion_state, live_resize)
        }
        #[cfg(test)]
        {
            from_native(false, NS_WINDOW_OCCLUSION_STATE_VISIBLE, false)
        }
    }
}

#[cfg(not(test))]
impl Drop for NativeWindowVisibilitySource {
    fn drop(&mut self) {
        // SAFETY: capture retained this exact NSWindow and TerminalPane drops the source on GPUI's
        // main thread.
        unsafe {
            let _: () = msg_send![self.window, release];
        }
    }
}

fn from_native(minimized: bool, occlusion_state: u64, live_resize: bool) -> NativeWindowVisibility {
    NativeWindowVisibility {
        minimized,
        occluded: occlusion_state & NS_WINDOW_OCCLUSION_STATE_VISIBLE == 0,
        live_resize,
    }
}

#[cfg(not(test))]
pub(crate) fn current_window_visibility() -> NativeWindowVisibility {
    // SAFETY: GPUI calls this from AppKit's main thread and the borrowed main window is queried
    // synchronously without retaining it.
    unsafe {
        let application = NSApp();
        let window: id = msg_send![application, mainWindow];
        if window == nil {
            return NativeWindowVisibility {
                minimized: false,
                occluded: true,
                live_resize: false,
            };
        }
        let minimized: bool = msg_send![window, isMiniaturized];
        let occlusion_state: u64 = msg_send![window, occlusionState];
        let live_resize: bool = msg_send![window, inLiveResize];
        from_native(minimized, occlusion_state, live_resize)
    }
}

#[cfg(test)]
pub(crate) fn current_window_visibility() -> NativeWindowVisibility {
    from_native(false, NS_WINDOW_OCCLUSION_STATE_VISIBLE, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_minimize_and_occlusion_bits_are_independent() {
        assert_eq!(
            from_native(false, NS_WINDOW_OCCLUSION_STATE_VISIBLE, false),
            NativeWindowVisibility {
                minimized: false,
                occluded: false,
                live_resize: false,
            }
        );
        assert_eq!(
            from_native(true, 0, true),
            NativeWindowVisibility {
                minimized: true,
                occluded: true,
                live_resize: true,
            }
        );
    }
}
