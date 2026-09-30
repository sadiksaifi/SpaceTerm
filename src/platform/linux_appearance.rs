//! Linux appearance facts. The desktop color-scheme preference arrives with a later desktop
//! wave; until then no system appearance is reported and SpaceTerm uses its default.
use crate::appearance::Appearance;

use super::appearance::{AppearancePlatform, SystemAppearanceObservation};

pub(super) struct LinuxAppearancePlatform;

impl AppearancePlatform for LinuxAppearancePlatform {
    fn system_appearance(&self) -> Option<Appearance> {
        None
    }

    fn observe(&self) -> Option<SystemAppearanceObservation> {
        None
    }

    fn apply_native_appearance(&self, _: Appearance) {}
}
