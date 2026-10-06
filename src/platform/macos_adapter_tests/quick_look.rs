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
        &crate::platform::unix_adapter_tests::local_filesystem(),
    )
}

#[gpui::test]
fn presenter_rejects_a_replaced_file_before_calling_the_platform(cx: &mut gpui::TestAppContext) {
    let window = cx.add_window(|_, _| gpui::EmptyView);
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-file-preview-platform-replaced-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    let replacement = directory.join("replacement.txt");
    fs::write(&file, b"preview").unwrap();
    let link = native_link("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
    let target = FilePreviewTarget::from_link(&link, LOCAL_FILES).unwrap();
    fs::write(&replacement, b"replacement").unwrap();
    fs::rename(replacement, &file).unwrap();
    let mut presenter = FilePreviewPresenter::new(RecordingPanel::default());

    let result = window
        .update(cx, |_, window, cx| {
            presenter.preview_in_window(&target, window, cx)
        })
        .unwrap();

    assert_eq!(result.err(), Some(FilePreviewError::StaleTarget));
    assert_eq!(
        (presenter.panel.previews.len(), presenter.panel.dismissals),
        (0, 1)
    );
    fs::remove_dir_all(directory).unwrap();
}
