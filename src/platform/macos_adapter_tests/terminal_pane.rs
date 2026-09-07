use super::*;

fn native_link(
    value: &str,
    directory: &std::path::Path,
    hostname: Option<&str>,
    local: crate::terminal::TerminalLocalFileCapabilities,
) -> Option<crate::terminal::HyperlinkTarget> {
    crate::terminal::HyperlinkTarget::resolve_osc8(
        value,
        directory,
        hostname,
        local,
        &crate::platform::macos_adapter_tests::local_filesystem(),
    )
}

#[gpui::test]
fn unavailable_replacement_preview_dismisses_the_previous_presentation(cx: &mut TestAppContext) {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-preview-replacement-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let first = directory.join("first");
    let second = directory.join("second");
    std::fs::write(&first, b"fixture").unwrap();
    std::fs::write(&second, b"fixture").unwrap();
    let replacement = directory.join("replacement");
    std::fs::write(&replacement, b"replacement").unwrap();
    let local = TerminalLocalFileCapabilities::Enabled;
    let first_link = native_link("file:first", &directory, None, local).unwrap();
    let second_link = native_link("file:second", &directory, None, local).unwrap();
    let previews = Rc::new(Cell::new(0));
    let dismissals = Rc::new(Cell::new(0));
    let (pane, cx, _) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.file_preview = Box::new(RecordingFilePreviewPresenter {
            previews: previews.clone(),
            dismissals: dismissals.clone(),
        });
        pane.preview_context_link(&first_link, cx);
        assert_eq!(previews.get(), 1);
        std::fs::remove_file(&second).unwrap();
        pane.preview_context_link(&second_link, cx);
        assert_eq!((previews.get(), dismissals.get()), (1, 1));
        std::fs::rename(&replacement, &second).unwrap();
        pane.preview_context_link(&second_link, cx);
        assert_eq!((previews.get(), dismissals.get()), (1, 2));
    });
    std::fs::remove_dir_all(directory).unwrap();
}
