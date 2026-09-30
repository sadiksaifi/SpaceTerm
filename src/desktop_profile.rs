//! Validated desktop policy supplied by host composition, with no host detection.
pub(crate) mod default_keymap;

use std::rc::Rc;

use crate::keybindings::{KeybindingPreferences, KeymapProfile, Shortcut};
use gpui::{Action, App, KeyContext, Keymap, Modifiers, SharedString};
use spaceterm_ui::{
    ComboBoxKeybindingProfile, CommandPaletteKeybindingProfile, MenuKeybindingProfile,
    ModalDesktopPolicy, ModalKeybindingProfile, TextInputKeybindingProfile,
};

#[derive(Clone, Copy)]
pub(crate) struct DesktopWording {
    pub(crate) file_preview: &'static str,
    pub(crate) operating_system_name: &'static str,
    /// The command that opens System Directory Selection.
    pub(crate) system_directory_selection: &'static str,
}

pub(crate) trait ShortcutFormatter {
    /// Presents one key with its modifiers. The key is GPUI's lowercase key name, such as `k`,
    /// `enter`, or `f5`.
    fn format_chord(&self, modifiers: Modifiers, key: &str) -> SharedString;
    fn format_modifiers(&self, modifiers: Modifiers) -> SharedString;
}

/// Which installed binding a desktop presents for an action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutSelection {
    /// The binding GPUI's native application menu shows, so hints agree with the menu bar.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only a desktop with a native menu bar composes this selection")
    )]
    NativeMenu,
    /// The binding that reaches the terminal first, for a desktop without a native menu whose
    /// text fields use other chords than the terminal.
    TerminalSurface,
}

#[derive(Clone)]
pub(crate) struct DesktopPresentation {
    wording: DesktopWording,
    formatter: Rc<dyn ShortcutFormatter>,
    selection: ShortcutSelection,
    shortcuts: Vec<PresentedShortcut>,
}

struct PresentedShortcut {
    action: Box<dyn Action>,
    display: SharedString,
}

impl Clone for PresentedShortcut {
    fn clone(&self) -> Self {
        Self {
            action: self.action.boxed_clone(),
            display: self.display.clone(),
        }
    }
}

impl DesktopPresentation {
    pub(crate) fn new(
        wording: DesktopWording,
        formatter: Rc<dyn ShortcutFormatter>,
        selection: ShortcutSelection,
    ) -> Self {
        Self {
            wording,
            formatter,
            selection,
            shortcuts: Vec::new(),
        }
    }

    pub(crate) fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub(crate) const fn wording(&self) -> DesktopWording {
        self.wording
    }

    pub(crate) fn shortcut(&self, action: &dyn Action) -> Option<SharedString> {
        self.shortcuts
            .iter()
            .find(|entry| entry.action.partial_eq(action))
            .map(|entry| entry.display.clone())
    }

    pub(crate) fn format(&self, shortcut: &Shortcut) -> SharedString {
        self.formatter
            .format_chord(shortcut.modifiers(), shortcut.key())
    }

    /// Presents a chord that need not be a valid [`Shortcut`], such as one a recorder refused.
    pub(crate) fn format_keystroke(&self, keystroke: &gpui::Keystroke) -> SharedString {
        self.formatter
            .format_chord(keystroke.modifiers, &keystroke.key.to_ascii_lowercase())
    }

    pub(crate) fn format_modifiers(&self, modifiers: Modifiers) -> SharedString {
        self.formatter.format_modifiers(modifiers)
    }

    pub(crate) fn refresh(&mut self, cx: &App) {
        self.refresh_keymap(&cx.key_bindings().borrow());
    }

    fn refresh_keymap(&mut self, keymap: &Keymap) {
        self.shortcuts.clear();
        for binding in keymap.bindings() {
            let action = binding.action();
            if self
                .shortcuts
                .iter()
                .any(|entry| entry.action.partial_eq(action))
            {
                continue;
            }
            let shortcut = match self.selection {
                ShortcutSelection::NativeMenu => installed_shortcut(keymap, action),
                ShortcutSelection::TerminalSurface => {
                    selected_shortcut(keymap, action, Some(crate::ui::TERMINAL_KEY_CONTEXT))
                }
            };
            if let Some(shortcut) = shortcut {
                self.shortcuts.push(PresentedShortcut {
                    action: action.boxed_clone(),
                    display: self.format(&shortcut),
                });
            }
        }
    }
}

/// Match GPUI's native menu selection among presentable bindings: first default-context
/// match, or first binding. A chord every desktop sends to the terminal is never presented.
pub(crate) fn installed_shortcut(keymap: &Keymap, action: &dyn Action) -> Option<Shortcut> {
    selected_shortcut(keymap, action, None)
}

/// The native menu selection with `surface` joining GPUI's default contexts.
fn selected_shortcut(
    keymap: &Keymap,
    action: &dyn Action,
    surface: Option<&str>,
) -> Option<Shortcut> {
    let mut context = KeyContext::new_with_defaults();
    for name in ["Workspace", "Pane", "Editor"].into_iter().chain(surface) {
        context.add(name);
    }
    let contexts = [context];
    let mut bindings = keymap.bindings_for_action(action).filter_map(|binding| {
        let [keystroke] = binding.keystrokes() else {
            return None;
        };
        let shortcut = Shortcut::from_keystroke(keystroke.inner()).ok()?;
        crate::keybindings::is_presentable(&shortcut).then_some((binding, shortcut))
    });
    let first = bindings.next()?;
    let matches = |binding: &gpui::KeyBinding| {
        binding
            .predicate()
            .is_none_or(|predicate| predicate.eval(&contexts))
    };
    let (_, shortcut) = if matches(first.0) {
        first
    } else {
        bindings.find(|(binding, _)| matches(binding)).unwrap_or(first)
    };
    Some(shortcut)
}
impl gpui::Global for DesktopPresentation {}

pub(crate) struct DesktopProfile {
    presentation: DesktopPresentation,
    modal_policy: ModalDesktopPolicy,
    control_keys: ControlKeybindingProfiles,
    keymap: KeymapProfile,
    locale: std::rc::Rc<dyn crate::platform::locale::LocaleDirection>,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlKeybindingProfiles {
    modal: ModalKeybindingProfile,
    menu: MenuKeybindingProfile,
    command_palette: CommandPaletteKeybindingProfile,
    combo_box: ComboBoxKeybindingProfile,
    text_input: TextInputKeybindingProfile,
}

impl ControlKeybindingProfiles {
    pub(crate) const fn new(
        modal: ModalKeybindingProfile,
        menu: MenuKeybindingProfile,
        command_palette: CommandPaletteKeybindingProfile,
        combo_box: ComboBoxKeybindingProfile,
        text_input: TextInputKeybindingProfile,
    ) -> Self {
        Self {
            modal,
            menu,
            command_palette,
            combo_box,
            text_input,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum DesktopProfileError {
    #[error("desktop policy and capabilities disagree")]
    InvalidCombination,
}
impl DesktopProfile {
    pub(crate) fn new(
        modal_policy: ModalDesktopPolicy,
        control_keys: ControlKeybindingProfiles,
        keymap: KeymapProfile,
        presentation: DesktopPresentation,
        locale: std::rc::Rc<dyn crate::platform::locale::LocaleDirection>,
    ) -> Self {
        Self {
            presentation,
            modal_policy,
            control_keys,
            keymap,
            locale,
        }
    }
    pub(crate) fn install(&self, cx: &mut App) -> KeymapProfile {
        cx.set_global(self.presentation.clone());
        spaceterm_ui::install_modal_policy(
            cx,
            self.modal_policy
                .with_text_direction(self.locale.text_direction()),
        );
        spaceterm_ui::install_menu_keybindings(cx, self.control_keys.menu);
        spaceterm_ui::install_command_palette_keybindings(cx, self.control_keys.command_palette);
        spaceterm_ui::install_portable_combo_box_keybindings(cx);
        spaceterm_ui::install_combo_box_keybindings(cx, self.control_keys.combo_box);
        spaceterm_ui::install_portable_modal_keybindings(cx);
        spaceterm_ui::install_modal_keybindings(cx, self.control_keys.modal);
        spaceterm_ui::install_text_input_keybindings(cx, self.control_keys.text_input);
        spaceterm_ui::install_text_area_keybindings(cx, self.control_keys.text_input);
        cx.bind_keys(
            self.keymap
                .resolve(&KeybindingPreferences::default())
                .key_bindings(),
        );
        cx.bind_keys(self.keymap.control_bindings().iter().cloned());
        cx.bind_keys(self.keymap.fixed_bindings().iter().cloned());
        self.keymap.clone()
    }
}

#[cfg(test)]
struct TestingShortcutFormatter;

#[cfg(test)]
impl ShortcutFormatter for TestingShortcutFormatter {
    fn format_chord(&self, modifiers: Modifiers, key: &str) -> SharedString {
        let key = match key {
            "enter" => "Enter".to_owned(),
            "space" => "Space".to_owned(),
            "tab" => "Tab".to_owned(),
            _ => key.to_uppercase(),
        };
        let modifiers = self.format_modifiers(modifiers);
        if modifiers.is_empty() {
            key.into()
        } else {
            format!("{modifiers}+{key}").into()
        }
    }

    fn format_modifiers(&self, modifiers: Modifiers) -> SharedString {
        [
            (modifiers.platform, "Primary"),
            (modifiers.control, "Ctrl"),
            (modifiers.alt, "Alt"),
            (modifiers.shift, "Shift"),
        ]
        .into_iter()
        .filter_map(|(enabled, label)| enabled.then_some(label))
        .collect::<Vec<_>>()
        .join("+")
        .into()
    }
}

/// A System Reserved table for tests, standing in for a host's.
#[cfg(test)]
fn testing_reserved_shortcuts() -> Vec<crate::keybindings::SystemReserved> {
    vec![crate::keybindings::SystemReserved {
        shortcut: Shortcut::parse("cmd-q").expect("static reserved shortcut"),
        reason: crate::keybindings::SystemReservation::Quit,
    }]
}

#[cfg(test)]
pub(crate) fn testing_presentation() -> DesktopPresentation {
    let mut presentation = DesktopPresentation::new(
        DesktopWording {
            file_preview: "Preview File",
            operating_system_name: "Operating System",
            system_directory_selection: "Choose Directory…",
        },
        Rc::new(TestingShortcutFormatter),
        ShortcutSelection::NativeMenu,
    );
    let profile = default_keymap::profile(
        crate::platform::keyboard_layout::testing::us(),
        testing_reserved_shortcuts(),
    )
    .unwrap();
    let keymap = Keymap::new(
        profile
            .resolve(&KeybindingPreferences::default())
            .key_bindings()
            .into_iter()
            .chain(profile.control_bindings().iter().cloned())
            .chain(profile.fixed_bindings().iter().cloned())
            .collect(),
    );
    presentation.refresh_keymap(&keymap);
    presentation
}

#[cfg(test)]
pub(crate) fn testing_profile(direction: spaceterm_ui::TextDirection) -> DesktopProfile {
    DesktopProfile::new(
        ModalDesktopPolicy::mac_os(),
        ControlKeybindingProfiles::new(
            ModalKeybindingProfile::MacOs,
            MenuKeybindingProfile::MacOs,
            CommandPaletteKeybindingProfile::MacOs,
            ComboBoxKeybindingProfile::MacOs,
            TextInputKeybindingProfile::MacOs,
        ),
        default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            testing_reserved_shortcuts(),
        )
        .unwrap(),
        testing_presentation(),
        std::rc::Rc::new(crate::platform::locale::FixedLocaleDirection(direction)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{NewWorkspace, SwitchWorkspace};
    use gpui::KeyBinding;

    #[gpui::test]
    fn complete_profile_installs_the_expected_bindings(cx: &mut gpui::TestAppContext) {
        let actual = cx.update(|cx| {
            crate::ui::init(cx).unwrap();
            let menu = std::rc::Rc::new(crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default());
            crate::app::init(
                cx,
                menu.clone(),
                std::rc::Rc::new(
                    crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
                ),
            )
            .unwrap();
            assert_eq!(menu.installs(), 1);
            let bindings = cx.key_bindings();
            let keymap = bindings.borrow();
            let tagged = keymap.bindings().enumerate().filter_map(|(index, binding)| {
                (binding.meta() == Some(crate::keybindings::CUSTOMIZABLE_BINDINGS)).then_some(index)
            }).collect::<Vec<_>>();
            assert_eq!(tagged.len(), 48);
            assert!(tagged.windows(2).all(|pair| pair[1] == pair[0] + 1));
            assert_eq!(DesktopPresentation::get(cx).shortcut(&crate::ui::IncreaseTerminalFontSize).as_deref(), Some("Primary+="));
            // Settings keeps its fixed Shortcut outside the customizable segment.
            assert_eq!(DesktopPresentation::get(cx).shortcut(&crate::ui::settings_window::OpenSettings).as_deref(), Some("Primary+,"));
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
        // The baseline spells the platform modifier as "cmd"; GPUI spells it per host.
        let expected = include_str!("keybindings_baseline.txt")
            .lines()
            .map(|line| {
                let (keys, rest) = line.split_once('\t').unwrap();
                let keys = keys
                    .split(' ')
                    .map(|key| gpui::Keystroke::parse(key).unwrap().unparse())
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{keys}\t{rest}")
            })
            .collect::<Vec<_>>();
        #[cfg(feature = "appearance-exerciser")]
        let expected = {
            use gpui::Action as _;
            let mut expected = expected;
            expected.extend([
                format!(
                    "{}\tNone\t{}",
                    gpui::Keystroke::parse("alt-cmd-a").unwrap().unparse(),
                    crate::ui::appearance_exerciser::ShowAppearanceExerciser.name()
                ),
                format!(
                    "{}\tNone\t{}",
                    gpui::Keystroke::parse("alt-cmd-c").unwrap().unparse(),
                    crate::ui::appearance_exerciser::ToggleAppearancePreview.name()
                ),
            ]);
            expected
        };
        assert_eq!(actual, expected);
    }

    #[gpui::test]
    fn terminal_surface_selection_presents_the_terminal_binding_over_text_field_chords(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("ctrl-insert", NewWorkspace, Some("SpaceTermTextInput")),
                KeyBinding::new("ctrl-shift-y", NewWorkspace, Some(crate::ui::TERMINAL_KEY_CONTEXT)),
            ]);
            let mut native = testing_presentation();
            native.refresh(cx);
            assert_eq!(native.shortcut(&NewWorkspace).as_deref(), Some("Ctrl+INSERT"));
            let mut terminal = DesktopPresentation::new(
                native.wording(),
                Rc::new(TestingShortcutFormatter),
                ShortcutSelection::TerminalSurface,
            );
            terminal.refresh(cx);
            assert_eq!(terminal.shortcut(&NewWorkspace).as_deref(), Some("Ctrl+Shift+Y"));
        });
    }

    #[gpui::test]
    fn presentation_refresh_follows_installed_bindings_and_clears_removed_hints(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let mut presentation = testing_presentation();
            assert_eq!(
                presentation
                    .format_modifiers(gpui::Modifiers {
                        platform: true,
                        shift: true,
                        ..Default::default()
                    })
                    .as_ref(),
                "Primary+Shift"
            );
            assert_eq!(
                presentation.wording().operating_system_name,
                "Operating System"
            );
            cx.bind_keys([
                KeyBinding::new("cmd-y", NewWorkspace, Some(crate::ui::TERMINAL_KEY_CONTEXT)),
                KeyBinding::new("cmd-u", NewWorkspace, None),
                KeyBinding::new("cmd-i", NewWorkspace, None),
            ]);
            presentation.refresh(cx);
            assert_eq!(
                presentation.shortcut(&NewWorkspace).as_deref(),
                Some("Primary+U")
            );
            cx.clear_key_bindings();
            cx.bind_keys([
                KeyBinding::new("cmd-y", NewWorkspace, Some(crate::ui::TERMINAL_KEY_CONTEXT)),
                KeyBinding::new("cmd-u", NewWorkspace, Some(crate::ui::TERMINAL_KEY_CONTEXT)),
            ]);
            presentation.refresh(cx);
            assert_eq!(
                presentation.shortcut(&NewWorkspace).as_deref(),
                Some("Primary+Y")
            );
            assert_eq!(presentation.shortcut(&SwitchWorkspace), None);
            cx.clear_key_bindings();
            presentation.refresh(cx);
            assert_eq!(presentation.shortcut(&NewWorkspace), None);
        });
    }
}
