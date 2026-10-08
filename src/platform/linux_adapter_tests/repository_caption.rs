use std::sync::Arc;

use gpui::TestAppContext;

#[gpui::test]
fn a_long_repository_branch_with_native_fonts_should_keep_its_segment_before_controls(
    cx: &mut TestAppContext,
) {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new("DejaVu Sans"));
    let mut cx = TestAppContext::build_with_text_system(
        cx.dispatcher.clone(),
        Some("repository_caption_bounds"),
        text_system,
    );
    super::tests::assert_long_repository_branch_bounds(&mut cx, None);
}
