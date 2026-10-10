//! The Background Image row: choose an image for Workspace windows, or remove it.
//!
//! Choosing reads the file off the UI thread and keeps SpaceTerm's own copy, so the Settings name
//! the copy by its digest and never the chosen path.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Window, div};
use spaceterm_ui::{Alert, AlertIntent, ModalAction, ModalActionRole, ModalId};

use super::SettingsWindow;
use super::import::{ImportError, read_selected_file};
use crate::background_image::{BackgroundImageError, BackgroundImageId, MAXIMUM_BYTES};
use crate::ui::appearance::ChromeAppearance;
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
                "That file is not a PNG, JPEG, HEIC, or WebP image."
            }
            Self::Keep(
                BackgroundImageError::Missing
                | BackgroundImageError::Unsafe
                | BackgroundImageError::Unavailable,
            ) => "SpaceTerm could not keep a copy of that image.",
        }
    }
}

impl SettingsWindow {
    pub(super) fn render_background_image(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editable = self.editor.editable();
        let owner = cx.weak_entity();
        let choose = action_button(
            "settings-background-image-choose",
            "Choose…",
            editable,
            move |window, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.choose_background_image(window, cx);
                });
            },
        );
        let chosen = self
            .editor
            .document()
            .appearance
            .window
            .background_image
            .is_some();
        let owner = cx.weak_entity();
        let remove = chosen.then(|| {
            action_button(
                "settings-background-image-remove",
                "Remove",
                editable,
                move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.edit(|draft| draft.appearance.window.background_image = None, cx);
                    });
                },
            )
        });
        div()
            .debug_selector(|| "settings-background-image".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .children(remove)
            .child(choose)
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
            let kept = cx
                .background_executor()
                .spawn(async move {
                    let bytes = read_selected_file(&path, opener.as_ref(), MAXIMUM_BYTES as u64)
                        .map_err(ChooseError::Read)?;
                    store.install(&bytes).map_err(ChooseError::Keep)
                })
                .await;
            let _ = owner.update_in(cx, |settings, window, cx| {
                settings.finish_background_image(kept, window, cx);
            });
        })
        .detach();
    }

    pub(super) fn finish_background_image(
        &mut self,
        kept: Result<BackgroundImageId, ChooseError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
