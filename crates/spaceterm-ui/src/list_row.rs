use gpui::Rgba;

/// Chooses the label font for a row's persistent selection state.
///
/// Disabled and inactive presentation may change paints, but neither clears selection, so every
/// selected row keeps the emphasis font supplied by the application catalog.
pub(crate) fn label_font(typography: &crate::ControlTypography, selected: bool) -> gpui::Font {
    if selected {
        typography.emphasis()
    } else {
        typography.regular()
    }
    .clone()
}

/// Complete result-row paints, including content that is secondary to the label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListRowPaint {
    pub(crate) background: Rgba,
    pub(crate) foreground: Rgba,
    pub(crate) secondary: Rgba,
    pub(crate) icon: Rgba,
    pub(crate) matched: Rgba,
    pub(crate) border: Rgba,
}

impl ListRowPaint {
    /// Creates an independently authored row state.
    pub fn new(
        background: Rgba,
        foreground: Rgba,
        secondary: Rgba,
        icon: Rgba,
        matched: Rgba,
        border: Rgba,
    ) -> Self {
        Self {
            background,
            foreground,
            secondary,
            icon,
            matched,
            border,
        }
    }
}

/// Bounded persistent-selection and pointer states shared by list controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListRowPaints {
    normal: ListRowPaint,
    hovered: ListRowPaint,
    selected: ListRowPaint,
    selected_hovered: ListRowPaint,
    disabled: ListRowPaint,
    disabled_selected: ListRowPaint,
}
impl ListRowPaints {
    /// Creates all independently authored list row states.
    pub fn new(
        normal: ListRowPaint,
        hovered: ListRowPaint,
        selected: ListRowPaint,
        selected_hovered: ListRowPaint,
        disabled: ListRowPaint,
    ) -> Self {
        Self {
            normal,
            hovered,
            selected,
            selected_hovered,
            disabled,
            disabled_selected: disabled,
        }
    }

    /// Sets the paint that preserves persistent selection on a disabled row.
    ///
    /// Without an explicit value, disabled selected rows retain the ordinary disabled paint for
    /// compatibility with catalogs that predate this state.
    pub fn disabled_selected(mut self, paint: ListRowPaint) -> Self {
        self.disabled_selected = paint;
        self
    }

    /// Resolves disabled first, then combines persistent selection and hover.
    pub fn resolve(self, enabled: bool, selected: bool, hovered: bool) -> ListRowPaint {
        if !enabled {
            if selected {
                self.disabled_selected
            } else {
                self.disabled
            }
        } else {
            match (selected, hovered) {
                (true, true) => self.selected_hovered,
                (true, false) => self.selected,
                (false, true) => self.hovered,
                (false, false) => self.normal,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_rows_keep_the_catalog_emphasis_font() {
        let typography = crate::ControlTypography::default();

        assert_eq!(label_font(&typography, false), typography.regular().clone());
        assert_eq!(label_font(&typography, true), typography.emphasis().clone());
    }

    #[test]
    fn complete_row_state_follows_selection_hover_and_disabled_precedence() {
        let row = |seed| {
            let [bg, fg, secondary, icon, matched, border] =
                [seed, seed + 1, seed + 2, seed + 3, seed + 4, seed + 5].map(gpui::rgba);
            ListRowPaint::new(bg, fg, secondary, icon, matched, border)
        };
        let states = [row(10), row(20), row(30), row(40), row(50), row(60)];
        let paints = ListRowPaints::new(states[0], states[1], states[2], states[3], states[4])
            .disabled_selected(states[5]);
        for (selected, hovered, index) in [
            (false, false, 0),
            (false, true, 1),
            (true, false, 2),
            (true, true, 3),
        ] {
            assert_eq!(paints.resolve(true, selected, hovered), states[index]);
            assert_eq!(
                paints.resolve(false, selected, hovered),
                states[if selected { 5 } else { 4 }]
            );
        }
    }

    #[test]
    fn disabled_selected_defaults_to_the_disabled_paint() {
        let row = |seed| {
            let [bg, fg, secondary, icon, matched, border] =
                [seed, seed + 1, seed + 2, seed + 3, seed + 4, seed + 5].map(gpui::rgba);
            ListRowPaint::new(bg, fg, secondary, icon, matched, border)
        };
        let disabled = row(50);
        let paints = ListRowPaints::new(row(10), row(20), row(30), row(40), disabled);

        assert_eq!(paints.resolve(false, true, true), disabled);
    }
}
