use super::*;

#[test]
fn bundled_terminal_fonts_resolve_all_styles_and_nerd_glyphs() {
    use gpui::PlatformTextSystem as _;
    let native = Arc::new(gpui_macos::MacTextSystem::new());
    assert!(native.font_id(&gpui::font("SpaceTerm Default")).is_err());
    let mut cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        native.clone(),
    );
    cx.update(|cx| register_terminal_fonts(cx).unwrap());
    start(&mut cx);
    cx.update(|cx| {
        let appearance = crate::ui::appearance_runtime::current(cx);
        assert_eq!(
            appearance.terminal.typography.regular.primary_family,
            "SpaceTerm Default"
        );
        let text = cx.text_system();
        let mut faces = std::collections::HashSet::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut font = gpui::font("SpaceTerm Default");
            if bold {
                font.weight = gpui::FontWeight::BOLD;
            }
            if italic {
                font.style = gpui::FontStyle::Italic;
            }
            let id = native
                .font_id(&font)
                .expect("embedded family must resolve without fallback");
            faces.insert(id);
            for character in ['M', '\u{e0b0}', '\u{f120}', '\u{f015}'] {
                assert!(text.advance(id, gpui::px(18.0), character).is_ok());
            }
            let width = text.advance(id, gpui::px(18.0), 'M').unwrap().width;
            for character in ['i', '0', ' '] {
                assert_eq!(
                    text.advance(id, gpui::px(18.0), character).unwrap().width,
                    width
                );
            }
        }
        assert_eq!(
            faces.len(),
            4,
            "each terminal style must resolve its own face"
        );
    });
}
