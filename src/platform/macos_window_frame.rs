//! AppKit window geometry used by application-owned content.

use gpui::{point, px};

use super::window_frame::{TrafficLightPlacement, WindowFrameGeometry};

/// Geometry observed for the standard titled window style used by SpaceTerm on macOS 27.
///
/// AppKit exposes effective corner radii for views but no public NSWindow outer-corner or
/// outer-edge query. Keep the observed fallbacks inside this Adapter until a native source can
/// replace them. The window draws its one-point edge over the outermost point of its content.
pub(crate) const fn window_frame_geometry() -> WindowFrameGeometry {
    WindowFrameGeometry::new(Some(16.0))
        .with_outer_edge_width(1.0)
        .with_traffic_lights(
            TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(41.0)),
            TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0)),
        )
}
