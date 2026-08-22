use std::path::PathBuf;

use gpui::{App, PathPromptOptions, SharedString, Task};

pub(crate) trait LocalProjectPicker {
    /// Begin a native directory-only selection. `None` means the selection was
    /// cancelled or produced no usable path.
    fn pick_local_project_directory(
        &self,
        prompt: SharedString,
        cx: &mut App,
    ) -> Task<Option<PathBuf>>;
}

/// Presents macOS's native directory-only open panel through GPUI.
pub(crate) struct MacosDirectoryPicker;

impl LocalProjectPicker for MacosDirectoryPicker {
    fn pick_local_project_directory(
        &self,
        prompt: SharedString,
        cx: &mut App,
    ) -> Task<Option<PathBuf>> {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt),
        });
        cx.spawn(async move |_| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return None;
            };
            paths.into_iter().next()
        })
    }
}
