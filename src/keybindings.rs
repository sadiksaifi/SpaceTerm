//! Portable shortcut policy and resolution of host defaults with retained overrides.

mod keymap;
mod preferences;
pub(crate) mod runtime;
mod shortcut;
mod terminal_conventions;

pub use keymap::*;
pub use preferences::KeybindingPreferences;
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "exported for the later Settings Document validation step"
    )
)]
pub use preferences::KeybindingPreferencesError;
pub use shortcut::{Shortcut, ShortcutRejection};
pub(crate) use terminal_conventions::is_presentable;
pub use terminal_conventions::{TerminalConvention, TerminalConventions};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CommandGroup {
    Workspace,
    Tab,
    Pane,
    Terminal,
    View,
    Help,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyScope {
    Application,
    Workspace,
}

impl KeyScope {
    pub const fn key_context(self) -> Option<&'static str> {
        match self {
            Self::Application => None,
            Self::Workspace => Some(crate::ui::TERMINAL_KEY_CONTEXT),
        }
    }
}

macro_rules! commands {
    ($($command:ident => ($id:literal, $label:literal, $group:ident, $scope:ident, $action:path)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub enum Command { $($command),+ }

        impl Command {
            pub const ALL: [Self; 55] = [$(Self::$command),+];
            pub const fn id(self) -> &'static str {
                match self { $(Self::$command => $id),+ }
            }
            pub fn from_id(id: &str) -> Option<Self> {
                match id { $($id => Some(Self::$command),)+ _ => None }
            }
            pub const fn label(self) -> &'static str {
                match self { $(Self::$command => $label),+ }
            }
            pub const fn group(self) -> CommandGroup {
                match self { $(Self::$command => CommandGroup::$group),+ }
            }
            pub const fn scope(self) -> KeyScope {
                match self { $(Self::$command => KeyScope::$scope),+ }
            }
            pub fn action(self) -> Box<dyn gpui::Action> {
                match self { $(Self::$command => Box::new($action)),+ }
            }
        }
    };
}

commands! {
    SwitchWorkspace => ("switch_workspace", "Switch Workspace", Workspace, Application, crate::ui::SwitchWorkspace),
    NewWorkspace => ("new_workspace", "New Workspace", Workspace, Application, crate::ui::NewWorkspace),
    NewRemoteWorkspace => ("new_remote_workspace", "New Remote Workspace", Workspace, Application, crate::ui::NewRemoteWorkspace),
    OpenLocalDirectory => ("open_local_directory", "Open Local Directory", Workspace, Application, crate::ui::OpenLocalDirectory),
    OpenRemoteDirectory => ("open_remote_directory", "Open Remote Directory", Workspace, Application, crate::ui::OpenRemoteDirectory),
    CloseWorkspace => ("close_workspace", "Close Workspace", Workspace, Application, crate::ui::CloseWorkspace),
    ActivateWorkspace1 => ("activate_workspace1", "Workspace 1", Workspace, Workspace, crate::ui::ActivateWorkspace1),
    ActivateWorkspace2 => ("activate_workspace2", "Workspace 2", Workspace, Workspace, crate::ui::ActivateWorkspace2),
    ActivateWorkspace3 => ("activate_workspace3", "Workspace 3", Workspace, Workspace, crate::ui::ActivateWorkspace3),
    ActivateWorkspace4 => ("activate_workspace4", "Workspace 4", Workspace, Workspace, crate::ui::ActivateWorkspace4),
    ActivateWorkspace5 => ("activate_workspace5", "Workspace 5", Workspace, Workspace, crate::ui::ActivateWorkspace5),
    ActivateWorkspace6 => ("activate_workspace6", "Workspace 6", Workspace, Workspace, crate::ui::ActivateWorkspace6),
    ActivateWorkspace7 => ("activate_workspace7", "Workspace 7", Workspace, Workspace, crate::ui::ActivateWorkspace7),
    ActivateWorkspace8 => ("activate_workspace8", "Workspace 8", Workspace, Workspace, crate::ui::ActivateWorkspace8),
    ActivateWorkspace9 => ("activate_workspace9", "Workspace 9", Workspace, Workspace, crate::ui::ActivateWorkspace9),
    CreateTab => ("create_tab", "New Tab", Tab, Application, crate::ui::CreateTab),
    CloseTab => ("close_tab", "Close Tab", Tab, Application, crate::ui::CloseTab),
    ActivateTab1 => ("activate_tab1", "Tab 1", Tab, Workspace, crate::ui::ActivateTab1),
    ActivateTab2 => ("activate_tab2", "Tab 2", Tab, Workspace, crate::ui::ActivateTab2),
    ActivateTab3 => ("activate_tab3", "Tab 3", Tab, Workspace, crate::ui::ActivateTab3),
    ActivateTab4 => ("activate_tab4", "Tab 4", Tab, Workspace, crate::ui::ActivateTab4),
    ActivateTab5 => ("activate_tab5", "Tab 5", Tab, Workspace, crate::ui::ActivateTab5),
    ActivateTab6 => ("activate_tab6", "Tab 6", Tab, Workspace, crate::ui::ActivateTab6),
    ActivateTab7 => ("activate_tab7", "Tab 7", Tab, Workspace, crate::ui::ActivateTab7),
    ActivateTab8 => ("activate_tab8", "Tab 8", Tab, Workspace, crate::ui::ActivateTab8),
    ActivateTab9 => ("activate_tab9", "Tab 9", Tab, Workspace, crate::ui::ActivateTab9),
    NextTab => ("next_tab", "Next Tab", Tab, Workspace, crate::ui::NextTab),
    PreviousTab => ("previous_tab", "Previous Tab", Tab, Workspace, crate::ui::PreviousTab),
    MoveTabRight => ("move_tab_right", "Move Tab Right", Tab, Workspace, crate::ui::MoveTabRight),
    MoveTabLeft => ("move_tab_left", "Move Tab Left", Tab, Workspace, crate::ui::MoveTabLeft),
    ClosePane => ("close_pane", "Close Pane", Pane, Application, crate::ui::ClosePane),
    SplitRight => ("split_right", "Split Right", Pane, Workspace, crate::ui::SplitRight),
    SplitDown => ("split_down", "Split Down", Pane, Workspace, crate::ui::SplitDown),
    FocusPaneLeft => ("focus_pane_left", "Focus Pane Left", Pane, Workspace, crate::ui::FocusPaneLeft),
    FocusPaneRight => ("focus_pane_right", "Focus Pane Right", Pane, Workspace, crate::ui::FocusPaneRight),
    FocusPaneUp => ("focus_pane_up", "Focus Pane Up", Pane, Workspace, crate::ui::FocusPaneUp),
    FocusPaneDown => ("focus_pane_down", "Focus Pane Down", Pane, Workspace, crate::ui::FocusPaneDown),
    FocusPreviousPane => ("focus_previous_pane", "Focus Previous Pane", Pane, Workspace, crate::ui::FocusPreviousPane),
    FocusNextPane => ("focus_next_pane", "Focus Next Pane", Pane, Workspace, crate::ui::FocusNextPane),
    TogglePaneZoom => ("toggle_pane_zoom", "Toggle Pane Zoom", Pane, Workspace, crate::ui::TogglePaneZoom),
    OpenTerminalFind => ("open_terminal_find", "Find", Terminal, Workspace, crate::ui::OpenTerminalFind),
    FindNext => ("find_next", "Find Next", Terminal, Workspace, crate::ui::FindNext),
    FindPrevious => ("find_previous", "Find Previous", Terminal, Workspace, crate::ui::FindPrevious),
    ClearTerminalScreenAndScrollback => ("clear_terminal_screen_and_scrollback", "Clear Screen and Scrollback", Terminal, Workspace, crate::ui::ClearTerminalScreenAndScrollback),
    ScrollPageUp => ("scroll_page_up", "Scroll Page Up", Terminal, Workspace, crate::ui::ScrollPageUp),
    ScrollPageDown => ("scroll_page_down", "Scroll Page Down", Terminal, Workspace, crate::ui::ScrollPageDown),
    ScrollToTop => ("scroll_to_top", "Scroll to Top", Terminal, Workspace, crate::ui::ScrollToTop),
    ScrollToBottom => ("scroll_to_bottom", "Scroll to Bottom", Terminal, Workspace, crate::ui::ScrollToBottom),
    IncreaseTerminalFontSize => ("increase_terminal_font_size", "Increase Font Size", View, Workspace, crate::ui::IncreaseTerminalFontSize),
    DecreaseTerminalFontSize => ("decrease_terminal_font_size", "Decrease Font Size", View, Workspace, crate::ui::DecreaseTerminalFontSize),
    ResetTerminalFontSize => ("reset_terminal_font_size", "Reset Font Size", View, Workspace, crate::ui::ResetTerminalFontSize),
    ToggleSidebar => ("toggle_sidebar", "Toggle Sidebar", View, Workspace, crate::ui::ToggleSidebar),
    ToggleSidebarFocus => ("toggle_sidebar_focus", "Toggle Sidebar Focus", View, Workspace, crate::ui::ToggleSidebarFocus),
    KeyboardShortcuts => ("keyboard_shortcuts", "Keyboard Shortcuts", View, Application, crate::ui::settings_window::OpenKeyboardShortcuts),
    About => ("about", "About SpaceTerm", Help, Application, crate::app::ShowAboutApplication),
}

impl Command {
    pub const fn mirrors_into_find_field(self) -> bool {
        matches!(
            self,
            Self::OpenTerminalFind | Self::FindNext | Self::FindPrevious
        )
    }

    pub const fn key_context(self) -> Option<&'static str> {
        self.scope().key_context()
    }
}

impl Serialize for Command {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_id(&String::deserialize(deserializer)?)
            .ok_or_else(|| serde::de::Error::custom("unknown keybinding command"))
    }
}

#[cfg(test)]
mod tests;
