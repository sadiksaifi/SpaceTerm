use gpui::{
    Bounds, InteractiveElement as _, IntoElement as _, Modifiers, ParentElement as _, ScaledPixels,
    Styled as _, point, px, size,
};

use crate::desktop_profile::ShortcutFormatter as _;
use crate::platform::macos_shortcut_glyphs::MacosShortcutFormatter;

/// Large enough that a misplaced glyph misses by several device pixels.
const FONT_SIZE: gpui::Pixels = px(44.0);

/// Paints `shortcut` in the system font and returns each glyph's ink bounds, leading first.
fn painted_ink(shortcut: gpui::SharedString) -> Vec<Bounds<ScaledPixels>> {
    painted_label(shortcut).0
}

/// Paints `shortcut` and also returns the label's box in device pixels.
fn painted_label(
    shortcut: gpui::SharedString,
) -> (Vec<Bounds<ScaledPixels>>, Bounds<ScaledPixels>) {
    painted(move || spaceterm_ui::ShortcutLabel::new(shortcut.clone()).into_any_element())
}

/// Paints `shortcut` as plain text, where every glyph keeps its shaped position.
fn painted_plain_ink(shortcut: gpui::SharedString) -> Vec<Bounds<ScaledPixels>> {
    painted(move || shortcut.clone().into_any_element()).0
}

fn painted(
    content: impl Fn() -> gpui::AnyElement + 'static,
) -> (Vec<Bounds<ScaledPixels>>, Bounds<ScaledPixels>) {
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
                .flex()
                .font_family(".SystemUIFont")
                .text_size(FONT_SIZE)
                .child(
                    gpui::div()
                        .debug_selector(|| "shortcut".to_owned())
                        .flex_none()
                        .child(content()),
                )
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
    let scale = cx.update(|window, _| window.scale_factor());
    let label = cx.debug_bounds("shortcut").unwrap().scale(scale);
    (ink, label)
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
        let shortcut = MacosShortcutFormatter.format_chord(
            Modifiers::command(),
            key,
            &crate::platform::keyboard_layout::KeyboardLayout::default(),
        );
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
    let shortcut = MacosShortcutFormatter.format_chord(
        Modifiers::control(),
        "k",
        &crate::platform::keyboard_layout::KeyboardLayout::default(),
    );
    let ink = painted_ink(shortcut);
    assert_eq!(ink.len(), 2);
    assert!(vertical_center(ink[0]) < vertical_center(ink[1]) - 4.0);
}

#[test]
fn macos_shortcut_key_glyphs_keep_their_shaped_advance() {
    for key in ["enter", "pageup", "tab"] {
        let shortcut = MacosShortcutFormatter.format_chord(
            Modifiers::command(),
            key,
            &crate::platform::keyboard_layout::KeyboardLayout::default(),
        );
        let moved = painted_ink(shortcut.clone());
        let plain = painted_plain_ink(shortcut.clone());
        let advance = |ink: &[Bounds<ScaledPixels>]| ink[1].origin.x - ink[0].origin.x;
        assert_eq!(
            advance(&moved),
            advance(&plain),
            "`{shortcut}` moved its glyphs sideways"
        );
    }
}

#[test]
fn macos_shortcut_label_box_should_span_its_ink() {
    for key in ["enter", "k"] {
        let shortcut = MacosShortcutFormatter.format_chord(
            Modifiers::command(),
            key,
            &crate::platform::keyboard_layout::KeyboardLayout::default(),
        );
        let (ink, label) = painted_label(shortcut.clone());
        let ink_left = ink[0].left().0;
        let ink_right = ink[ink.len() - 1].right().0;
        // A glyph raster keeps up to two device pixels of antialiasing past its ink. An untrimmed
        // side bearing would miss by about nine.
        assert!(
            (ink_left - label.left().0).abs() <= 2.0 && (ink_right - label.right().0).abs() <= 2.0,
            "`{shortcut}` box {label:?} does not span its ink {ink_left}..{ink_right}"
        );
    }
}

#[test]
fn macos_shortcut_label_box_should_not_overreach_a_decomposed_glyph() {
    // Thai Sara Am shapes into two glyphs, so its whole-character bounds describe neither. The
    // label then keeps the plain text box at that edge.
    let shortcut = MacosShortcutFormatter.format_chord(
        Modifiers::command(),
        "\u{e33}",
        &crate::platform::keyboard_layout::KeyboardLayout::default(),
    );
    let (ink, label) = painted_label(shortcut.clone());
    let plain = painted({
        let shortcut = shortcut.clone();
        move || shortcut.clone().into_any_element()
    })
    .1;
    let ink_right = ink
        .iter()
        .map(|bounds| bounds.right().0)
        .fold(f32::MIN, f32::max);
    assert!(
        label.right().0 <= plain.right().0 + 1.0,
        "`{shortcut}` box {label:?} reaches past the plain text box {plain:?}"
    );
    assert!(
        ink_right - label.right().0 <= 2.0,
        "`{shortcut}` box {label:?} cuts its ink at {ink_right}"
    );
}
