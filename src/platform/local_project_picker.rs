use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use gpui::{App, PathPromptOptions};

pub(crate) type LocalProjectPickerFuture =
    Pin<Box<dyn Future<Output = Result<Option<PathBuf>, String>>>>;

pub(crate) trait LocalProjectPicker {
    fn pick(&self, cx: &App) -> LocalProjectPickerFuture;
}

pub(crate) struct NativeLocalProjectPicker;

impl LocalProjectPicker for NativeLocalProjectPicker {
    fn pick(&self, cx: &App) -> LocalProjectPickerFuture {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        Box::pin(async move {
            match selection.await {
                Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

#[cfg(test)]
pub(crate) struct ScriptedLocalProjectPicker {
    selections: std::cell::RefCell<std::collections::VecDeque<Result<Option<PathBuf>, String>>>,
}

#[cfg(test)]
impl ScriptedLocalProjectPicker {
    pub(crate) fn new(
        selections: impl IntoIterator<Item = Result<Option<PathBuf>, String>>,
    ) -> Self {
        Self {
            selections: std::cell::RefCell::new(selections.into_iter().collect()),
        }
    }
}

#[cfg(test)]
impl LocalProjectPicker for ScriptedLocalProjectPicker {
    fn pick(&self, _: &App) -> LocalProjectPickerFuture {
        let result = self.selections.borrow_mut().pop_front().unwrap_or(Ok(None));
        Box::pin(async move { result })
    }
}
