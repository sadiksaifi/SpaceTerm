use super::*;

#[test]
fn bundled_terminal_fonts_resolve_all_styles_and_nerd_glyphs() {
    let mut cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        Arc::new(gpui_macos::MacTextSystem::new()),
    );
    cx.update(|cx| register_terminal_fonts(cx).unwrap());
    start(&mut cx);
    cx.update(|cx| {
        let appearance = crate::ui::appearance_runtime::current(cx);
        assert_eq!(
            appearance.terminal.typography.regular.primary_family,
            "JetBrainsMono Nerd Font"
        );
        let text = cx.text_system();
        let mut faces = std::collections::HashSet::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut font = gpui::font("JetBrainsMono Nerd Font");
            if bold {
                font.weight = gpui::FontWeight::BOLD;
            }
            if italic {
                font.style = gpui::FontStyle::Italic;
            }
            let id = text.resolve_font(&font);
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
