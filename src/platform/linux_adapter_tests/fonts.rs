use gpui::{FontStyle, FontWeight, PlatformTextSystem};
use std::sync::Arc;

#[test]
fn linux_startup_registers_private_ui_and_terminal_faces_without_installed_fonts() {
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "SpaceTerm UI",
    ));
    let cx = gpui::TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        text.clone(),
    );
    cx.update(|cx| {
        cx.set_global(crate::platform::linux_fonts::capture());
        super::register_fonts(cx).unwrap();
    });
    let names = text.all_font_names();
    assert!(names.iter().any(|family| family == "SpaceTerm UI"));
    assert!(names.iter().any(|family| family == "SpaceTerm Default"));
    for weight in [
        FontWeight::NORMAL,
        FontWeight::MEDIUM,
        FontWeight::SEMIBOLD,
        FontWeight::BOLD,
    ] {
        let mut request = gpui::font("SpaceTerm UI");
        request.weight = weight;
        let id = text.font_id(&request).unwrap();
        assert_eq!(
            text.font_weight_and_style(id).unwrap(),
            (weight, FontStyle::Normal)
        );
        for ch in ['M', 'é', 'Ж', 'Ω'] {
            assert!(
                text.glyph_for_char(id, ch).is_some(),
                "missing chrome glyph {ch}"
            );
        }
    }
    for (weight, style) in [
        (FontWeight::NORMAL, FontStyle::Normal),
        (FontWeight::BOLD, FontStyle::Normal),
        (FontWeight::NORMAL, FontStyle::Italic),
        (FontWeight::BOLD, FontStyle::Italic),
    ] {
        let mut request = gpui::font("SpaceTerm Default");
        request.weight = weight;
        request.style = style;
        let id = text.font_id(&request).unwrap();
        assert_eq!(text.font_weight_and_style(id).unwrap(), (weight, style));
        for ch in ['M', '\u{e0b0}', '\u{f120}'] {
            assert!(
                text.glyph_for_char(id, ch).is_some(),
                "missing terminal glyph {ch}"
            );
        }
    }
}

#[test]
fn linux_fontconfig_fallback_is_a_real_resolvable_monospace_family() {
    let facts = crate::platform::linux_fonts::capture();
    assert_ne!(facts.system_monospace_family, "monospace");
    let text = gpui_wgpu::CosmicTextSystem::new("SpaceTerm UI");
    text.add_fonts(
        crate::bundled_font::FACES
            .iter()
            .map(|bytes| std::borrow::Cow::Borrowed(*bytes))
            .collect(),
    )
    .unwrap();
    let id = text
        .font_id(&gpui::font(facts.system_monospace_family))
        .unwrap();
    let widths = ['i', 'M', '0', ' '].map(|ch| {
        let glyph = text.glyph_for_char(id, ch).unwrap();
        text.advance(id, glyph).unwrap().width
    });
    assert!(
        widths
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < 0.01)
    );
}
