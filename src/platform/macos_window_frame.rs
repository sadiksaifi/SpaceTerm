//! AppKit window geometry used by application-owned content.

use super::window_frame::WindowFrameGeometry;

/// Geometry observed for the standard titled window style used by SpaceTerm on macOS 27.
///
/// AppKit exposes effective corner radii for views but no public NSWindow outer-corner query. Keep
/// the observed fallback inside this Adapter until a native source can replace it.
pub(crate) const fn workspace_window_frame_geometry() -> WindowFrameGeometry {
    WindowFrameGeometry::new(Some(16.0))
}
