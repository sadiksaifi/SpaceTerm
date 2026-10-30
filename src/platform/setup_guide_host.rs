//! Portable seam for the native pieces a Setup Guide presents on System Settings.
//!
//! Locating System Settings' window reads only window geometry and owners, which needs no
//! permission.

use gpui::{Bounds, DisplayId, Pixels};

/// Where System Settings presents the list that holds the server's entry.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only the macOS Setup Guide host produces System Settings window observations in production"
    )
)]
pub(crate) enum SystemSettingsWindow {
    /// System Settings is not running.
    Closed,
    /// System Settings runs, but it shows no window or another application's window is in front.
    Covered,
    /// System Settings' window is in front of every other application's window.
    Frontmost {
        display: DisplayId,
        /// The frame of the window's content column, which holds the privacy list, relative to
        /// the top-left corner of `display`.
        content: Bounds<Pixels>,
    },
}

pub(crate) trait SetupGuideHost: Send + Sync {
    /// Locates System Settings' window. It may be called from any thread.
    fn locate_system_settings(&self) -> SystemSettingsWindow;

    /// Keeps mouse interaction with the guide from taking keyboard focus and places native glass
    /// behind its content, rounded to `corner_radius`. Returns whether glass was installed; the
    /// guide paints its own surface otherwise. Called on the main thread once per guide window.
    fn configure_window(&self, window: &gpui::Window, corner_radius: Pixels) -> bool;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A host whose System Settings window the test controls.
    pub(crate) struct ScriptedSetupGuideHost {
        window: Mutex<SystemSettingsWindow>,
        glass: AtomicUsize,
    }

    impl ScriptedSetupGuideHost {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                window: Mutex::new(SystemSettingsWindow::Closed),
                glass: AtomicUsize::new(0),
            })
        }

        pub(crate) fn set_window(&self, window: SystemSettingsWindow) {
            *self.window.lock().expect("window lock") = window;
        }

        pub(crate) fn glass_requests(&self) -> usize {
            self.glass.load(Ordering::Relaxed)
        }
    }

    impl SetupGuideHost for ScriptedSetupGuideHost {
        fn locate_system_settings(&self) -> SystemSettingsWindow {
            *self.window.lock().expect("window lock")
        }

        fn configure_window(&self, _: &gpui::Window, _: Pixels) -> bool {
            self.glass.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}
