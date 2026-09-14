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
            "SpaceTerm roles extracted from the pinned Vague Pro source",
        )),
    };
    let spaceterm_metadata = SchemeMetadata {
        origin: None,
        author: Some(String::from("SpaceTerm contributors")),
        license: Some(String::from("MIT")),
        description: Some(String::from("SpaceTerm-owned light appearance")),
    };
    vec![
        CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: vague_chrome_id(),
            name: String::from("Vague Pro Dark"),
            appearance: Appearance::Dark,
            metadata: vague_metadata.clone(),
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
            metadata: spaceterm_metadata.clone(),
            colors: chrome_definition(Appearance::Light),
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

pub(super) fn chrome_definition(appearance: Appearance) -> ChromeColorOverrides {
    let values = match appearance {
        Appearance::Dark => [
            0x141415, 0x141415, 0x1c1c24, 0xcdcdcd, 0xa0a0ab, 0x9898a3, 0x606079, 0x6e94b2,
            0x7e98e8, 0x7fa563, 0xf3be7c, 0xd8647e,
        ],
        Appearance::Light => [
            0xf7f7f9, 0xeeeef2, 0xffffff, 0x202124, 0x45474d, 0x5c6069, 0x8d919a, 0x315f91,
            0x285f9e, 0x28733d, 0x8a5300, 0xa62f43,
        ],
    }
    .map(Color::rgb);
    ChromeColorOverrides {
        background: Some(values[0]),
        panel_background: Some(values[1]),
        elevated_surface_background: Some(values[2]),
        text: Some(values[3]),
        text_secondary: Some(values[4]),
        text_muted: Some(values[5]),
        text_disabled: Some(values[6]),
        text_accent: Some(values[7]),
        info: Some(values[8]),
        success: Some(values[9]),
        warning: Some(values[10]),
        error: Some(values[11]),
        ..ChromeColorOverrides::default()
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
}
