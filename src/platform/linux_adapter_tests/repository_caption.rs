use std::sync::Arc;

use gpui::TestAppContext;

#[gpui::test]
fn a_long_repository_branch_with_bundled_linux_fonts_should_keep_its_segment_before_controls(
    cx: &mut TestAppContext,
) {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "SpaceTerm UI",
    ));
    let mut cx = TestAppContext::build_with_text_system(
        cx.dispatcher.clone(),
        Some("repository_caption_bounds"),
        text_system,
    );
    cx.update(|cx| {
        cx.set_global(crate::host_fonts::HostFonts {
            system_monospace_family: crate::bundled_font::FAMILY.into(),
            emoji_family: crate::bundled_font::FAMILY.into(),
            ..crate::platform::linux_fonts::capture()
        });
        crate::ui::appearance_runtime::register_fonts(cx).unwrap();
    });
    super::tests::assert_long_repository_branch_bounds(&mut cx, None);
}
