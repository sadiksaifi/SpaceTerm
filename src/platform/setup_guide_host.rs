//! Portable seam for the native pieces a Setup Guide presents on System Settings.
//!
//! The guide follows System Settings' window and offers SpaceTerm itself for a person to drag into
//! a privacy list. Locating the window reads only window geometry and owners, which needs no
//! permission.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Bounds, DisplayId, Pixels};

/// Where System Settings presents the list a person drops SpaceTerm onto.
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// The running application as a person drags it into a System Settings list.
#[derive(Clone)]
pub(crate) struct ApplicationBundle {
    /// The running application's bundle. It is only dragged, so it carries no authority for any
    /// other file action.
    pub(crate) path: PathBuf,
    /// The application's icon as the system presents it.
    pub(crate) icon: Arc<gpui::Image>,
}

pub(crate) trait SetupGuideHost: Send + Sync {
    /// Locates System Settings' window. It may be called from any thread.
    fn locate_system_settings(&self) -> SystemSettingsWindow;

    /// The running application's bundle, or `None` when it runs outside one. It is called on the
    /// main thread.
    fn application_bundle(&self) -> Option<ApplicationBundle>;

    /// Places the Operating System's glass material behind the guide's content, rounded to
    /// `corner_radius`, so the guide floats on System Settings like part of it. Returns `false`
    /// when the host has no such material; the guide then paints its own surface. It is called on
    /// the main thread, once for each guide window.
    fn install_glass(&self, window: &gpui::Window, corner_radius: Pixels) -> bool;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A host whose System Settings window and application bundle the test controls.
    pub(crate) struct ScriptedSetupGuideHost {
        window: Mutex<SystemSettingsWindow>,
        bundle: Option<ApplicationBundle>,
        glass: AtomicUsize,
    }

    impl ScriptedSetupGuideHost {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                window: Mutex::new(SystemSettingsWindow::Closed),
                bundle: Some(ApplicationBundle {
                    path: PathBuf::from("/Applications/SpaceTerm.app"),
                    icon: Arc::new(gpui::Image::from_bytes(
                        gpui::ImageFormat::Png,
                        Vec::new(),
                    )),
                }),
                glass: AtomicUsize::new(0),
            })
        }

        pub(crate) fn set_window(&self, window: SystemSettingsWindow) {
            *self.window.lock().expect("window lock") = window;
        }

        /// How many guide windows asked for glass.
        pub(crate) fn glass_requests(&self) -> usize {
            self.glass.load(Ordering::Relaxed)
        }
    }

    impl SetupGuideHost for ScriptedSetupGuideHost {
        fn locate_system_settings(&self) -> SystemSettingsWindow {
            *self.window.lock().expect("window lock")
        }

        fn application_bundle(&self) -> Option<ApplicationBundle> {
            self.bundle.clone()
        }

        fn install_glass(&self, _: &gpui::Window, _: Pixels) -> bool {
            self.glass.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}
