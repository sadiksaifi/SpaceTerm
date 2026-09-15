//! Immutable geometry supplied by the host for application-owned window content.

/// Geometry the application needs to make inset surfaces follow their hosting window.
///
/// The host supplies this fact during composition. Shared UI can fall back when a platform cannot
/// resolve it, without selecting an Adapter or querying the Operating System itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WindowFrameGeometry {
    outer_corner_radius: Option<f32>,
}

impl WindowFrameGeometry {
    pub(crate) const fn new(outer_corner_radius: Option<f32>) -> Self {
        Self {
            outer_corner_radius,
        }
    }

    pub(crate) const fn outer_corner_radius(self) -> Option<f32> {
        self.outer_corner_radius
    }
}

impl gpui::Global for WindowFrameGeometry {}
