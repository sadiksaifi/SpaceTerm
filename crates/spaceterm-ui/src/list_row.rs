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
    unfocused_selected: Option<ListRowPaint>,
    unfocused_selected_hovered: Option<ListRowPaint>,
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
            unfocused_selected: None,
            unfocused_selected_hovered: None,
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

    /// Installs the prepared selected paints used while the collection lacks keyboard focus.
    pub fn unfocused_selection(mut self, prepared: Self) -> Self {
        self.unfocused_selected = Some(prepared.selected);
        self.unfocused_selected_hovered = Some(prepared.selected_hovered);
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

    /// Resolves selection with the collection's independent keyboard-focus state.
    pub fn resolve_for_collection(
        self,
        enabled: bool,
        selected: bool,
        hovered: bool,
        focused: bool,
    ) -> ListRowPaint {
        if enabled && selected && !focused {
            if hovered {
                self.unfocused_selected_hovered
                    .unwrap_or(self.selected_hovered)
            } else {
                self.unfocused_selected.unwrap_or(self.selected)
            }
        } else {
            self.resolve(enabled, selected, hovered)
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

    #[test]
    fn unfocused_selection_uses_only_the_prepared_selected_states() {
        let row = |seed| {
            let [bg, fg, secondary, icon, matched, border] =
                [seed, seed + 1, seed + 2, seed + 3, seed + 4, seed + 5].map(gpui::rgba);
            ListRowPaint::new(bg, fg, secondary, icon, matched, border)
        };
        let active = ListRowPaints::new(row(10), row(20), row(30), row(40), row(50))
            .disabled_selected(row(60));
        let prepared = ListRowPaints::new(row(110), row(120), row(130), row(140), row(150))
            .disabled_selected(row(160));
        let paints = active.unfocused_selection(prepared);

        assert_eq!(
            paints.resolve_for_collection(true, false, false, false),
            row(10)
        );
        assert_eq!(
            paints.resolve_for_collection(true, false, true, false),
            row(20)
        );
        assert_eq!(
            paints.resolve_for_collection(false, true, true, false),
            row(60)
        );
        assert_eq!(
            paints.resolve_for_collection(true, true, false, false),
            row(130)
        );
        assert_eq!(
            paints.resolve_for_collection(true, true, true, false),
            row(140)
        );
        assert_eq!(
            paints.resolve_for_collection(true, true, false, true),
            row(30)
        );
    }
}
