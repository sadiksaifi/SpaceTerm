use super::scheme::{
    ChromeColorOverrides, ChromeScheme, CustomScheme, SchemeMetadata, TerminalColorOverrides,
    TerminalScheme,
};
use super::{Appearance, ChromeColors, Color, SchemeId, SchemeKind, TerminalColors};

pub(crate) fn vague_chrome_id() -> SchemeId {
    SchemeId::builtin("builtin.vague-pro.chrome.dark")
}
pub(crate) fn vague_terminal_id() -> SchemeId {
    SchemeId::builtin("builtin.vague-pro.terminal.dark")
}
pub(crate) fn light_chrome_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.chrome.light")
}
pub(crate) fn light_terminal_id() -> SchemeId {
    SchemeId::builtin("builtin.spaceterm.terminal.light")
}

pub(crate) fn fallback_id(kind: SchemeKind, appearance: Appearance) -> SchemeId {
    match (kind, appearance) {
        (SchemeKind::Chrome, Appearance::Dark) => vague_chrome_id(),
        (SchemeKind::Terminal, Appearance::Dark) => vague_terminal_id(),
        (SchemeKind::Chrome, Appearance::Light) => light_chrome_id(),
        (SchemeKind::Terminal, Appearance::Light) => light_terminal_id(),
    }
}

pub(crate) fn builtin_schemes() -> Vec<CustomScheme> {
    let vague_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("Vague Theme contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from(
            "Terminal colors extracted from the pinned Vague Pro source",
        )),
    };
    let spaceterm_chrome_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned Chrome appearance")),
    };
    let spaceterm_terminal_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned Terminal appearance")),
    };
    vec![
        // The Chrome palette is SpaceTerm's own identity, so it carries SpaceTerm attribution even
        // though the retained scheme id keeps existing settings resolving. Only the paired Terminal
        // scheme still consumes the pinned Vague Pro source.
        CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: vague_chrome_id(),
            name: String::from("SpaceTerm Dark"),
            appearance: Appearance::Dark,
            metadata: spaceterm_chrome_metadata.clone(),
            colors: chrome_definition(Appearance::Dark),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: vague_terminal_id(),
            name: String::from("Vague Pro Dark"),
            appearance: Appearance::Dark,
            metadata: vague_metadata,
            colors: TerminalColorOverrides::default(),
        })),
        CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: light_chrome_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: spaceterm_chrome_metadata,
            colors: chrome_definition(Appearance::Light),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: light_terminal_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: spaceterm_terminal_metadata,
            colors: TerminalColorOverrides::default(),
        })),
    ]
}

#[cfg(test)]
pub(crate) fn chrome_base(appearance: Appearance) -> ChromeColors {
    match appearance {
        Appearance::Dark => vague_dark_chrome(),
        Appearance::Light => spaceterm_light_chrome(),
    }
}

pub(crate) fn terminal_base(appearance: Appearance) -> TerminalColors {
    match appearance {
        Appearance::Dark => vague_dark_terminal(),
        Appearance::Light => spaceterm_light_terminal(),
    }
}

impl Default for ChromeColors {
    fn default() -> Self {
        vague_dark_chrome()
    }
}

impl Default for TerminalColors {
    fn default() -> Self {
        vague_dark_terminal()
    }
}

fn vague_dark_chrome() -> ChromeColors {
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

/// The authored SpaceTerm Chrome identity: one cool-neutral surface ladder per appearance plus a
/// small set of restrained semantic accents.
///
/// Light and dark are a paired tonal system rather than an inversion. Each appearance authors the
/// same ladder (root, chrome shell, raised surface, field, hover, pressed, persistent selection)
/// as small luminance steps over a near-achromatic cool gray, so adjacent structural surfaces
/// separate by weight instead of by hue. Persistent selection is a neutral step on that ladder: it
/// must never read as a call to action, so it stays out of the accent family entirely. Blue is
/// reserved for emphasis, links, focus, and small active indicators; red stays destructive; the
/// remaining status hues are desaturated enough to sit beside the neutrals.
///
/// Separators are authored low-contrast because a Chrome that outlines every region reads as a grid
/// of boxes. Roles that communicate state, namely focus, invalid, and the active indicator, stay
/// obvious, and raised surfaces earn their separation from `shadow` plus a slightly stronger
/// `border` rather than from a heavier hairline everywhere.
///
/// Roles absent here keep deriving from these seeds. What is authored beyond the ladder is the set
/// the compiler would otherwise hold to a readability floor against its own fill: control
/// outlines, switch indicators, and the labels on filled actions. Those floors protect a glyph, and
/// applying them to a ring or a knob collapses a quiet palette into pure black and white.
pub(super) fn chrome_definition(appearance: Appearance) -> ChromeColorOverrides {
    let palette = match appearance {
        Appearance::Dark => ChromePalette {
            root: 0x141517,
            shell: 0x161719,
            shell_inactive: 0x151618,
            raised: 0x1e2024,
            field: 0x1a1c1f,
            hover: 0x1c1d21,
            pressed: 0x222428,
            selected: 0x2c2e33,
            selected_inactive: 0x1b1c20,
            row_selected: 0x25272b,
            row_selected_rim: 0x303338,
            row_selected_text: 0xf1f2f5,
            row_selected_secondary: 0xb9bdc5,
            text: 0xe3e4e8,
            text_secondary: 0xa9adb5,
            text_muted: 0x969aa2,
            text_placeholder: 0x979ba3,
            text_disabled: 0x5e6167,
            separator: 0x25272b,
            separator_quiet: 0x1c1e22,
            separator_disabled: 0x202226,
            field_outline: 0x2c2f34,
            control_outline: 0x2e3136,
            control_outline_strong: 0x3b3e45,
            tab_separator: 0x33363c,
            mark_outline: 0x727781,
            mark_outline_strong: 0x848a95,
            mark_track: 0x1e2024,
            mark_indicator: 0xa8acb4,
            mark_indicator_strong: 0xc2c6cd,
            scrollbar_thumb: 0x82868e,
            accent: 0x78ade9,
            accent_hover: 0x98c1f0,
            accent_pressed: 0xb8d6f5,
            emphasis: 0x2c65c0,
            emphasis_hover: 0x3572d0,
            emphasis_pressed: 0x24549f,
            destructive: 0xbb3b38,
            destructive_hover: 0xc8443f,
            destructive_pressed: 0x9e2f2d,
            on_emphasis: 0xffffff,
            info: 0x78ace8,
            success: 0x69b183,
            warning: 0xe0a75c,
            error: 0xe3707a,
            shadow: 0x00000080,
        },
        Appearance::Light => ChromePalette {
            root: 0xdcdee3,
            shell: 0xe3e5e9,
            shell_inactive: 0xe7e9ed,
            raised: 0xffffff,
            field: 0xfbfcfd,
            hover: 0xeceef2,
            pressed: 0xe4e6ea,
            selected: 0xf6f8fb,
            selected_inactive: 0xeef0f4,
            row_selected: 0xf6f8fb,
            row_selected_rim: 0xd3d6dc,
            row_selected_text: 0x15171a,
            row_selected_secondary: 0x4d5158,
            text: 0x1d1f23,
            text_secondary: 0x555960,
            text_muted: 0x6b6f77,
            text_placeholder: 0x6e727a,
            text_disabled: 0xa2a6ad,
            separator: 0xe0e2e6,
            separator_quiet: 0xe7e9ed,
            separator_disabled: 0xe4e6ea,
            field_outline: 0xd3d6dc,
            control_outline: 0xd0d3d9,
            control_outline_strong: 0xbdc0c8,
            tab_separator: 0xbcbfc6,
            mark_outline: 0x838890,
            mark_outline_strong: 0x686e78,
            mark_track: 0xeceef1,
            mark_indicator: 0x565a62,
            mark_indicator_strong: 0x44484f,
            scrollbar_thumb: 0x7d8189,
            accent: 0x14559f,
            accent_hover: 0x0f4885,
            accent_pressed: 0x0c3a6c,
            emphasis: 0x1a63bb,
            emphasis_hover: 0x15539e,
            emphasis_pressed: 0x114582,
            destructive: 0xc23b34,
            destructive_hover: 0xa93028,
            destructive_pressed: 0x8d2621,
            on_emphasis: 0xffffff,
            info: 0x1c6bc7,
            success: 0x22713f,
            warning: 0x8a5b12,
            error: 0xb8352f,
            shadow: 0x0f172a26,
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
    hover: u32,
    pressed: u32,
    /// Persistent selection: a neutral rung, never an accent.
    selected: u32,
    /// The Active Tab's chip in an unfocused window: the same shape, a shorter step off the bar.
    ///
    /// It stays on the same side of the chrome shell as the focused chip rather than crossing to
    /// the other side of it, so an unfocused window reads as quieter rather than as inverted.
    selected_inactive: u32,
    /// The selected row of a persistent list: a chip the reader reads as the current place.
    ///
    /// This rung is authored apart from `selected` because a list row and a segmented option do
    /// not sit on the same surface. A row chip rests on the chrome shell and on the raised menu
    /// surface, so it is tuned to carry a visible edge against both without ever reaching the
    /// lightness of the raised surface itself.
    row_selected: u32,
    /// The chip's hairline. Darker than its fill in light and lighter in dark, so the same role
    /// reads as a lit edge in either appearance rather than as an outline drawn around a box.
    row_selected_rim: u32,
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

            element_hover: opaque(self.hover),
            element_active: opaque(self.pressed),
            element_selected: opaque(self.selected),
            ghost_element_hover: opaque(self.hover),
            ghost_element_active: opaque(self.pressed),
            selection_background: opaque(self.selected),
            row_background: opaque(self.shell),
            // A persistent list row is authored apart from the shared selection rung. Deriving it
            // would tie the Workspace sidebar, the Settings sections, and every menu row to the
            // fill a segmented option needs against its own track, and those surfaces differ.
            row_selected_background: opaque(self.row_selected),
            row_selected_border: opaque(self.row_selected_rim),
            row_selected_hover_border: opaque(self.row_selected_rim),
            row_selected_foreground: opaque(self.row_selected_text),
            row_selected_hover_foreground: opaque(self.row_selected_text),
            row_selected_secondary: opaque(self.row_selected_secondary),
            row_selected_hover_secondary: opaque(self.row_selected_secondary),
            // The Active Tab is an inset chip resting inside the title bar rather than a
            // full-height panel continuous with the content, so it takes the same rung as a
            // selected list row. Painting it the window root would read as a well cut into the
            // bar in dark Chrome and as a raised card in light, which is one shape describing two
            // different things.
            tab_active_background: opaque(self.row_selected),
            // The rest of that hierarchy comes along with the rung: the same lit rim, and the
            // same heavier fill and text under the pointer, so a Tab and a navigation row answer
            // hover identically.
            tab_active_foreground: opaque(self.row_selected_text),
            tab_active_border: opaque(self.row_selected_rim),
            tab_active_hover_foreground: opaque(self.row_selected_text),
            tab_inactive_selected_background: opaque(self.selected_inactive),
            tab_inactive_selected_foreground: opaque(self.text_secondary),
            // An unfocused window quietens the rim by the same proportion it quietens the fill,
            // so the Tab keeps its edge without outlining a chip that has stepped back.
            tab_inactive_selected_border: Some(
                Color::rgb(self.row_selected_rim).mix(Color::rgb(self.selected_inactive), 0.5),
            ),
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

fn vague_dark_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0xcdcdcd),
        background: Color::rgb(0x141415),
        normal: [
            0x252530, 0xd8647e, 0x7fa563, 0xf3be7c, 0x6e94b2, 0xbb9dbd, 0xaeaed1, 0xcdcdcd,
        ]
        .map(Color::rgb),
        bright: [
            0x606079, 0xe08398, 0x99b782, 0xf5cb96, 0x8ba9c1, 0xc9b1ca, 0xbebeda, 0xd7d7d7,
        ]
        .map(Color::rgb),
        dim: [
            0x18181f, 0x8e4253, 0x536c41, 0xa07d51, 0x486175, 0x7b677c, 0x727289, 0x878787,
        ]
        .map(Color::rgb),
        bright_foreground: Color::rgb(0xd7d7d7),
        dim_foreground: Color::rgb(0x878787),
        cursor: Color::rgb(0xcdcdcd),
        cursor_text: None,
        selection_background: Color::rgba(0x333738aa),
        selection_foreground: None,
        find_match_background: Color::rgba(0x6e94b266),
        find_match_foreground: None,
        find_active_match_background: Color::rgba(0xe8b58966),
        find_active_match_foreground: None,
        hyperlink: Color::rgb(0x7e98e8),
        visual_bell: Color::rgba(0xf3be7c80),
    }
}

fn spaceterm_light_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0x242426),
        background: Color::rgb(0xfbfbfc),
        normal: [
            0x343438, 0xa62f43, 0x28733d, 0x8a5300, 0x315f91, 0x78508e, 0x19717a, 0x686a70,
        ]
        .map(Color::rgb),
        bright: [
            0x777981, 0xc54258, 0x399451, 0xa96a09, 0x477cb4, 0x9868b1, 0x268f99, 0x242426,
        ]
        .map(Color::rgb),
        dim: [
            0x9a9ca3, 0x71303c, 0x315f3c, 0x654b24, 0x38536f, 0x584263, 0x315d62, 0x5f6065,
        ]
        .map(Color::rgb),
        bright_foreground: Color::rgb(0x111113),
        dim_foreground: Color::rgb(0x66686e),
        cursor: Color::rgb(0x315f91),
        cursor_text: None,
        selection_background: Color::rgba(0x9fc4ea88),
        selection_foreground: None,
        find_match_background: Color::rgba(0xf0c66c88),
        find_match_foreground: None,
        find_active_match_background: Color::rgba(0xe3964388),
        find_active_match_foreground: None,
        hyperlink: Color::rgb(0x315f91),
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

    /// A near-achromatic surface: cool enough to feel deliberate, never enough to read as a hue.
    const NEUTRAL_TINT: f64 = 0.06;

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

    /// The Active Tab rests on the same rung as the selected row of a navigation list.
    ///
    /// Both are inset chips on the chrome shell, so a palette that gave them different materials
    /// would make one window present two conventions for the same idea. The unfocused window's Tab
    /// keeps that identity with a shorter step off the bar: it has to stay on the same side of the
    /// shell, because crossing to the other side reads as a different state rather than a quieter
    /// one.
    #[test]
    fn the_active_tab_should_rest_on_the_same_chip_material_as_a_selected_row() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();

            for (role, tab, row) in [
                (
                    "fill",
                    colors.tab_active_background,
                    colors.row_selected_background,
                ),
                ("rim", colors.tab_active_border, colors.row_selected_border),
                (
                    "hovered fill",
                    colors.tab_active_hover_background,
                    colors.row_selected_hover_background,
                ),
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
            let shell = weight(colors.panel_background);
            let focused = weight(colors.tab_active_background) - shell;
            let unfocused = weight(colors.tab_inactive_selected_background) - shell;
            assert!(
                focused * unfocused > 0.0,
                "{appearance:?} should keep the unfocused Tab on the shell's lit side, got \
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
            // The unfocused rim keeps its lit side of the fill, at a shorter step than the focused one.
            let rim_step = |rim, fill| weight(rim) - weight(fill);
            let focused_rim = rim_step(colors.tab_active_border, colors.tab_active_background);
            let unfocused_rim = rim_step(
                colors.tab_inactive_selected_border,
                colors.tab_inactive_selected_background,
            );
            assert!(
                focused_rim * unfocused_rim > 0.0 && unfocused_rim.abs() < focused_rim.abs(),
                "{appearance:?} should quieten the unfocused Tab rim, got {unfocused_rim} against                  {focused_rim}"
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

    /// Each appearance separates its structural surfaces by weight rather than by hue.
    ///
    /// Root, chrome shell, hover and a selected chip are read as depth: the base is the darkest or
    /// most shaded rung and everything resting on it is lighter, in both appearances, so floating
    /// surfaces lift off the base without any color of their own.
    #[test]
    fn the_surface_ladder_should_climb_in_one_direction_in_both_appearances() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = chrome_base(appearance).opaque_presentation();
            let ladder = [
                ("root", colors.background),
                ("shell", colors.panel_background),
                ("hover", colors.element_hover),
                ("selected chip", colors.row_selected_background),
            ];

            for (role, surface) in ladder {
                assert!(
                    tint(surface) <= NEUTRAL_TINT,
                    "{appearance:?} {role} surface carries a hue cast: {surface:?}"
                );
            }
            for pair in ladder.windows(2) {
                let [(lower, from), (upper, to)] = pair else {
                    unreachable!()
                };
                let climb = weight(*to) - weight(*from);
                assert!(
                    climb > 0.0,
                    "{appearance:?} should raise {upper} above {lower}"
                );
                let step = to.contrast_ratio(*from);
                assert!(
                    step < 1.6,
                    "{appearance:?} steps from {lower} to {upper} at {step}, which reads as an edge"
                );
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
