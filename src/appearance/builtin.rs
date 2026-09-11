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
        author: Some(String::from("Vague Theme contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from(
            "SpaceTerm roles extracted from the pinned Vague Pro source",
        )),
    };
    let spaceterm_metadata = SchemeMetadata {
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned light appearance")),
    };
    vec![
        CustomScheme::Chrome(Box::new(ChromeScheme {
            id: vague_chrome_id(),
            name: String::from("Vague Pro Dark"),
            appearance: Appearance::Dark,
            metadata: vague_metadata.clone(),
            colors: ChromeColorOverrides::default(),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: vague_terminal_id(),
            name: String::from("Vague Pro Dark"),
            appearance: Appearance::Dark,
            metadata: vague_metadata,
            colors: TerminalColorOverrides::default(),
        })),
        CustomScheme::Chrome(Box::new(ChromeScheme {
            id: light_chrome_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: spaceterm_metadata.clone(),
            colors: ChromeColorOverrides::default(),
        })),
        CustomScheme::Terminal(Box::new(TerminalScheme {
            id: light_terminal_id(),
            name: String::from("SpaceTerm Light"),
            appearance: Appearance::Light,
            metadata: spaceterm_metadata,
            colors: TerminalColorOverrides::default(),
        })),
    ]
}

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

struct ChromePalette {
    background: Color,
    panel: Color,
    elevated: Color,
    raised: Color,
    inactive: Color,
    text: Color,
    secondary: Color,
    muted: Color,
    icon_muted: Color,
    disabled: Color,
    accent: Color,
    accent_hover: Color,
    border: Color,
    focus: Color,
    selection: Color,
    hover: Color,
    info: Color,
    success: Color,
    warning: Color,
    error: Color,
}

fn chrome_from_palette(p: ChromePalette) -> ChromeColors {
    let transparent = Color::rgba(0x00000000);
    let selection_overlay = Color {
        a: 0x66,
        ..p.accent
    };
    let status_background = |color: Color| Color { a: 0x1a, ..color };
    ChromeColors {
        background: p.background,
        panel_background: p.panel,
        elevated_surface_background: p.elevated,
        title_bar_background: p.background,
        title_bar_inactive_background: p.inactive,
        tab_active_background: p.raised,
        tab_inactive_background: p.background,
        text: p.text,
        text_secondary: p.secondary,
        text_muted: p.muted,
        text_placeholder: p.disabled,
        text_disabled: p.disabled,
        text_accent: p.accent,
        link_text: p.accent,
        link_text_hover: p.accent_hover,
        icon: p.text,
        icon_muted: p.icon_muted,
        icon_disabled: p.disabled,
        icon_accent: p.accent,
        border: p.border,
        border_variant: p.border,
        border_focused: p.focus,
        border_selected: p.accent,
        border_disabled: p.border,
        border_transparent: transparent,
        element_background: p.background,
        element_hover: p.hover,
        element_active: p.raised,
        element_selected: p.raised,
        element_selected_hover: p.selection,
        element_disabled: p.background,
        element_foreground: p.text,
        element_hover_foreground: p.text,
        element_active_foreground: p.text,
        element_selected_foreground: p.text,
        element_selected_hover_foreground: p.text,
        element_disabled_foreground: p.disabled,
        ghost_element_background: transparent,
        ghost_element_hover: p.hover,
        ghost_element_active: p.raised,
        ghost_element_selected: p.raised,
        ghost_element_selected_hover: p.selection,
        ghost_element_disabled: p.background,
        ghost_element_foreground: p.text,
        ghost_element_hover_foreground: p.text,
        ghost_element_active_foreground: p.text,
        ghost_element_selected_foreground: p.text,
        ghost_element_selected_hover_foreground: p.text,
        ghost_element_disabled_foreground: p.disabled,
        navigation_selection: p.accent,
        sidebar_selection: p.raised,
        sidebar_selection_hover: p.selection,
        sidebar_hover: p.hover,
        sidebar_focus: p.focus,
        info: p.info,
        info_background: status_background(p.info),
        success: p.success,
        warning: p.warning,
        warning_background: status_background(p.warning),
        warning_border: p.warning,
        error: p.error,
        error_background: status_background(p.error),
        error_border: p.error,
        input_text: p.text,
        input_placeholder: p.disabled,
        input_disabled_text: p.disabled,
        input_caret: p.text,
        input_selection_background: selection_overlay,
        input_background: p.background,
        input_disabled_background: p.background,
        input_border: p.border,
        input_focused_border: p.focus,
        input_invalid_border: p.error,
        modal_scrim: Color {
            a: 0x99,
            ..p.background
        },
        modal_checkbox: p.border,
        modal_checkbox_selected: p.accent,
        modal_checkbox_focused: p.focus,
        modal_checkbox_disabled: p.disabled,
        scrollbar_track: transparent,
        scrollbar_track_border: transparent,
        scrollbar_thumb_background: Color {
            a: 0x78,
            ..p.selection
        },
        scrollbar_thumb_border: transparent,
        scrollbar_thumb_hover_background: Color {
            a: 0x78,
            ..p.disabled
        },
        resize_idle: p.border,
        resize_focused: p.focus,
        resize_hovered: p.accent,
        resize_dragged: p.accent,
        resize_disabled: p.border,
        shadow: Color {
            a: 0x1a,
            ..Color::rgb(0x000000)
        },
    }
}

fn vague_dark_chrome() -> ChromeColors {
    chrome_from_palette(ChromePalette {
        background: Color::rgb(0x141415),
        panel: Color::rgb(0x141415),
        elevated: Color::rgb(0x141415),
        raised: Color::rgb(0x252530),
        inactive: Color::rgb(0x1c1c24),
        text: Color::rgb(0xcdcdcd),
        secondary: Color::rgb(0x8f8f8f),
        muted: Color::rgb(0x878787),
        icon_muted: Color::rgb(0x606079),
        disabled: Color::rgb(0x606079),
        accent: Color::rgb(0x6e94b2),
        accent_hover: Color::rgb(0x7e98e8),
        border: Color::rgb(0x252530),
        focus: Color::rgb(0x405065),
        selection: Color::rgb(0x333738),
        hover: Color::rgb(0x252530),
        info: Color::rgb(0x7e98e8),
        success: Color::rgb(0x7fa563),
        warning: Color::rgb(0xf3be7c),
        error: Color::rgb(0xd8647e),
    })
}

fn spaceterm_light_chrome() -> ChromeColors {
    chrome_from_palette(ChromePalette {
        background: Color::rgb(0xf7f7f9),
        panel: Color::rgb(0xeeeef2),
        elevated: Color::rgb(0xffffff),
        raised: Color::rgb(0xe4e5ea),
        inactive: Color::rgb(0xededf1),
        text: Color::rgb(0x202124),
        secondary: Color::rgb(0x45474d),
        muted: Color::rgb(0x666a73),
        icon_muted: Color::rgb(0x666a73),
        disabled: Color::rgb(0x8d919a),
        accent: Color::rgb(0x315f91),
        accent_hover: Color::rgb(0x244d78),
        border: Color::rgb(0xc9cbd2),
        focus: Color::rgb(0x315f91),
        selection: Color::rgb(0xd7e5f4),
        hover: Color::rgb(0xe9eaf0),
        info: Color::rgb(0x285f9e),
        success: Color::rgb(0x28733d),
        warning: Color::rgb(0x8a5300),
        error: Color::rgb(0xa62f43),
    })
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
