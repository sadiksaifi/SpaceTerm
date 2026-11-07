//! Portable seam for the native pieces a Setup Guide presents on System Settings.
//!
//! Locating System Settings' window reads only window geometry and owners, which needs no
//! permission.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Bounds, DisplayId, Pixels, Rgba, Size};

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
        /// The window's frame, relative to the top-left corner of `display`.
        window: Bounds<Pixels>,
    },
}

/// The bundle that holds the server's grants, as a person drags it into a System Settings list.
#[derive(Clone)]
pub(crate) struct ApplicationBundle {
    /// The bundle's path. It is only dragged, so it carries no authority for any other file
    /// action.
    pub(crate) path: PathBuf,
    pub(crate) icon: Arc<gpui::Image>,
}

/// A copy of the guide's application row for the host to draw as the drag image, so a person
/// seems to drag the row itself into the list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ApplicationRowImage {
    pub(crate) size: Size<Pixels>,
    /// Device pixels for each point.
    pub(crate) scale: f32,
    pub(crate) corner_radius: Pixels,
    pub(crate) fill: Rgba,
    pub(crate) border: Rgba,
    /// The space before the icon.
    pub(crate) padding: Pixels,
    /// The space between the icon and the name.
    pub(crate) gap: Pixels,
    pub(crate) icon_size: Pixels,
    pub(crate) name: String,
    pub(crate) name_color: Rgba,
    pub(crate) font_size: Pixels,
    /// The name's weight, from 100 to 900.
    pub(crate) font_weight: f32,
}

pub(crate) trait SetupGuideHost: Send + Sync {
    /// Locates System Settings' window. It may be called from any thread.
    fn locate_system_settings(&self) -> SystemSettingsWindow;

    /// The application bundle at `path` with its icon, or `None` when `path` holds no
    /// application bundle. It is called on the main thread.
    fn application_bundle(&self, path: &Path) -> Option<ApplicationBundle>;

    /// Keeps mouse interaction with the guide from taking keyboard focus and places native glass
    /// behind its content, rounded to `corner_radius`. Returns whether glass was installed; the
    /// guide paints its own surface otherwise. Called on the main thread once per guide window.
    fn configure_window(&self, window: &gpui::Window, corner_radius: Pixels) -> bool;

    /// Draws `row` with the icon of the bundle at `bundle`, or returns `None` when the host
    /// cannot; the drag then shows a file icon alone. It is called on the main thread.
    fn draw_application_row(
        &self,
        bundle: &Path,
        row: &ApplicationRowImage,
    ) -> Option<Arc<gpui::Image>>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A host whose System Settings window the test controls. Any path ending in `.app` holds an
    /// application bundle.
    pub(crate) struct ScriptedSetupGuideHost {
        window: Mutex<SystemSettingsWindow>,
        glass: AtomicUsize,
        rows: Mutex<Vec<(PathBuf, ApplicationRowImage)>>,
    }

    impl ScriptedSetupGuideHost {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                window: Mutex::new(SystemSettingsWindow::Closed),
                glass: AtomicUsize::new(0),
                rows: Mutex::new(Vec::new()),
            })
        }

        pub(crate) fn set_window(&self, window: SystemSettingsWindow) {
            *self.window.lock().expect("window lock") = window;
        }

        pub(crate) fn glass_requests(&self) -> usize {
            self.glass.load(Ordering::Relaxed)
        }

        pub(crate) fn drawn_rows(&self) -> Vec<(PathBuf, ApplicationRowImage)> {
            self.rows.lock().expect("rows lock").clone()
        }
    }

    impl SetupGuideHost for ScriptedSetupGuideHost {
        fn locate_system_settings(&self) -> SystemSettingsWindow {
            *self.window.lock().expect("window lock")
        }

        fn application_bundle(&self, path: &Path) -> Option<ApplicationBundle> {
            path.extension()
                .is_some_and(|extension| extension == "app")
                .then(|| ApplicationBundle {
                    path: path.to_owned(),
                    icon: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, Vec::new())),
                })
        }

        fn configure_window(&self, _: &gpui::Window, _: Pixels) -> bool {
            self.glass.fetch_add(1, Ordering::Relaxed);
            false
        }

        fn draw_application_row(
            &self,
            bundle: &Path,
            row: &ApplicationRowImage,
        ) -> Option<Arc<gpui::Image>> {
            self.rows
                .lock()
                .expect("rows lock")
                .push((bundle.to_owned(), row.clone()));
            Some(Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                Vec::new(),
            )))
        }
    }
}
