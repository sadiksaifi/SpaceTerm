//! AppKit window geometry used by application-owned content.

use gpui::{point, px};

use super::window_frame::{TrafficLightPlacement, WindowFrameGeometry};

/// Geometry observed for the standard titled window style used by SpaceTerm on macOS 27.
///
/// AppKit exposes effective corner radii for views but no public NSWindow outer-corner query. Keep
/// the observed fallback inside this Adapter until a native source can replace it.
pub(crate) const fn window_frame_geometry() -> WindowFrameGeometry {
    WindowFrameGeometry::new(Some(16.0)).with_traffic_lights(
        TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(42.0)),
        TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0)),
    )
}
