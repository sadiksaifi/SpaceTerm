use gpui::{Bounds, Modifiers, ParentElement as _, ScaledPixels, Styled as _, point, px, size};

use crate::desktop_profile::ShortcutFormatter as _;
use crate::platform::macos_shortcut_glyphs::MacosShortcutFormatter;

/// Large enough that a misplaced glyph misses by several device pixels.
const FONT_SIZE: gpui::Pixels = px(44.0);

/// Paints `shortcut` in the system font and returns each glyph's ink bounds, leading first.
fn painted_ink(shortcut: gpui::SharedString) -> Vec<Bounds<ScaledPixels>> {
    let mut cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        std::sync::Arc::new(gpui_macos::MacTextSystem::new()),
    );
    let cx = cx.add_empty_window();
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(400.0), px(120.0)),
        move |_, _| {
            gpui::div()
                .font_family(".SystemUIFont")
                .text_size(FONT_SIZE)
                .child(spaceterm_ui::ShortcutLabel::new(shortcut))
        },
    );
    let mut ink = cx.update(|window, _| {
        window
            .painted_monochrome_sprites()
            .into_iter()
            .map(|sprite| sprite.bounds)
            .collect::<Vec<_>>()
    });
    ink.sort_by(|left, right| left.origin.x.0.total_cmp(&right.origin.x.0));
    ink
}

fn vertical_center(bounds: Bounds<ScaledPixels>) -> f32 {
    bounds.origin.y.0 + bounds.size.height.0 / 2.0
}

#[test]
fn macos_shortcut_key_glyphs_paint_on_the_command_glyph_center() {
    for key in [
        "enter",
        "tab",
        "escape",
        "backspace",
        "delete",
        "left",
        "right",
        "up",
        "down",
        "home",
        "end",
        "pageup",
        "pagedown",
        "k",
    ] {
        let shortcut = MacosShortcutFormatter.format_chord(Modifiers::command(), key);
        let ink = painted_ink(shortcut.clone());
        assert_eq!(ink.len(), 2, "`{shortcut}` did not paint two glyphs");
        let miss = vertical_center(ink[1]) - vertical_center(ink[0]);
        assert!(
            miss.abs() <= 1.0,
            "`{shortcut}` paints its key {miss} device pixels from the Command glyph's center"
        );
    }
}

#[test]
fn macos_shortcut_control_caret_keeps_its_raised_design() {
    let shortcut = MacosShortcutFormatter.format_chord(Modifiers::control(), "k");
    let ink = painted_ink(shortcut);
    assert_eq!(ink.len(), 2);
    assert!(vertical_center(ink[0]) < vertical_center(ink[1]) - 4.0);
}
