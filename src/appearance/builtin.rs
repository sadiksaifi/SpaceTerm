use super::terminal_theme::{TerminalColorOverrides, TerminalTheme, ThemeMetadata};
use super::{Appearance, ChromeColors, Color, TerminalColors, ThemeId};

pub(crate) fn dark_terminal_id() -> ThemeId {
    ThemeId::builtin("builtin.spaceterm.dark")
}
pub(crate) fn light_terminal_id() -> ThemeId {
    ThemeId::builtin("builtin.spaceterm.light")
}
pub(crate) fn fallback_id(appearance: Appearance) -> ThemeId {
    match appearance {
        Appearance::Dark => dark_terminal_id(),
        Appearance::Light => light_terminal_id(),
    }
}
pub(crate) fn builtin_themes() -> Vec<TerminalTheme> {
    [Appearance::Dark, Appearance::Light]
        .into_iter()
        .map(|appearance| TerminalTheme {
            id: fallback_id(appearance),
            name: match appearance {
                Appearance::Dark => "SpaceTerm Dark",
                Appearance::Light => "SpaceTerm Light",
            }
            .into(),
            appearance,
            metadata: ThemeMetadata {
                author: Some("SpaceTerm contributors".into()),
                license: Some("MIT".into()),
                description: Some("SpaceTerm-owned Terminal appearance".into()),
                ..Default::default()
            },
            colors: TerminalColorOverrides::default(),
        })
        .collect()
}
#[cfg(test)]
pub(crate) fn chrome_base(appearance: Appearance) -> ChromeColors {
    super::compiler::compile_builtin_chrome(appearance)
}

pub(crate) fn terminal_base(appearance: Appearance) -> TerminalColors {
    match appearance {
        Appearance::Dark => spaceterm_dark_terminal(),
        Appearance::Light => spaceterm_light_terminal(),
    }
}

impl Default for ChromeColors {
    fn default() -> Self {
        spaceterm_dark_chrome()
    }
}

impl Default for TerminalColors {
    fn default() -> Self {
        spaceterm_dark_terminal()
    }
}

fn spaceterm_dark_chrome() -> ChromeColors {
    super::compiler::compile_builtin_chrome(Appearance::Dark)
}

/// The boundary rungs of built-in Light, authored as ink rather than as grays so one value states
/// the same step over every host, including a transmitting one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryInk {
    /// Separators inside an already-bounded surface.
    pub(crate) quiet: Color,
    /// A disabled boundary, the quietest visible rung.
    pub(crate) disabled: Color,
    /// A structural separator, and the rim of a selected segment or switch thumb.
    pub(crate) rule: Color,
    /// A field outline and the resting edge of an ordinary control.
    pub(crate) control: Color,
    /// The edge of a hovered or pressed control.
    pub(crate) strong: Color,
}

pub(crate) const LIGHT_BOUNDARY_INK: BoundaryInk = BoundaryInk {
    quiet: Color::rgba(0x0000000f),
    disabled: Color::rgba(0x0000000a),
    rule: Color::rgba(0x0000001a),
    control: Color::rgba(0x0000002b),
    strong: Color::rgba(0x00000040),
};

pub(super) fn chrome_definition(appearance: Appearance) -> ChromeDefinition {
    let palette = match appearance {
        Appearance::Dark => ChromePalette {
            root: 0x151515,
            shell: 0x171717,
            shell_inactive: 0x161616,
            raised: 0x202020,
            field: 0x262626,
            control_fill: 0x272727,
            segmented_track: None,
            control_hover: 0x2f2f2f,
            control_pressed: 0x363636,
            ghost_hover: 0x1b1b1b,
            ghost_pressed: 0x212121,
            selected: 0x2e2e2e,
            selected_inactive: 0x1c1c1c,
            row_hover: 0x282828,
            row_selected: 0x323232,
            row_selected_hover: None,
            navigation_selected: None,
            tab_active: 0x2e2e2e,
            tab_active_hover: None,
            row_selected_text: 0xf2f2f2,
            row_selected_secondary: 0xbcbcbc,
            text: 0xe4e4e4,
            text_secondary: 0xacacac,
            text_muted: 0x999999,
            text_placeholder: 0x9a9a9a,
            text_disabled: 0x606060,
            separator: Color::rgb(0x2d2d2d),
            separator_quiet: Color::rgb(0x262626),
            separator_disabled: Color::rgb(0x222222),
            field_outline: Color::rgb(0x2f2f2f),
            control_outline: Color::rgb(0x313131),
            control_outline_strong: Color::rgb(0x3e3e3e),
            tab_separator: Color::rgb(0x363636),
            // Dark states the rim faintest, where an outline reads as a frame.
            selected_rim: Color::rgba(0xffffff0a),
            selected_rim_hover: Color::rgba(0xffffff10),
            selected_rim_inactive: Color::rgba(0xffffff0a),
            mark_outline: 0x767676,
            mark_outline_strong: 0x898989,
            mark_track: 0x202020,
            mark_indicator: 0xababab,
            mark_indicator_strong: 0xc5c5c5,
            scrollbar_thumb: 0x858585,
            accent: 0x5ea8ff,
            // AppKit's keyboardFocusIndicatorColor for the default blue accent in Dark Aqua.
            focus_ring: Color::rgba(0x1aa9ff7f),
            accent_hover: 0x80baff,
            accent_pressed: 0xa6cfff,
            emphasis: 0x1f66d1,
            emphasis_hover: 0x2a72de,
            emphasis_pressed: 0x1a56b0,
            destructive: 0xbb3b38,
            destructive_hover: 0xc8443f,
            destructive_pressed: 0x9e2f2d,
            on_emphasis: 0xffffff,
            info: 0x5ea8ff,
            success: 0x66b77e,
            warning: 0xe0a75c,
            error: 0xe3707a,
            shadow: 0x00000080,
        },
        Appearance::Light => ChromePalette {
            root: 0xe5e5e5,
            shell: 0xe5e5e5,
            shell_inactive: 0xe5e5e5,
            raised: 0xfafafa,
            field: 0xfafafa,
            control_fill: 0xfafafa,
            segmented_track: Some(0xe5e5e5),
            control_hover: 0xe9e9e9,
            control_pressed: 0xdcdcdc,
            ghost_hover: 0xd2d2d2,
            ghost_pressed: 0xc4c4c4,
            // Short of white: reproducing pure white takes an ink no window can see through.
            selected: 0xfdfdfd,
            selected_inactive: 0xf4f4f4,
            row_hover: 0xf4f4f4,
            row_selected: 0xfdfdfd,
            row_selected_hover: Some(0xfdfdfd),
            navigation_selected: Some(0xfdfdfd),
            tab_active: 0xfdfdfd,
            tab_active_hover: Some(0xfdfdfd),
            row_selected_text: 0x161616,
            row_selected_secondary: 0x505050,
            text: 0x1e1e1e,
            text_secondary: 0x585858,
            text_muted: 0x666666,
            text_placeholder: 0x717171,
            text_disabled: 0x828282,
            separator: LIGHT_BOUNDARY_INK.rule,
            separator_quiet: LIGHT_BOUNDARY_INK.quiet,
            separator_disabled: LIGHT_BOUNDARY_INK.disabled,
            field_outline: LIGHT_BOUNDARY_INK.control,
            control_outline: LIGHT_BOUNDARY_INK.control,
            control_outline_strong: LIGHT_BOUNDARY_INK.strong,
            tab_separator: Color::rgba(0x00000028),
            // A bright fill has no room above it, so the rim steps inward and stays faint: an
            // edge that outruns the fill's collapsing step draws a line around the chip.
            selected_rim: Color::rgba(0x00000004),
            selected_rim_hover: Color::rgba(0x00000006),
            selected_rim_inactive: Color::rgba(0x00000004),
            mark_outline: 0x7f7f7f,
            mark_outline_strong: 0x6d6d6d,
            mark_track: 0xe5e5e5,
            mark_indicator: 0x595959,
            mark_indicator_strong: 0x474747,
            scrollbar_thumb: 0x808080,
            accent: 0x1259b0,
            // AppKit's keyboardFocusIndicatorColor for the default blue accent in Aqua.
            focus_ring: Color::rgba(0x0067f47f),
            accent_hover: 0x0e4c98,
            accent_pressed: 0x0b3f7e,
            emphasis: 0x1a66c8,
            emphasis_hover: 0x1558ad,
            emphasis_pressed: 0x114a92,
            destructive: 0xc23b34,
            destructive_hover: 0xa93028,
            destructive_pressed: 0x8d2621,
            on_emphasis: 0xffffff,
            info: 0x1a66c8,
            success: 0x22713f,
            warning: 0x8a5b12,
            error: 0xb8352f,
            shadow: 0x0000002b,
        },
    };
    palette.into_definition()
}

/// The authored seeds of one built-in Chrome appearance, named by the job each value does.
struct ChromePalette {
    /// Window root: the surface a Workspace's Pane Layout rests on.
    root: u32,
    /// Title bar, Tab strip, Workspace sidebar, and the Settings Window: the chrome shell.
    shell: u32,
    shell_inactive: u32,
    /// Menus, popovers, dialogs, tooltips, and the cards one run of Settings Rows rests on.
    raised: u32,
    field: u32,
    /// The resting fill of an ordinary bordered control.
    control_fill: u32,
    /// An explicit track gives selected chips their own host, independent of button fills.
    segmented_track: Option<u32>,
    /// Filled and unfilled controls have independent interaction steps.
    control_hover: u32,
    control_pressed: u32,
    /// Interaction steps for a control with no resting fill, taken from its host surface.
    ghost_hover: u32,
    ghost_pressed: u32,
    /// Persistent selection: a neutral rung, never an accent.
    selected: u32,
    /// The Active Tab's chip in an unfocused window: a shorter step off the bar, on the same side
    /// of the shell as the focused chip.
    selected_inactive: u32,
    /// Row hover steps in the same direction from both the shell and raised menu surface.
    row_hover: u32,
    /// Persistent row selection is stronger than hover on both hosts, without a decorative rim.
    row_selected: u32,
    row_selected_hover: Option<u32>,
    navigation_selected: Option<u32>,
    /// The Active Tab, authored apart from the list-row fills but stepping the same way from the
    /// title bar that a selected row steps from its own surface.
    tab_active: u32,
    tab_active_hover: Option<u32>,
    /// The label on a selected chip, which is the strongest text in a navigation list.
    row_selected_text: u32,
    /// Path, machine, and count text on a selected chip, lifted so the chip never reads dimmer
    /// than the rows around it.
    row_selected_secondary: u32,
    text: u32,
    text_secondary: u32,
    text_muted: u32,
    text_placeholder: u32,
    text_disabled: u32,
    /// The hairline around a raised surface, and the outer edge of a structural region.
    separator: Color,
    /// Separators inside an already-bounded surface.
    separator_quiet: Color,
    separator_disabled: Color,
    field_outline: Color,
    control_outline: Color,
    control_outline_strong: Color,
    /// The short hairline between two neighbouring inactive Tabs, authored apart from
    /// `separator` and `control_outline` so retuning either does not move the Tab strip.
    tab_separator: Color,
    /// The rim a persistent selection carries where its fill alone cannot state its shape.
    selected_rim: Color,
    selected_rim_hover: Color,
    selected_rim_inactive: Color,
    /// Checkbox and switch outlines, which must survive a quiet separator.
    mark_outline: u32,
    mark_outline_strong: u32,
    /// The unswitched track of a checkbox or switch.
    mark_track: u32,
    /// The indicator riding that track, which the compiler holds to a glyph's readability floor.
    mark_indicator: u32,
    mark_indicator_strong: u32,
    scrollbar_thumb: u32,
    /// Links, focus, and small active indicators.
    accent: u32,
    /// The translucent band a focused control draws around itself.
    focus_ring: Color,
    accent_hover: u32,
    accent_pressed: u32,
    /// Filled emphasis: Primary actions and switched-on controls.
    emphasis: u32,
    emphasis_hover: u32,
    emphasis_pressed: u32,
    destructive: u32,
    destructive_hover: u32,
    destructive_pressed: u32,
    /// The label carried by every filled emphasis and destructive state.
    on_emphasis: u32,
    info: u32,
    success: u32,
    warning: u32,
    error: u32,
    shadow: u32,
}

pub(super) struct ChromeDefinition {
    pub(super) background: Color,
    pub(super) panel_background: Color,
    pub(super) title_bar_background: Color,
    pub(super) title_bar_inactive_background: Color,
    pub(super) elevated_surface_background: Color,
    pub(super) input_background: Color,
    pub(super) text: Color,
    pub(super) text_secondary: Color,
    pub(super) text_muted: Color,
    pub(super) text_placeholder: Color,
    pub(super) text_disabled: Color,
    pub(super) text_accent: Color,
    pub(super) focus_ring: Color,
    pub(super) link_text_hover: Color,
    pub(super) link_text_pressed: Color,
    pub(super) border: Color,
    pub(super) border_variant: Color,
    pub(super) border_disabled: Color,
    pub(super) input_border: Color,
    pub(super) outline_border: Color,
    pub(super) outline_hover_border: Color,
    pub(super) outline_pressed_border: Color,
    pub(super) outline_disabled_border: Color,
    pub(super) element_background: Color,
    pub(super) segmented_track_background: Option<Color>,
    pub(super) element_hover: Color,
    pub(super) element_active: Color,
    pub(super) element_selected: Color,
    pub(super) ghost_element_hover: Color,
    pub(super) ghost_element_active: Color,
    pub(super) selection_background: Color,
    pub(super) row_background: Color,
    pub(super) row_hover_background: Color,
    pub(super) row_selected_background: Color,
    pub(super) row_selected_hover_background: Option<Color>,
    pub(super) navigation_selected_background: Option<Color>,
    pub(super) row_selected_border: Color,
    pub(super) row_selected_hover_border: Color,
    pub(super) row_selected_foreground: Color,
    pub(super) row_selected_hover_foreground: Color,
    pub(super) row_selected_secondary: Color,
    pub(super) row_selected_hover_secondary: Color,
    pub(super) tab_active_background: Color,
    pub(super) tab_active_hover_background: Option<Color>,
    pub(super) tab_active_foreground: Color,
    pub(super) tab_active_border: Color,
    pub(super) tab_active_hover_foreground: Color,
    pub(super) inactive_selection_background: Color,
    pub(super) inactive_selection_border: Color,
    pub(super) tab_separator: Color,
    pub(super) primary_background: Color,
    pub(super) primary_hover_background: Color,
    pub(super) primary_pressed_background: Color,
    pub(super) primary_foreground: Color,
    pub(super) primary_hover_foreground: Color,
    pub(super) primary_pressed_foreground: Color,
    pub(super) destructive_background: Color,
    pub(super) destructive_hover_background: Color,
    pub(super) destructive_pressed_background: Color,
    pub(super) destructive_foreground: Color,
    pub(super) destructive_hover_foreground: Color,
    pub(super) destructive_pressed_foreground: Color,
    pub(super) toggle_off_background: Color,
    pub(super) toggle_off_mark: Color,
    pub(super) toggle_off_hover_mark: Color,
    pub(super) toggle_off_pressed_mark: Color,
    pub(super) toggle_off_border: Color,
    pub(super) toggle_off_hover_border: Color,
    pub(super) toggle_off_pressed_border: Color,
    pub(super) toggle_on_background: Color,
    pub(super) toggle_on_hover_background: Color,
    pub(super) toggle_on_pressed_background: Color,
    pub(super) toggle_on_border: Color,
    pub(super) toggle_on_hover_border: Color,
    pub(super) toggle_on_pressed_border: Color,
    pub(super) toggle_on_mark: Color,
    pub(super) toggle_on_hover_mark: Color,
    pub(super) toggle_on_pressed_mark: Color,
    pub(super) toggle_on_disabled_mark: Color,
    pub(super) scrollbar_thumb_background: Color,
    pub(super) info: Color,
    pub(super) success: Color,
    pub(super) warning: Color,
    pub(super) error: Color,
    pub(super) shadow: Color,
}

impl ChromePalette {
    fn into_definition(self) -> ChromeDefinition {
        let opaque = |value: u32| Color::rgb(value);
        let translucent = |value: u32| Color::rgba(value);
        let on_emphasis = opaque(self.on_emphasis);
        ChromeDefinition {
            background: opaque(self.root),
            panel_background: opaque(self.shell),
            title_bar_background: opaque(self.shell),
            title_bar_inactive_background: opaque(self.shell_inactive),
            elevated_surface_background: opaque(self.raised),
            input_background: opaque(self.field),

            text: opaque(self.text),
            text_secondary: opaque(self.text_secondary),
            text_muted: opaque(self.text_muted),
            text_placeholder: opaque(self.text_placeholder),
            text_disabled: opaque(self.text_disabled),
            text_accent: opaque(self.accent),
            focus_ring: self.focus_ring,
            link_text_hover: opaque(self.accent_hover),
            link_text_pressed: opaque(self.accent_pressed),

            border: self.separator,
            border_variant: self.separator_quiet,
            border_disabled: self.separator_disabled,
            input_border: self.field_outline,
            outline_border: self.control_outline,
            outline_hover_border: self.control_outline_strong,
            // A press is carried by the fill. Thickening the ring as well would make an
            // outlined action jump against the quiet separators around it.
            outline_pressed_border: self.control_outline,
            outline_disabled_border: self.separator_disabled,

            element_background: Color::rgb(self.control_fill),
            segmented_track_background: self.segmented_track.map(Color::rgb),
            element_hover: opaque(self.control_hover),
            element_active: opaque(self.control_pressed),
            element_selected: opaque(self.selected),
            ghost_element_hover: opaque(self.ghost_hover),
            ghost_element_active: opaque(self.ghost_pressed),
            selection_background: opaque(self.selected),
            row_background: opaque(self.shell),
            // Authored apart from the selection rung, which segmented options need on their track.
            row_hover_background: opaque(self.row_hover),
            row_selected_background: opaque(self.row_selected),
            row_selected_hover_background: self.row_selected_hover.map(Color::rgb),
            navigation_selected_background: self.navigation_selected.map(Color::rgb),
            // The rim steps up on hover because a selected row's fill has nowhere left to move.
            row_selected_border: self.selected_rim,
            row_selected_hover_border: self.selected_rim_hover,
            row_selected_foreground: opaque(self.row_selected_text),
            row_selected_hover_foreground: opaque(self.row_selected_text),
            row_selected_secondary: opaque(self.row_selected_secondary),
            row_selected_hover_secondary: opaque(self.row_selected_secondary),
            // Tabs and navigation rows share the appearance's selection direction.
            tab_active_background: opaque(self.tab_active),
            tab_active_hover_background: self.tab_active_hover.map(Color::rgb),
            tab_active_foreground: opaque(self.row_selected_text),
            tab_active_border: self.selected_rim,
            tab_active_hover_foreground: opaque(self.row_selected_text),
            inactive_selection_background: opaque(self.selected_inactive),
            inactive_selection_border: self.selected_rim_inactive,
            tab_separator: self.tab_separator,

            primary_background: opaque(self.emphasis),
            primary_hover_background: opaque(self.emphasis_hover),
            primary_pressed_background: opaque(self.emphasis_pressed),
            primary_foreground: on_emphasis,
            primary_hover_foreground: on_emphasis,
            primary_pressed_foreground: on_emphasis,

            destructive_background: opaque(self.destructive),
            destructive_hover_background: opaque(self.destructive_hover),
            destructive_pressed_background: opaque(self.destructive_pressed),
            destructive_foreground: on_emphasis,
            destructive_hover_foreground: on_emphasis,
            destructive_pressed_foreground: on_emphasis,

            toggle_off_background: opaque(self.mark_track),
            // Authored directly: the compiler's glyph readability floor turns a switch dot into a blob.
            toggle_off_mark: opaque(self.mark_indicator),
            toggle_off_hover_mark: opaque(self.mark_indicator_strong),
            toggle_off_pressed_mark: opaque(self.mark_indicator_strong),
            toggle_off_border: opaque(self.mark_outline),
            toggle_off_hover_border: opaque(self.mark_outline_strong),
            toggle_off_pressed_border: opaque(self.mark_outline_strong),
            toggle_on_background: opaque(self.emphasis),
            toggle_on_hover_background: opaque(self.emphasis_hover),
            toggle_on_pressed_background: opaque(self.emphasis_pressed),
            toggle_on_border: opaque(self.emphasis),
            toggle_on_hover_border: opaque(self.emphasis_hover),
            toggle_on_pressed_border: opaque(self.emphasis_pressed),
            toggle_on_mark: on_emphasis,
            toggle_on_hover_mark: on_emphasis,
            toggle_on_pressed_mark: on_emphasis,
            // Deriving this against the faded fill turns the knob into a solid dark disc.
            toggle_on_disabled_mark: on_emphasis,

            scrollbar_thumb_background: opaque(self.scrollbar_thumb),

            info: opaque(self.info),
            success: opaque(self.success),
            warning: opaque(self.warning),
            error: opaque(self.error),

            shadow: translucent(self.shadow),
        }
    }
}

/// The authored SpaceTerm Terminal palettes, paired with the Chrome ladder of the same appearance.
fn spaceterm_dark_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0xd8d8d8),
        background: Color::rgb(0x191919),
        normal: [
            0x2e2e2e, 0xe5696b, 0x83c07e, 0xe6b85c, 0x5fa3f0, 0xc387d9, 0x5fc0c8, 0xc8c8c8,
        ]
        .map(Color::rgb),
        bright: [
            0x6e6e6e, 0xf28a8b, 0x9fd39a, 0xf0cb80, 0x84b9f6, 0xd5a4e6, 0x82d3d9, 0xf0f0f0,
        ]
        .map(Color::rgb),
        dim: [
            0x1f1f1f, 0xa24c4e, 0x5e895a, 0xa38443, 0x4a78ae, 0x8c64a0, 0x468a90, 0x8a8a8a,
        ]
        .map(Color::rgb),
        bright_foreground: Color::rgb(0xf0f0f0),
        dim_foreground: Color::rgb(0x8a8a8a),
        cursor: Color::rgb(0xd8d8d8),
        cursor_text: None,
        selection_background: Color::rgba(0x34485eaa),
        selection_foreground: None,
        find_match_background: Color::rgba(0xe6b85c4d),
        find_match_foreground: None,
        find_active_match_background: Color::rgba(0xf0913a80),
        find_active_match_foreground: None,
        hyperlink: Color::rgb(0x5ea8ff),
        visual_bell: Color::rgba(0xe6b85c80),
    }
}

fn spaceterm_light_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0x242424),
        // Pure white costs an ink of alpha 251 over the root, which ignores the Transparency Setting.
        background: Color::rgb(0xfdfdfd),
        normal: [
            0x2e2e2e, 0xb3313c, 0x2a7a3b, 0x8c5a00, 0x2d62a8, 0x8a4ba0, 0x16767e, 0x6e6e6e,
        ]
        .map(Color::rgb),
        bright: [
            0x767676, 0xcc4450, 0x3a9450, 0xa86c0a, 0x3f7cc8, 0xa566bb, 0x258f98, 0x242424,
        ]
        .map(Color::rgb),
        dim: [
            0x9c9c9c, 0x7a3038, 0x33603c, 0x664a22, 0x34547f, 0x5f4468, 0x2f5d62, 0x606060,
        ]
        .map(Color::rgb),
        bright_foreground: Color::rgb(0x111111),
        dim_foreground: Color::rgb(0x686868),
        cursor: Color::rgb(0x3a3a3a),
        cursor_text: None,
        selection_background: Color::rgba(0xa9cdf588),
        selection_foreground: None,
        find_match_background: Color::rgba(0xf0c66c88),
        find_match_foreground: None,
        find_active_match_background: Color::rgba(0xe3964388),
        find_active_match_foreground: None,
        hyperlink: Color::rgb(0x1259b0),
        visual_bell: Color::rgba(0xd28a2380),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The luminance a reader compares when two Chrome surfaces meet.
    fn weight(color: Color) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    /// How far a surface departs from gray, as an absolute share of the full channel range, since
    /// hue angle is unstable near gray.
    fn tint(color: Color) -> f64 {
        let channels = [color.r, color.g, color.b].map(f64::from);
        let high = channels.into_iter().fold(f64::MIN, f64::max);
        let low = channels.into_iter().fold(f64::MAX, f64::min);
        (high - low) / 255.0
    }

    /// An achromatic resting role, allowing only the rounding a mix of two grays can introduce.
    const NEUTRAL_TINT: f64 = 2.0 / 255.0;

    /// Every resting role of a built-in appearance is gray; hue is kept for what it communicates.
    #[test]
    fn resting_roles_should_stay_achromatic_in_both_appearances() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();
            let terminal = terminal_base(appearance);

            for (role, color) in [
                ("root", colors.background),
                ("shell", colors.panel_background),
                ("title bar", colors.title_bar_background),
                ("inactive title bar", colors.title_bar_inactive_background),
                ("raised", colors.elevated_surface_background),
                ("field", colors.input_background),
                ("hover", colors.element_hover),
                ("pressed", colors.element_active),
                ("ghost hover", colors.ghost_element_hover),
                ("row hover", colors.row_hover_background),
                ("tab hover", colors.tab_hover_background),
                ("selected chip", colors.row_selected_background),
                ("selected chip hover", colors.row_selected_hover_background),
                ("tab", colors.tab_active_background),
                ("active tab hover", colors.tab_active_hover_background),
                ("toggle track", colors.toggle_off_background),
                ("text", colors.text),
                ("secondary text", colors.text_secondary),
                ("muted text", colors.text_muted),
                ("placeholder", colors.text_placeholder),
                ("disabled text", colors.text_disabled),
                ("icon", colors.icon),
                ("muted icon", colors.icon_muted),
                ("row text", colors.row_foreground),
                ("row secondary", colors.row_secondary),
                ("chip secondary", colors.row_selected_secondary),
                ("border", colors.border),
                ("quiet border", colors.border_variant),
                ("field outline", colors.input_border),
                ("control outline", colors.outline_border),
                ("chip rim", colors.row_selected_border),
                ("inactive tab rim", colors.inactive_selection_border),
                ("tab separator", colors.tab_separator),
                ("resize handle", colors.resize_idle),
                ("scrollbar", colors.scrollbar_thumb_background),
                ("shadow", colors.shadow),
                ("modal scrim", colors.modal_scrim),
                ("terminal background", terminal.background),
                ("terminal foreground", terminal.foreground),
                ("terminal cursor", terminal.cursor),
            ] {
                assert!(
                    tint(color) <= NEUTRAL_TINT,
                    "{appearance:?} {role} carries a hue cast: {color:?}"
                );
            }
            assert!(
                tint(colors.text_accent) > 0.3,
                "{appearance:?} keeps its accent recognisably colored"
            );
        }
    }

    /// Every built-in chrome interaction fill stays in the neutral surface family.
    #[test]
    fn selected_and_ghost_fills_remain_opaque_with_distinct_selection_hover() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance);

            assert_ne!(
                colors.selection_background, colors.selection_hover_background,
                "{appearance:?} should keep the hovered state of a selected element visible"
            );
            for role in [
                colors.selection_background,
                colors.selection_hover_background,
                colors.ghost_element_hover,
                colors.ghost_element_selected,
            ] {
                assert_eq!(role.a, 0xff, "{appearance:?} paints chrome surfaces opaque");
            }
        }
    }

    /// Persistent selection reads as a neutral rung of the surface ladder, not as an action.
    #[test]
    fn persistent_selection_should_stay_a_neutral_surface_rather_than_an_emphasized_action() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance);

            for (role, fill) in [
                ("selection", colors.selection_background),
                ("row", colors.row_selected_background),
                ("row hover", colors.row_selected_hover_background),
                ("element", colors.element_selected),
                ("tab", colors.tab_active_background),
                ("inactive window tab", colors.inactive_selection_background),
            ] {
                assert!(
                    tint(fill) <= NEUTRAL_TINT,
                    "{appearance:?} {role} selection carries a hue cast: {fill:?}"
                );
            }
            assert!(
                tint(colors.primary_background) > 0.3,
                "{appearance:?} keeps an emphasized action recognisably colored"
            );
            assert!(
                (weight(colors.selection_background) - weight(colors.primary_background)).abs()
                    > 0.02,
                "{appearance:?} should not let selection approach the emphasis fill"
            );
        }
    }

    /// Light adds quiet rims; Dark keeps fill-only chips. An unfocused Tab uses a smaller step.
    #[test]
    fn the_active_tab_should_share_row_selection_direction_and_theme_edge_policy() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();

            for (role, tab, row) in [
                (
                    "text",
                    colors.tab_active_foreground,
                    colors.row_selected_foreground,
                ),
                (
                    "hovered text",
                    colors.tab_active_hover_foreground,
                    colors.row_selected_hover_foreground,
                ),
                (
                    "hovered icon",
                    colors.tab_active_hover_icon,
                    colors.row_selected_hover_icon,
                ),
            ] {
                assert_eq!(
                    tab, row,
                    "{appearance:?} should give the Active Tab and a selected row one {role}"
                );
            }
            assert_eq!(
                colors.tab_active_background == colors.row_selected_background,
                appearance == Appearance::Light,
                "Light should share its selected fill across Tab and row hosts"
            );
            if appearance == Appearance::Light {
                assert_eq!(
                    colors.tab_active_hover_background,
                    colors.tab_active_background
                );
                assert_eq!(
                    colors.row_selected_hover_background,
                    colors.row_selected_background
                );
                assert_ne!(colors.row_selected_hover_border, colors.row_selected_border);
            } else {
                assert_ne!(
                    colors.tab_active_hover_background,
                    colors.tab_active_background
                );
                assert_ne!(
                    colors.row_selected_hover_background,
                    colors.row_selected_background
                );
            }
            for (role, border) in [
                ("selected row", colors.row_selected_border),
                ("selected row hover", colors.row_selected_hover_border),
                ("Active Tab", colors.tab_active_border),
                (
                    "inactive-window Active Tab",
                    colors.inactive_selection_border,
                ),
            ] {
                assert!(
                    border.a > 0,
                    "{appearance:?} built-in {role} should carry the chip hairline"
                );
                assert_eq!(
                    weight(border) > weight(colors.row_selected_background),
                    appearance == Appearance::Dark,
                    "{appearance:?} built-in {role} should draw its hairline in the ink its \
                     appearance bounds surfaces with"
                );
            }

            let bar = weight(colors.title_bar_background);
            let focused = weight(colors.tab_active_background) - bar;
            let unfocused = weight(colors.inactive_selection_background) - bar;
            let row =
                weight(colors.navigation_selected_background) - weight(colors.panel_background);
            assert!(
                focused * row > 0.0 && unfocused * row > 0.0,
                "{appearance:?} should keep Tab states in the row selection direction, got \
                 {unfocused} against {focused}"
            );
            assert!(
                unfocused.abs() < focused.abs(),
                "{appearance:?} should quieten the unfocused Tab, got {unfocused} against \
                 {focused}"
            );
            assert_eq!(
                colors.tab_inactive_background, colors.title_bar_background,
                "{appearance:?} should leave an inactive Tab as text on the bar"
            );
        }
    }

    /// Structural separators organize without outlining, while stateful borders stay obvious.
    #[test]
    fn structural_separators_should_stay_quieter_than_the_borders_that_report_state() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();

            for (role, border, surface) in [
                ("divider", colors.border, colors.background),
                (
                    "panel divider",
                    colors.border_variant,
                    colors.panel_background,
                ),
                ("field", colors.input_border, colors.input_background),
                ("resize handle", colors.resize_idle, colors.background),
            ] {
                let ratio = border.source_over(surface).contrast_ratio(surface);
                assert!(
                    ratio < 2.0,
                    "{appearance:?} {role} separator outlines its region at {ratio}"
                );
            }
            for (role, border, surface) in [
                ("focus", colors.border_focused, colors.background),
                (
                    "invalid field",
                    colors.input_invalid_border,
                    colors.input_background,
                ),
            ] {
                assert!(
                    border.contrast_ratio(surface) >= 3.0,
                    "{appearance:?} {role} border should stay visible"
                );
            }
        }
    }

    /// Authored navigation lifts off the shell without hue. Light popup hosts are prepared
    /// separately from the raised card role, which matches the authored navigation selection.
    #[test]
    fn the_row_ladder_should_step_consistently_from_shell_and_raised_surfaces() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();

            for (role, surface) in [
                ("shell", colors.panel_background),
                ("raised", colors.elevated_surface_background),
                ("row hover", colors.row_hover_background),
                ("selected row", colors.row_selected_background),
            ] {
                assert!(
                    tint(surface) <= NEUTRAL_TINT,
                    "{appearance:?} {role} surface carries a hue cast: {surface:?}"
                );
            }
            for (host_name, host) in [
                ("shell", colors.panel_background),
                ("raised surface", colors.elevated_surface_background),
            ] {
                if appearance == Appearance::Light && host_name == "raised surface" {
                    // A bright selection takes the rung above this surface, and hover stays
                    // below it, so the raised host sits between the two rather than under both.
                    assert!(weight(colors.row_selected_background) > weight(host));
                    assert!(weight(colors.row_hover_background) < weight(host));
                    continue;
                }
                let hover_step = weight(colors.row_hover_background) - weight(host);
                let selection_step = weight(colors.row_selected_background) - weight(host);
                assert!(
                    hover_step * selection_step > 0.0,
                    "{appearance:?} row hover and selection should step the same way from the \
                     {host_name}, got {hover_step} and {selection_step}"
                );
                assert!(
                    selection_step.abs() > hover_step.abs(),
                    "{appearance:?} selection should step farther than hover from the \
                     {host_name}, got {selection_step} against {hover_step}"
                );
                for (role, fill) in [
                    ("hover", colors.row_hover_background),
                    ("selection", colors.row_selected_background),
                ] {
                    let contrast = fill.contrast_ratio(host);
                    assert!(
                        contrast > 1.0 && contrast < 1.6,
                        "{appearance:?} {role} steps from the {host_name} at {contrast}, which \
                         should stay visible without reading as an edge"
                    );
                }
            }
            assert!(
                colors
                    .elevated_surface_background
                    .contrast_ratio(colors.panel_background)
                    > 1.0
                    && weight(colors.elevated_surface_background) > weight(colors.panel_background),
                "{appearance:?} should lift a floating surface above the chrome shell"
            );
        }
    }
}
