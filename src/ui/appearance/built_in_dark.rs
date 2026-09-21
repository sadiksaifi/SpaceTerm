//! Boundary policy for the built-in Dark definition. Custom definitions retain their own policy.

use super::separator::SeparatorBand;

/// Internal rules separate rows without outlining their containing group.
pub(super) const RULE_BAND: SeparatorBand = SeparatorBand {
    floor: 1.15,
    ceiling: 1.35,
};

/// Pane and floating-shell edges distinguish independent surfaces from their surroundings.
pub(super) const SURFACE_BAND: SeparatorBand = SeparatorBand {
    floor: 1.25,
    ceiling: 1.50,
};
