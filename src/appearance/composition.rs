//! Requested native backdrop and effective presentation are independent from scheme classification.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowBackgroundAppearance {
    #[default]
    Opaque,
    Transparent,
    Blurred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedWindowComposition {
    pub(crate) requested: WindowBackgroundAppearance,
    pub(crate) effective: WindowBackgroundAppearance,
}

impl ResolvedWindowComposition {
    /// Foundation capability policy keeps native rendering opaque while retaining source intent.
    /// Public transparency delivery will supply accepted platform/accessibility capabilities here.
    pub(crate) const fn foundation(requested: WindowBackgroundAppearance) -> Self {
        Self {
            requested,
            effective: WindowBackgroundAppearance::Opaque,
        }
    }
}
