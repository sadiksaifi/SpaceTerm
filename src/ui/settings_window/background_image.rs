//! The Background Image row: choose an image for Workspace windows. The row's reset removes it.
//!
//! Choosing reads the file off the UI thread and keeps SpaceTerm's own copy, so the Settings name
//! the copy by its digest and never the chosen path.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Window};
use spaceterm_ui::{Alert, AlertIntent, ModalAction, ModalActionRole, ModalId};

use super::SettingsWindow;
use super::import::{ImportError, read_selected_file};
use crate::background_image::{BackgroundImageError, BackgroundImageId, MAXIMUM_BYTES};
use crate::ui::background_image_runtime;
use crate::ui::sidebar_window::form::action_button;

/// Why a chosen file did not become the Background Image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChooseError {
    Read(ImportError),
    Keep(BackgroundImageError),
}

impl ChooseError {
    const fn message(self) -> &'static str {
        match self {
            Self::Read(ImportError::Unreadable) => "That file could not be read.",
            Self::Read(ImportError::TooLarge) | Self::Keep(BackgroundImageError::TooLarge) => {
                "That image is larger than 32 MB."
            }
            Self::Keep(BackgroundImageError::UnsupportedFormat) => {
                "That file is not a PNG, JPEG, HEIC, or WebP image that opens."
            }
            Self::Keep(BackgroundImageError::TooManyPixels) => {
                "That image has more than 64 megapixels."
            }
            Self::Keep(
                BackgroundImageError::Missing
                | BackgroundImageError::Unsafe
                | BackgroundImageError::Unavailable,
            ) => "SpaceTerm could not keep a copy of that image.",
        }
    }
}

/// One Choose in progress. It applies only while it is the latest choice and the Settings still
/// name the image they named when it began, so a later Remove, reset, or choice wins.
#[derive(Clone, Copy, Debug)]
pub(super) struct BackgroundImageChoice {
    generation: u64,
    before: Option<BackgroundImageId>,
}

impl SettingsWindow {
    pub(super) fn begin_background_image_choice(&mut self) -> BackgroundImageChoice {
        self.background_image_choices = self.background_image_choices.wrapping_add(1);
        BackgroundImageChoice {
            generation: self.background_image_choices,
            before: self.editor.document().appearance.window.background_image,
        }
    }

    pub(super) fn render_background_image(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        action_button(
            "settings-background-image-choose",
            "Choose…",
            self.editor.editable(),
            move |window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.choose_background_image(window, cx);
                });
            },
        )
        .into_any_element()
    }

    /// Asks for an image, keeps a copy of it, and names that copy in the Settings.
    pub(super) fn choose_background_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(store), Some(opener)) = (
            background_image_runtime::store(cx),
            cx.try_global::<crate::app::SelectedFileAccess>()
                .map(|access| Arc::clone(&access.0)),
        ) else {
            present_failure(ChooseError::Read(ImportError::Unreadable), window, cx);
            return;
        };
        let choice = self.begin_background_image_choice();
        let app = cx.to_async();
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |owner, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            // From here the runtime keeps every copy, so it cannot discard the one this choice
            // keeps before the Settings name it. It owns that copy once the choice ends, even
            // when this window closed first.
            app.update(background_image_runtime::begin_choice);
            let kept = cx
                .background_executor()
                .spawn(async move {
                    let bytes = read_selected_file(&path, opener.as_ref(), MAXIMUM_BYTES as u64)
                        .map_err(ChooseError::Read)?;
                    store.install(&bytes).map_err(ChooseError::Keep)
                })
                .await;
            let _ = owner.update_in(cx, |settings, window, cx| {
                settings.finish_background_image(choice, kept, window, cx);
            });
            app.update(|cx| background_image_runtime::end_choice(kept.ok(), cx));
        })
        .detach();
    }

    pub(super) fn finish_background_image(
        &mut self,
        choice: BackgroundImageChoice,
        kept: Result<BackgroundImageId, ChooseError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if choice.generation != self.background_image_choices
            || choice.before != self.editor.document().appearance.window.background_image
        {
            return;
        }
        match kept {
            Ok(id) => self.edit(
                move |draft| draft.appearance.window.background_image = Some(id),
                cx,
            ),
            Err(error) => present_failure(error, window, cx),
        }
    }
}

fn present_failure(error: ChooseError, window: &mut Window, cx: &mut Context<SettingsWindow>) {
    let _ = Alert::new(
        ModalId::new("settings-background-image-failed"),
        "Background image unavailable",
        "Background Image Unavailable",
        error.message(),
        vec![ModalAction::new(
            (),
            "OK",
            ModalActionRole::Cancel,
            "settings-background-image-failed-ok",
        )],
    )
    .intent(AlertIntent::Warning)
    .present(window, cx, |_, _| {});
}
