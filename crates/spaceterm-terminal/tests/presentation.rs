use std::sync::Arc;

use spaceterm_terminal::appearance::{
    Appearance, AppearanceGeneration, Color, ResolvedTerminalAppearance, TerminalAppearanceUpdate,
    TerminalColors,
};
use spaceterm_terminal::local_path::LocalPathSemantics;
use spaceterm_terminal::{PresentationGeneration, ScreenSnapshot, TerminalDefaultColorSource};

#[test]
fn presentation_projection_preserves_program_overrides_and_applied_generation() {
    let initial = TerminalAppearanceUpdate::new(
        AppearanceGeneration::new(3),
        Arc::new(ResolvedTerminalAppearance {
            appearance: Appearance::Dark,
            colors: TerminalColors::default(),
            bold_as_bright: true,
        }),
    );
    let mut retained = ScreenSnapshot::empty_with_appearance(LocalPathSemantics::Posix, &initial);
    let screen = Arc::make_mut(&mut retained);
    screen.generation = PresentationGeneration::default().next();
    screen.colors.foreground = Color::rgb(0x123456);
    screen.colors.foreground_source = TerminalDefaultColorSource::ProgramOverride;
    Arc::make_mut(&mut screen.colors.palette)[2] = Color::rgb(0xabcdef);
    Arc::make_mut(&mut screen.colors.palette_overrides)[2] = true;

    let mut replacement = (*initial.appearance).clone();
    replacement.appearance = Appearance::Light;
    replacement.colors.foreground = Color::WHITE;
    replacement.colors.background = Color::rgb(0xf0f0f0);
    replacement.colors.normal[2] = Color::BLACK;
    let projected = ScreenSnapshot::projected_for_renderer(&retained, &replacement);

    assert_eq!(
        projected.appearance_generation,
        AppearanceGeneration::new(3)
    );
    assert_eq!(projected.terminal_appearance, Appearance::Light);
    assert_eq!(projected.colors.foreground, Color::rgb(0x123456));
    assert_eq!(projected.background, Color::rgb(0xf0f0f0));
    assert_eq!(projected.colors.palette[2], Color::rgb(0xabcdef));
    assert_eq!(retained.background, initial.appearance.colors.background);
}
