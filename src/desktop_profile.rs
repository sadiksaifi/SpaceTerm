//! Validated desktop policy supplied by host composition, with no host detection.
pub(crate) mod keybindings;

use gpui::{Action, App, KeyBinding};
use spaceterm_ui::{
    ComboBoxKeybindingProfile, CommandPaletteKeybindingProfile, ModalDesktopPolicy,
    ModalKeybindingProfile, TextInputKeybindingProfile,
};

#[derive(Clone, Copy)]
pub(crate) struct DesktopWording {
    pub(crate) directory_selection: &'static str,
    pub(crate) file_preview: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct ActionShortcut {
    action: &'static str,
    display: &'static str,
}

impl ActionShortcut {
    pub(crate) fn new<A: Action>(action: A, display: &'static str) -> Self {
        Self {
            action: action.name(),
            display,
        }
    }
}

#[derive(Clone)]
pub(crate) struct DesktopPresentation {
    wording: DesktopWording,
    command_palette_confirm_shortcut: &'static str,
    shortcuts: Vec<ActionShortcut>,
}

impl DesktopPresentation {
    pub(crate) fn new(
        wording: DesktopWording,
        command_palette_confirm_shortcut: &'static str,
        shortcuts: Vec<ActionShortcut>,
    ) -> Self {
        Self {
            wording,
            command_palette_confirm_shortcut,
            shortcuts,
        }
    }

    pub(crate) fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub(crate) const fn wording(&self) -> DesktopWording {
        self.wording
    }

    pub(crate) const fn command_palette_confirm_shortcut(&self) -> &'static str {
        self.command_palette_confirm_shortcut
    }

    pub(crate) fn shortcut<A: Action>(&self, action: &A) -> &'static str {
        self.shortcuts
            .iter()
            .find(|presentation| presentation.action == action.name())
            .expect("desktop profile validation must cover every displayed action")
            .display
    }
}
impl gpui::Global for DesktopPresentation {}

pub(crate) struct DesktopProfile {
    presentation: DesktopPresentation,
    modal_policy: ModalDesktopPolicy,
    control_keys: ControlKeybindingProfiles,
    bindings: Vec<KeyBinding>,
    locale: std::rc::Rc<dyn crate::platform::locale::LocaleDirection>,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlKeybindingProfiles {
    modal: ModalKeybindingProfile,
    command_palette: CommandPaletteKeybindingProfile,
    combo_box: ComboBoxKeybindingProfile,
    text_input: TextInputKeybindingProfile,
}

impl ControlKeybindingProfiles {
    pub(crate) const fn new(
        modal: ModalKeybindingProfile,
        command_palette: CommandPaletteKeybindingProfile,
        combo_box: ComboBoxKeybindingProfile,
        text_input: TextInputKeybindingProfile,
    ) -> Self {
        Self {
            modal,
            command_palette,
            combo_box,
            text_input,
        }
    }
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
        control_keys: ControlKeybindingProfiles,
        bindings: Vec<KeyBinding>,
        presentation: DesktopPresentation,
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
        if presentation.command_palette_confirm_shortcut.is_empty() {
            return Err(DesktopProfileError::MissingCapability);
        }
        for (index, shortcut) in presentation.shortcuts.iter().enumerate() {
            if shortcut.display.is_empty()
                || !bindings
                    .iter()
                    .any(|binding| binding.action().name() == shortcut.action)
                || presentation.shortcuts[..index]
                    .iter()
                    .any(|previous| previous.action == shortcut.action)
            {
                return Err(DesktopProfileError::InvalidCombination);
            }
        }
        if required_presented_actions().iter().any(|required| {
            !presentation
                .shortcuts
                .iter()
                .any(|shortcut| shortcut.action == *required)
        }) {
            return Err(DesktopProfileError::MissingCapability);
        }
        Ok(Self {
            presentation,
            modal_policy,
            control_keys,
            bindings,
            locale,
        })
    }
    pub(crate) fn install(&self, cx: &mut App) {
        cx.set_global(self.presentation.clone());
        spaceterm_ui::install_modal_policy(
            cx,
            self.modal_policy
                .with_text_direction(self.locale.text_direction()),
        );
        spaceterm_ui::install_command_palette_keybindings(cx, self.control_keys.command_palette);
        spaceterm_ui::install_portable_combo_box_keybindings(cx);
        spaceterm_ui::install_combo_box_keybindings(cx, self.control_keys.combo_box);
        spaceterm_ui::install_portable_modal_keybindings(cx);
        spaceterm_ui::install_modal_keybindings(cx, self.control_keys.modal);
        spaceterm_ui::install_text_input_keybindings(cx, self.control_keys.text_input);
        cx.bind_keys(self.bindings.clone());
    }
}

fn required_presented_actions() -> [&'static str; 20] {
    use crate::ui::OpenTerminalFind;
    use crate::ui::{
        ClosePane, CloseTab, CreateTab, NewWorkspace, SplitDown, SplitRight, SwitchWorkspace,
        TogglePaneZoom,
    };
    use spaceterm_ui::{EditCopy, EditPaste};

    [
        crate::ui::ActivateWorkspace1.name(),
        crate::ui::ActivateWorkspace2.name(),
        crate::ui::ActivateWorkspace3.name(),
        crate::ui::ActivateWorkspace4.name(),
        crate::ui::ActivateWorkspace5.name(),
        crate::ui::ActivateWorkspace6.name(),
        crate::ui::ActivateWorkspace7.name(),
        crate::ui::ActivateWorkspace8.name(),
        crate::ui::ActivateWorkspace9.name(),
        SwitchWorkspace.name(),
        NewWorkspace.name(),
        CreateTab.name(),
        EditCopy.name(),
        EditPaste.name(),
        OpenTerminalFind.name(),
        SplitRight.name(),
        SplitDown.name(),
        TogglePaneZoom.name(),
        ClosePane.name(),
        CloseTab.name(),
    ]
}

#[cfg(test)]
pub(crate) fn testing_presentation() -> DesktopPresentation {
    use crate::ui::OpenTerminalFind;
    use crate::ui::{
        ClosePane, CloseTab, CreateTab, NewWorkspace, SplitDown, SplitRight, SwitchWorkspace,
        TogglePaneZoom,
    };
    use spaceterm_ui::{EditCopy, EditPaste};

    DesktopPresentation::new(
        DesktopWording {
            directory_selection: "Choose Directory",
            file_preview: "Preview File",
        },
        "Primary+Enter",
        vec![
            ActionShortcut::new(crate::ui::ActivateWorkspace1, "Ctrl+1"),
            ActionShortcut::new(crate::ui::ActivateWorkspace2, "Ctrl+2"),
            ActionShortcut::new(crate::ui::ActivateWorkspace3, "Ctrl+3"),
            ActionShortcut::new(crate::ui::ActivateWorkspace4, "Ctrl+4"),
            ActionShortcut::new(crate::ui::ActivateWorkspace5, "Ctrl+5"),
            ActionShortcut::new(crate::ui::ActivateWorkspace6, "Ctrl+6"),
            ActionShortcut::new(crate::ui::ActivateWorkspace7, "Ctrl+7"),
            ActionShortcut::new(crate::ui::ActivateWorkspace8, "Ctrl+8"),
            ActionShortcut::new(crate::ui::ActivateWorkspace9, "Ctrl+9"),
            ActionShortcut::new(SwitchWorkspace, "Primary+K"),
            ActionShortcut::new(NewWorkspace, "Primary+N"),
            ActionShortcut::new(CreateTab, "Primary+T"),
            ActionShortcut::new(EditCopy, "Primary+C"),
            ActionShortcut::new(EditPaste, "Primary+V"),
            ActionShortcut::new(OpenTerminalFind, "Primary+F"),
            ActionShortcut::new(SplitRight, "Primary+D"),
            ActionShortcut::new(SplitDown, "Primary+Shift+D"),
            ActionShortcut::new(TogglePaneZoom, "Primary+Shift+Enter"),
            ActionShortcut::new(ClosePane, "Primary+W"),
            ActionShortcut::new(CloseTab, "Primary+Shift+W"),
        ],
    )
}

#[cfg(test)]
pub(crate) fn testing_profile(direction: spaceterm_ui::TextDirection) -> DesktopProfile {
    DesktopProfile::new(
        ModalDesktopPolicy::mac_os(),
        ControlKeybindingProfiles::new(
            ModalKeybindingProfile::MacOs,
            CommandPaletteKeybindingProfile::MacOs,
            ComboBoxKeybindingProfile::MacOs,
            TextInputKeybindingProfile::MacOs,
        ),
        keybindings::bindings(),
        testing_presentation(),
        std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(direction)),
    )
    .expect("valid explicit desktop profile fixture")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{NewWorkspace, SwitchWorkspace};
    use gpui::Action;

    #[gpui::test]
    fn complete_profile_installs_the_expected_bindings(cx: &mut gpui::TestAppContext) {
        let actual = cx.update(|cx| {
            crate::ui::init(cx).unwrap();
            crate::app::init(
                cx,
                std::rc::Rc::new(
                    crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
                ),
            );
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
        let expected = include_str!("keybindings_baseline.txt")
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn duplicate_global_shortcuts_are_rejected_after_modifier_normalization() {
        let result = DesktopProfile::new(
            ModalDesktopPolicy::mac_os(),
            ControlKeybindingProfiles::new(
                ModalKeybindingProfile::MacOs,
                CommandPaletteKeybindingProfile::MacOs,
                ComboBoxKeybindingProfile::MacOs,
                TextInputKeybindingProfile::MacOs,
            ),
            vec![
                KeyBinding::new("cmd-shift-k", SwitchWorkspace, None),
                KeyBinding::new("shift-cmd-k", NewWorkspace, None),
            ],
            testing_presentation(),
            std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(
                spaceterm_ui::TextDirection::LeftToRight,
            )),
        );
        assert_eq!(result.err(), Some(DesktopProfileError::DuplicateGlobalKey));
    }

    #[test]
    fn missing_required_action_presentation_is_rejected() {
        let mut presentation = testing_presentation();
        presentation
            .shortcuts
            .retain(|shortcut| shortcut.action != NewWorkspace.name());

        let result = DesktopProfile::new(
            ModalDesktopPolicy::mac_os(),
            ControlKeybindingProfiles::new(
                ModalKeybindingProfile::MacOs,
                CommandPaletteKeybindingProfile::MacOs,
                ComboBoxKeybindingProfile::MacOs,
                TextInputKeybindingProfile::MacOs,
            ),
            keybindings::bindings(),
            presentation,
            std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(
                spaceterm_ui::TextDirection::LeftToRight,
            )),
        );

        assert_eq!(result.err(), Some(DesktopProfileError::MissingCapability));
    }
}
