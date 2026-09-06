use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use gpui::{App, PathPromptOptions};

pub(crate) type FinderFallbackFuture =
    Pin<Box<dyn Future<Output = Result<Option<PathBuf>, DirectoryChooserError>>>>;

pub(crate) trait FinderFallback {
    fn choose(&self, cx: &App) -> FinderFallbackFuture;
}

pub(crate) struct NativeFinderFallback;

impl FinderFallback for NativeFinderFallback {
    fn choose(&self, cx: &App) -> FinderFallbackFuture {
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
                Ok(Err(_)) => Err(DirectoryChooserError::Rejected),
                Err(_) => Err(DirectoryChooserError::Unavailable),
            }
        })
    }
}

#[cfg(test)]
pub(crate) struct ScriptedFinderFallback {
    selections: std::cell::RefCell<
        std::collections::VecDeque<Result<Option<PathBuf>, DirectoryChooserError>>,
    >,
}

#[cfg(test)]
impl ScriptedFinderFallback {
    pub(crate) fn new(
        selections: impl IntoIterator<Item = Result<Option<PathBuf>, DirectoryChooserError>>,
    ) -> Self {
        Self {
            selections: std::cell::RefCell::new(selections.into_iter().collect()),
        }
    }
}

#[cfg(test)]
impl FinderFallback for ScriptedFinderFallback {
    fn choose(&self, _: &App) -> FinderFallbackFuture {
        let result = self.selections.borrow_mut().pop_front().unwrap_or(Ok(None));
        Box::pin(async move { result })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryChooserError {
    Unavailable,
    Rejected,
}
