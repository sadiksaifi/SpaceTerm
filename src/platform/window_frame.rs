//! Immutable geometry supplied by the host for application-owned window content and native
//! titlebar controls.

use gpui::{Pixels, Point, Window, point, px};

/// A native traffic-light position anchored to one client titlebar height.
///
/// The native buttons do not scale with application Chrome. Moving the anchor by half of the
/// client-height delta keeps their center aligned when a window opens at another density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrafficLightPlacement {
    compact_position: Point<Pixels>,
    compact_titlebar_height: Pixels,
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only desktops with native window controls place them")
)]
impl TrafficLightPlacement {
    pub(crate) const fn new(
        compact_position: Point<Pixels>,
        compact_titlebar_height: Pixels,
    ) -> Self {
        Self {
            compact_position,
            compact_titlebar_height,
        }
    }

    fn resolve(self, titlebar_height: Pixels) -> Point<Pixels> {
        let delta = (f32::from(titlebar_height) - f32::from(self.compact_titlebar_height)) / 2.0;
        point(self.compact_position.x, self.compact_position.y + px(delta))
    }
}

/// Moves the native traffic lights of `window`. Only hosts that supply a
/// [`TrafficLightPlacement`] produce a position, so other hosts never reach a native effect.
pub(crate) fn place_traffic_lights(window: &Window, position: Point<Pixels>) {
    window.set_traffic_light_position(position);
}

/// Geometry the application needs to make inset surfaces follow their hosting window.
///
/// The host supplies this fact during composition. Shared UI can fall back when a platform cannot
/// resolve it, without selecting an Adapter or querying the Operating System itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WindowFrameGeometry {
    outer_corner_radius: Option<f32>,
    outer_edge_width: f32,
    workspace_traffic_lights: Option<TrafficLightPlacement>,
    sidebar_window_traffic_lights: Option<TrafficLightPlacement>,
}

impl WindowFrameGeometry {
    pub(crate) const fn new(outer_corner_radius: Option<f32>) -> Self {
        Self {
            outer_corner_radius,
            outer_edge_width: 0.0,
            workspace_traffic_lights: None,
            sidebar_window_traffic_lights: None,
        }
    }

    /// Records the edge the window paints over the outermost points of its own content.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only desktops with native window controls describe frame placement"
        )
    )]
    pub(crate) const fn with_outer_edge_width(mut self, width: f32) -> Self {
        self.outer_edge_width = width;
        self
    }

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only desktops with native window controls place them"
        )
    )]
    pub(crate) const fn with_traffic_lights(
        mut self,
        workspace: TrafficLightPlacement,
        sidebar_window: TrafficLightPlacement,
    ) -> Self {
        self.workspace_traffic_lights = Some(workspace);
        self.sidebar_window_traffic_lights = Some(sidebar_window);
        self
    }

    pub(crate) const fn outer_corner_radius(self) -> Option<f32> {
        self.outer_corner_radius
    }

    /// Width of the window's own edge inside its content bounds; zero when the host paints none.
    pub(crate) const fn outer_edge_width(self) -> f32 {
        self.outer_edge_width
    }

    pub(crate) fn workspace_traffic_light_position(
        self,
        titlebar_height: Pixels,
    ) -> Option<Point<Pixels>> {
        self.workspace_traffic_lights
            .map(|placement| placement.resolve(titlebar_height))
    }

    pub(crate) fn sidebar_window_traffic_light_position(
        self,
        titlebar_height: Pixels,
    ) -> Option<Point<Pixels>> {
        self.sidebar_window_traffic_lights
            .map(|placement| placement.resolve(titlebar_height))
    }
}

impl gpui::Global for WindowFrameGeometry {}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> WindowFrameGeometry {
        WindowFrameGeometry::new(Some(16.0)).with_traffic_lights(
            TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(42.0)),
            TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0)),
        )
    }

    #[test]
    fn each_window_should_preserve_its_compact_traffic_light_position() {
        let geometry = geometry();

        assert_eq!(
            (
                geometry.workspace_traffic_light_position(px(42.0)),
                geometry.sidebar_window_traffic_light_position(px(36.0)),
            ),
            (
                Some(point(px(15.5), px(14.0))),
                Some(point(px(12.0), px(11.0))),
            )
        );
    }

    #[test]
    fn density_height_should_move_each_native_row_by_half_the_height_delta() {
        let geometry = geometry();

        assert_eq!(
            (
                geometry.workspace_traffic_light_position(px(50.0)),
                geometry.sidebar_window_traffic_light_position(px(44.0)),
            ),
            (
                Some(point(px(15.5), px(18.0))),
                Some(point(px(12.0), px(15.0))),
            )
        );
    }

    #[test]
    fn unavailable_native_geometry_should_leave_traffic_lights_to_the_host_default() {
        let geometry = WindowFrameGeometry::default();

        assert_eq!(
            (
                geometry.workspace_traffic_light_position(px(42.0)),
                geometry.sidebar_window_traffic_light_position(px(36.0)),
            ),
            (None, None)
        );
    }
}
