use super::scheme::{
    ChromeColorOverrides, ChromeScheme, CustomScheme, SchemeMetadata, TerminalColorOverrides,
    TerminalScheme,
};
use super::{Appearance, ChromeColors, Color, SchemeId, SchemeKind, TerminalColors};

pub(crate) fn dark_chrome_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.chrome.dark")
}
pub(crate) fn dark_terminal_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.terminal.dark")
}
pub(crate) fn light_chrome_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.chrome.light")
}
pub(crate) fn light_terminal_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.terminal.light")
}

pub(crate) fn fallback_id(kind: SchemeKind, appearance: Appearance) -> SchemeId {
    match (kind, appearance) {
        (SchemeKind::Chrome, Appearance::Dark) => dark_chrome_id(),
        (SchemeKind::Terminal, Appearance::Dark) => dark_terminal_id(),
        (SchemeKind::Chrome, Appearance::Light) => light_chrome_id(),
        (SchemeKind::Terminal, Appearance::Light) => light_terminal_id(),
    }
}

pub(crate) fn builtin_schemes() -> Vec<CustomScheme> {
    let chrome_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned Chrome appearance")),
    };
    let terminal_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned Terminal appearance")),
    };
    vec![
        CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: dark_chrome_id(),
            name: String::from("SpaceTerm Dark"),
            appearance: Appearance::Dark,
            metadata: chrome_metadata.clone(),
            colors: chrome_definition(Appearance::Dark),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: dark_terminal_id(),
            name: String::from("SpaceTerm Dark"),
            appearance: Appearance::Dark,
            metadata: terminal_metadata.clone(),
            colors: TerminalColorOverrides::default(),
        })),
        CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: light_chrome_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: chrome_metadata,
            colors: chrome_definition(Appearance::Light),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: light_terminal_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: terminal_metadata,
            colors: TerminalColorOverrides::default(),
        })),
    ]
}

#[cfg(test)]
pub(crate) fn chrome_base(appearance: Appearance) -> ChromeColors {
    match appearance {
        Appearance::Dark => spaceterm_dark_chrome(),
        Appearance::Light => spaceterm_light_chrome(),
    }
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
    super::compiler::compile_chrome(
        Appearance::Dark,
        &chrome_definition(Appearance::Dark),
        &ChromeColorOverrides::default(),
    )
    .colors
}
#[cfg(test)]
fn spaceterm_light_chrome() -> ChromeColors {
    super::compiler::compile_chrome(
        Appearance::Light,
        &chrome_definition(Appearance::Light),
        &ChromeColorOverrides::default(),
    )
    .colors
}

/// The authored SpaceTerm Chrome identity: one achromatic surface ladder per appearance plus a
/// small set of restrained semantic accents.
///
/// Light and dark are a paired tonal system rather than an inversion. Each appearance authors the
/// same ladder (root, chrome shell, raised surface, field, hover, pressed, persistent selection)
/// as small luminance steps over true gray, so adjacent structural surfaces separate by weight
/// alone.
///
/// Cards and floating surfaces lighten in both appearances. Interaction fills move toward text:
/// lighter in Dark and darker in Light. Tabs and navigation rows share that selection direction.
///
/// Every resting surface, separator, text gray, and shadow is authored with equal channels:
/// a cool or warm cast in a resting role reads as a tinted window over any desktop, so hue belongs
/// only to what it communicates. Persistent selection is a neutral step on that ladder: it must
/// never read as a call to action, so it stays out of the accent family entirely. Blue is reserved
/// for emphasis, links, focus, and small active indicators; red stays destructive; the remaining
/// status hues are desaturated enough to sit beside the neutrals.
///
/// Separators are authored low-contrast because a Chrome that outlines every region reads as a grid
/// of boxes. Roles that communicate state, namely focus, invalid, and the active indicator, stay
/// obvious, and raised surfaces earn their separation from `shadow` plus a slightly stronger
/// `border` rather than from a heavier hairline everywhere.
///
/// Roles absent here keep deriving from these seeds, and every derived resting role mixes only
/// these grays, so it stays achromatic too. What is authored beyond the ladder is the set the
/// compiler would otherwise hold to a readability floor against its own fill: control outlines,
/// switch indicators, and the labels on filled actions. Those floors protect a glyph, and applying
/// them to a ring or a knob collapses a quiet palette into pure black and white.
pub(super) fn chrome_definition(appearance: Appearance) -> ChromeColorOverrides {
    let palette = match appearance {
        Appearance::Dark => ChromePalette {
            root: 0x151515,
            shell: 0x171717,
            shell_inactive: 0x161616,
            raised: 0x202020,
            field: 0x1c1c1c,
            control_fill: Some(0x272727),
            control_hover: 0x2f2f2f,
            control_pressed: 0x363636,
            ghost_hover: 0x1d1d1d,
            ghost_pressed: 0x242424,
            selected: 0x2e2e2e,
            selected_inactive: 0x1c1c1c,
            row_hover: 0x282828,
            row_selected: 0x323232,
            tab_active: 0x2e2e2e,
            row_selected_text: 0xf2f2f2,
            row_selected_secondary: 0xbcbcbc,
            text: 0xe4e4e4,
            text_secondary: 0xacacac,
            text_muted: 0x999999,
            text_placeholder: 0x9a9a9a,
            text_disabled: 0x606060,
            separator: 0x2d2d2d,
            separator_quiet: 0x262626,
            separator_disabled: 0x222222,
            field_outline: 0x2f2f2f,
            control_outline: 0x313131,
            control_outline_strong: 0x3e3e3e,
            tab_separator: 0x363636,
            mark_outline: 0x767676,
            mark_outline_strong: 0x898989,
            mark_track: 0x202020,
            mark_indicator: 0xababab,
            mark_indicator_strong: 0xc5c5c5,
            scrollbar_thumb: 0x858585,
            accent: 0x5ea8ff,
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
            root: 0xebebeb,
            shell: 0xf1f1f1,
            shell_inactive: 0xefefef,
            raised: 0xf8f8f8,
            field: 0xffffff,
            control_fill: Some(0xededed),
            control_hover: 0xe3e3e3,
            control_pressed: 0xdcdcdc,
            ghost_hover: 0xe3e3e3,
            ghost_pressed: 0xdcdcdc,
            selected: 0xd6d6d6,
            selected_inactive: 0xececec,
            row_hover: 0xe9e9e9,
            row_selected: 0xdfdfdf,
            tab_active: 0xe4e4e4,
            row_selected_text: 0x161616,
            row_selected_secondary: 0x505050,
            text: 0x1e1e1e,
            text_secondary: 0x585858,
            text_muted: 0x6e6e6e,
            text_placeholder: 0x717171,
            text_disabled: 0xa5a5a5,
            separator: 0xe6e6e6,
            separator_quiet: 0xeeeeee,
            separator_disabled: 0xf2f2f2,
            field_outline: 0xdadada,
            control_outline: 0xd8d8d8,
            control_outline_strong: 0xc6c6c6,
            tab_separator: 0xcbcbcb,
            mark_outline: 0x858585,
            mark_outline_strong: 0x6d6d6d,
            mark_track: 0xe8e8e8,
            mark_indicator: 0x595959,
            mark_indicator_strong: 0x474747,
            scrollbar_thumb: 0x808080,
            accent: 0x1259b0,
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
    ///
    /// Floating preparation may strengthen the equivalent overlay without changing this authored
    /// composite when a transmitting host would otherwise make its content unreadable.
    control_fill: Option<u32>,
    /// Filled and unfilled controls have independent interaction steps.
    control_hover: u32,
    control_pressed: u32,
    /// Interaction steps for a control with no resting fill, taken from its host surface.
    ghost_hover: u32,
    ghost_pressed: u32,
    /// Persistent selection: a neutral rung, never an accent.
    selected: u32,
    /// The Active Tab's chip in an unfocused window: the same shape, a shorter step off the bar.
    ///
    /// It stays on the same side of the chrome shell as the focused chip rather than crossing to
    /// the other side of it, so an unfocused window reads as quieter rather than as inverted.
    selected_inactive: u32,
    /// Row hover steps in the same direction from both the shell and raised menu surface.
    row_hover: u32,
    /// Persistent row selection is stronger than hover on both hosts, without a decorative rim.
    row_selected: u32,
    /// The Active Tab, authored apart from the list-row fills but stepping the same way from the
    /// title bar that a selected row steps from its own surface.
    tab_active: u32,
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
    /// Structural dividers, and the hairline around raised surfaces.
    separator: u32,
    /// Separators inside an already-bounded surface.
    separator_quiet: u32,
    separator_disabled: u32,
    field_outline: u32,
    control_outline: u32,
    control_outline_strong: u32,
    /// The short hairline between two neighbouring inactive Tabs.
    ///
    /// It is authored apart from `separator` because a full-length divider disappears at this
    /// length, and apart from `control_outline` because retuning an outlined action must not move
    /// the Tab strip. It sits a visible step off both title-bar surfaces and well under the titles.
    tab_separator: u32,
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

impl ChromePalette {
    fn into_definition(self) -> ChromeColorOverrides {
        let opaque = |value: u32| Some(Color::rgb(value));
        let translucent = |value: u32| Some(Color::rgba(value));
        let on_emphasis = opaque(self.on_emphasis);
        ChromeColorOverrides {
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
            link_text_hover: opaque(self.accent_hover),
            link_text_pressed: opaque(self.accent_pressed),

            border: opaque(self.separator),
            border_variant: opaque(self.separator_quiet),
            border_disabled: opaque(self.separator_disabled),
            input_border: opaque(self.field_outline),
            outline_border: opaque(self.control_outline),
            outline_hover_border: opaque(self.control_outline_strong),
            // A press is carried by the fill. Thickening the ring as well would make an
            // outlined action jump against the quiet separators around it.
            outline_pressed_border: opaque(self.control_outline),
            outline_disabled_border: opaque(self.separator_disabled),

            element_background: self.control_fill.map(Color::rgb),
            element_hover: opaque(self.control_hover),
            element_active: opaque(self.control_pressed),
            element_selected: opaque(self.selected),
            ghost_element_hover: opaque(self.ghost_hover),
            ghost_element_active: opaque(self.ghost_pressed),
            selection_background: opaque(self.selected),
            row_background: opaque(self.shell),
            // A persistent list row is authored apart from the shared selection rung. Deriving it
            // would tie the Workspace sidebar, the Settings sections, and every menu row to the
            // fill a segmented option needs against its own track, and those surfaces differ.
            row_hover_background: opaque(self.row_hover),
            row_selected_background: opaque(self.row_selected),
            // Built-in rows use fill and text for selection. Custom appearances may author rims.
            row_selected_border: translucent(0),
            row_selected_hover_border: translucent(0),
            row_selected_foreground: opaque(self.row_selected_text),
            row_selected_hover_foreground: opaque(self.row_selected_text),
            row_selected_secondary: opaque(self.row_selected_secondary),
            row_selected_hover_secondary: opaque(self.row_selected_secondary),
            // Tabs share the row selection direction but use a smaller contrast step.
            tab_active_background: opaque(self.tab_active),
            // The label hierarchy comes along with the chip, so a Tab and a navigation row answer
            // hover identically. The chip's shape is its fill; no hairline is drawn around it.
            tab_active_foreground: opaque(self.row_selected_text),
            tab_active_border: translucent(0),
            tab_active_hover_foreground: opaque(self.row_selected_text),
            tab_inactive_selected_background: opaque(self.selected_inactive),
            tab_inactive_selected_foreground: opaque(self.text_secondary),
            tab_inactive_selected_border: translucent(0),
            tab_separator: opaque(self.tab_separator),

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
            // The compiler reads this role as a glyph and would hold it to text weight against its
            // own track. That is right for a checkmark and wrong for the dot on a switch, which
            // turns into a solid blob, so both appearances author the softer indicator directly.
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
            // A faded switch is still a switch. Deriving this mark against the faded fill passes
            // the readability floor with ordinary text, which swaps the knob for a solid dark disc
            // and makes a disabled control look like a different one.
            toggle_on_disabled_mark: on_emphasis,

            scrollbar_thumb_background: opaque(self.scrollbar_thumb),

            info: opaque(self.info),
            success: opaque(self.success),
            warning: opaque(self.warning),
            error: opaque(self.error),

            shadow: translucent(self.shadow),
            ..ChromeColorOverrides::default()
        }
    }
}

/// The authored SpaceTerm Terminal palettes, paired with the Chrome ladder of the same appearance.
///
/// The default background, foreground, and grays share Chrome's achromatic family, so a Pane reads
/// as part of the window rather than as a tinted inset. ANSI hues stay distinct and are tuned to
/// read over their own background and over the translucent Pane backdrop at every transparency;
/// dim colors keep their hue at a lower weight instead of fading toward gray. Selection answers a
/// reader's action and takes a restrained accent blue; find matches keep the familiar yellow and
/// orange so they never compete with selection.
fn spaceterm_dark_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0xd8d8d8),
        background: Color::rgb(0x141414),
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
        background: Color::rgb(0xfbfbfb),
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

    /// How far a surface departs from gray, as a share of the full channel range.
    ///
    /// Colored grays are what make a neutral Chrome read as tinted, so this is the measure the
    /// surface ladder is held to rather than a hue angle, which stays unstable near gray. The
    /// spread is absolute because a fixed channel offset is equally visible at either end of the
    /// ramp, while the same offset taken as a fraction of the brightest channel would forgive a
    /// light surface and condemn a dark one for the same departure.
    fn tint(color: Color) -> f64 {
        let channels = [color.r, color.g, color.b].map(f64::from);
        let high = channels.into_iter().fold(f64::MIN, f64::max);
        let low = channels.into_iter().fold(f64::MAX, f64::min);
        (high - low) / 255.0
    }

    /// An achromatic resting role, allowing only the rounding a mix of two grays can introduce.
    const NEUTRAL_TINT: f64 = 2.0 / 255.0;

    /// Every resting role of a built-in appearance is gray; hue is kept for what it communicates.
    ///
    /// A cast in surfaces, text grays, separators, or shadow tints the whole window, and it is
    /// most visible over a translucent backdrop where nothing else carries color. The Terminal's
    /// default backdrop and text belong to the same family, so a Pane never reads as a tinted inset.
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
                ("inactive tab rim", colors.tab_inactive_selected_border),
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
    ///
    /// The fills a reader sees while pointing at a list row, a selected element, or a scrollbar
    /// thumb are surfaces, so they come from this palette's own surface ramp. Reaching for an
    /// unrelated role, such as a terminal selection color, produces a fill from another hue family
    /// and a state that reads as a different control.
    #[test]
    fn a_selected_element_should_gain_weight_on_hover_without_leaving_its_family() {
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
    ///
    /// A selected Tab or Workspace row is a resting state that stays on screen, so tinting it with
    /// the accent gives every list a colored cast and makes selection compete with the one control
    /// the reader is meant to press. Holding the fill near gray, and far from the emphasis fill,
    /// is what keeps a quiet selection distinguishable from a call to action.
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
                (
                    "inactive window tab",
                    colors.tab_inactive_selected_background,
                ),
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

    /// The Active Tab and selected rows carry the same content hierarchy with distinct materials.
    ///
    /// Both move toward text without a decorative outline. An unfocused Tab uses a smaller step.
    #[test]
    fn the_active_tab_should_share_borderless_row_selection_direction() {
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
            assert_ne!(
                colors.tab_active_background, colors.row_selected_background,
                "{appearance:?} should tune the Active Tab for its title-bar host"
            );
            assert_ne!(
                colors.tab_active_hover_background, colors.tab_active_background,
                "{appearance:?} Active Tab should answer hover"
            );
            assert_ne!(
                colors.row_selected_hover_background, colors.row_selected_background,
                "{appearance:?} selected row should answer hover"
            );
            for (role, border) in [
                ("selected row", colors.row_selected_border),
                ("selected row hover", colors.row_selected_hover_border),
                ("Active Tab", colors.tab_active_border),
                (
                    "inactive-window Active Tab",
                    colors.tab_inactive_selected_border,
                ),
            ] {
                assert_eq!(
                    border.a, 0,
                    "{appearance:?} built-in {role} should state its shape without an outline"
                );
            }

            let bar = weight(colors.title_bar_background);
            let focused = weight(colors.tab_active_background) - bar;
            let unfocused = weight(colors.tab_inactive_selected_background) - bar;
            let row = weight(colors.row_selected_background) - weight(colors.panel_background);
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
    ///
    /// Dividers, field outlines, and resize handles appear at nearly every seam, so drawing them at
    /// the contrast a state deserves turns the window into a grid of boxes. Focus and invalid say
    /// something the reader has to act on, so they keep the contrast a signal needs.
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
                let ratio = border.contrast_ratio(surface);
                assert!(
                    ratio < 2.0,
                    "{appearance:?} {role} separator outlines its region at {ratio}"
                );
            }
            for (role, border, surface) in [
                ("focus", colors.border_focused, colors.background),
                (
                    "field focus",
                    colors.input_focused_border,
                    colors.input_background,
                ),
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

    /// Rows step consistently from every built-in host without introducing hue.
    ///
    /// Persistent rows can rest on the chrome shell or on a raised menu. Hover and selection must
    /// move in the same direction from both hosts, with selection taking the stronger step, so the
    /// same interaction never reads as a lift in one list and a recess in another.
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
