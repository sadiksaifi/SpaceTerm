//! Validated desktop policy supplied by host composition, with no host detection.
pub(crate) mod keybindings;

use gpui::{App, KeyBinding};
use spaceterm_ui::{ModalDesktopPolicy, ModalKeybindingProfile, TextInputKeybindingProfile};

#[derive(Clone, Copy)]
pub(crate) struct FileInteractionLabels {
    pub(crate) directory_selection: &'static str,
    pub(crate) file_preview: &'static str,
}
impl Default for FileInteractionLabels {
    fn default() -> Self {
        Self {
            directory_selection: "Choose with System",
            file_preview: "Preview File",
        }
    }
}
impl gpui::Global for FileInteractionLabels {}
impl FileInteractionLabels {
    pub(crate) fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }
}

pub(crate) struct DesktopProfile {
    file_labels: FileInteractionLabels,
    modal_policy: ModalDesktopPolicy,
    modal_keys: ModalKeybindingProfile,
    text_keys: TextInputKeybindingProfile,
    bindings: Vec<KeyBinding>,
    locale: std::rc::Rc<dyn crate::platform::locale::LocaleDirection>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum DesktopProfileError {
    #[error("a required desktop capability is unavailable")]
    MissingCapability,
    #[error("desktop policy and capabilities disagree")]
    InvalidCombination,
    #[error("a global shortcut has more than one owner")]
    DuplicateGlobalKey,
}
impl DesktopProfile {
    pub(crate) fn new(
        modal_policy: ModalDesktopPolicy,
        modal_keys: ModalKeybindingProfile,
        text_keys: TextInputKeybindingProfile,
        bindings: Vec<KeyBinding>,
        locale: std::rc::Rc<dyn crate::platform::locale::LocaleDirection>,
    ) -> Result<Self, DesktopProfileError> {
        for (index, binding) in bindings.iter().enumerate() {
            if binding.predicate().is_none()
                && bindings[..index].iter().any(|previous| {
                    previous.predicate().is_none() && previous.keystrokes() == binding.keystrokes()
                })
            {
                return Err(DesktopProfileError::DuplicateGlobalKey);
            }
        }
        Ok(Self {
            file_labels: FileInteractionLabels::default(),
            modal_policy,
            modal_keys,
            text_keys,
            bindings,
            locale,
        })
    }
    pub(crate) fn with_file_labels(mut self, labels: FileInteractionLabels) -> Self {
        self.file_labels = labels;
        self
    }
    pub(crate) fn install(&self, cx: &mut App) {
        cx.set_global(self.file_labels);
        spaceterm_ui::install_modal_policy(
            cx,
            self.modal_policy
                .with_text_direction(self.locale.text_direction()),
        );
        spaceterm_ui::install_modal_keybindings(cx, self.modal_keys);
        spaceterm_ui::install_text_input_keybindings(cx, self.text_keys);
        cx.bind_keys(self.bindings.clone());
    }
}
#[cfg(test)]
pub(crate) fn testing_profile(direction: spaceterm_ui::TextDirection) -> DesktopProfile {
    DesktopProfile::new(
        ModalDesktopPolicy::mac_os(),
        ModalKeybindingProfile::MacOs,
        TextInputKeybindingProfile::MacOs,
        keybindings::bindings(),
        std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(direction)),
    )
    .expect("valid explicit desktop profile fixture")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{CreateScratchWorkspace, OpenLocalProject, ShowNewWorkspacePanel};
    use gpui::Action;

    #[gpui::test]
    fn complete_profile_preserves_141_bindings_and_remaps_only_three(
        cx: &mut gpui::TestAppContext,
    ) {
        let actual = cx.update(|cx| {
            crate::ui::init(cx).unwrap();
            crate::app::init(cx);
            cx.key_bindings()
                .borrow()
                .bindings()
                .map(|binding| {
                    let keys = binding
                        .keystrokes()
                        .iter()
                        .map(|key| key.unparse())
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!(
                        "{}\t{:?}\t{}",
                        keys,
                        binding.predicate(),
                        binding.action().name()
                    )
                })
                .collect::<Vec<_>>()
        });
        // Captured from the original installed GPUI keymap, including every control context.
        let mut expected = include_str!("keybindings_baseline.txt")
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), 144);
        for (action, shortcut) in [
            (CreateScratchWorkspace.name(), "cmd-shift-n"),
            (ShowNewWorkspacePanel.name(), "cmd-n"),
            (OpenLocalProject.name(), "cmd-o"),
        ] {
            let entry = expected
                .iter_mut()
                .find(|line| line.ends_with(action))
                .unwrap();
            *entry = format!("{shortcut}\tNone\t{action}");
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn duplicate_global_shortcuts_are_rejected_after_modifier_normalization() {
        let result = DesktopProfile::new(
            ModalDesktopPolicy::mac_os(),
            ModalKeybindingProfile::MacOs,
            TextInputKeybindingProfile::MacOs,
            vec![
                KeyBinding::new("cmd-shift-n", CreateScratchWorkspace, None),
                KeyBinding::new("shift-cmd-n", ShowNewWorkspacePanel, None),
            ],
            std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(
                spaceterm_ui::TextDirection::LeftToRight,
            )),
        );
        assert_eq!(result.err(), Some(DesktopProfileError::DuplicateGlobalKey));
    }
}
