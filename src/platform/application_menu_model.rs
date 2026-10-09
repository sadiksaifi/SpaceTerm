//! Native macOS application menu contents and grouping.
use gpui::{Menu, MenuItem, SystemMenuType};
use spaceterm_ui::{EditCopy, EditCut, EditPaste, EditRedo, EditSelectAll, EditUndo};

use crate::app::{
    BringAllWindowsToFront, HideApplication, HideOtherApplications, MinimizeWindow,
    OpenApplicationHelp, QuitApplication, ShowAboutApplication, ShowAllApplications,
    ZoomActiveWindow,
};
use crate::ui::updates::{
    CHECK_FOR_UPDATES_TITLE, CheckForUpdates, OpenReleaseNotes, RELEASE_NOTES_TITLE,
};
use crate::ui::{
    ClosePane, CloseTab, CloseWorkspace, CreateTab, DecreaseTerminalFontSize,
    ExportTerminalDiagnostics, FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft,
    FocusPaneRight, FocusPaneUp, IncreaseTerminalFontSize, MoveTabLeft, MoveTabRight, NewWorkspace,
    NextTab, NextWorktree, OpenTerminalFind, PreviousTab, PreviousWorktree, ResetTerminalFontSize,
    ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop, SplitDown, SplitRight,
    SwitchWorkspace, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
};

pub(crate) const TOGGLE_PANE_ZOOM_TITLE: &str = "Toggle Pane Zoom";

pub(crate) fn menus(application_name: &str) -> Vec<Menu> {
    let mut menus = vec![
        application_menu(application_name),
        file_menu(),
        edit_menu(),
        view_menu(),
    ];
    #[cfg(feature = "developer-tools")]
    menus.push(develop_menu());
    menus.extend([window_menu(), help_menu()]);
    menus
}

pub(crate) fn application_menu(application_name: &str) -> Menu {
    Menu {
        disabled: false,
        name: application_name.to_owned().into(),
        items: vec![
            MenuItem::action(format!("About {application_name}"), ShowAboutApplication),
            MenuItem::action(CHECK_FOR_UPDATES_TITLE, CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", crate::ui::settings_window::OpenSettings),
            MenuItem::action(
                "Keyboard Shortcuts…",
                crate::ui::settings_window::OpenKeyboardShortcuts,
            ),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {application_name}"), HideApplication),
            MenuItem::action("Hide Others", HideOtherApplications),
            MenuItem::action("Show All", ShowAllApplications),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {application_name}"), QuitApplication),
        ],
    }
}

pub(crate) fn file_menu() -> Menu {
    Menu {
        disabled: false,
        name: "File".into(),
        items: vec![
            MenuItem::action("New Workspace", NewWorkspace),
            MenuItem::action("New Remote Workspace", crate::ui::NewRemoteWorkspace),
            MenuItem::action("Open Local Directory…", crate::ui::OpenLocalDirectory),
            MenuItem::action("Open Remote Directory…", crate::ui::OpenRemoteDirectory),
            MenuItem::action("Switch Workspace", SwitchWorkspace),
            MenuItem::separator(),
            MenuItem::action("New Tab", CreateTab),
            MenuItem::action("New Worktree…", crate::ui::NewWorktree),
            MenuItem::separator(),
            MenuItem::action("Close Pane", ClosePane),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Workspace", CloseWorkspace),
            MenuItem::separator(),
            MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
        ],
    }
}

fn edit_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Edit".into(),
        items: vec![
            MenuItem::action("Undo", EditUndo),
            MenuItem::action("Redo", EditRedo),
            MenuItem::separator(),
            MenuItem::action("Cut", EditCut),
            MenuItem::action("Copy", EditCopy),
            MenuItem::action("Paste", EditPaste),
            MenuItem::action("Select All", EditSelectAll),
            MenuItem::separator(),
            MenuItem::submenu(Menu {
                disabled: false,
                name: "Find".into(),
                items: vec![
                    MenuItem::action("Find…", OpenTerminalFind),
                    MenuItem::action("Find Next", FindNext),
                    MenuItem::action("Find Previous", FindPrevious),
                ],
            }),
        ],
    }
}

pub(crate) fn view_menu() -> Menu {
    Menu {
        disabled: false,
        name: "View".into(),
        items: vec![
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Toggle Sidebar Focus", ToggleSidebarFocus),
            MenuItem::separator(),
            MenuItem::action("Increase Terminal Font Size", IncreaseTerminalFontSize),
            MenuItem::action("Decrease Terminal Font Size", DecreaseTerminalFontSize),
            MenuItem::action("Reset Terminal Font Size", ResetTerminalFontSize),
            MenuItem::separator(),
            MenuItem::action("Scroll to Top", ScrollToTop),
            MenuItem::action("Scroll to Bottom", ScrollToBottom),
            MenuItem::action("Scroll Page Up", ScrollPageUp),
            MenuItem::action("Scroll Page Down", ScrollPageDown),
            MenuItem::separator(),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::submenu(Menu {
                disabled: false,
                name: "Focus Pane".into(),
                items: vec![
                    MenuItem::action("Left", FocusPaneLeft),
                    MenuItem::action("Right", FocusPaneRight),
                    MenuItem::action("Up", FocusPaneUp),
                    MenuItem::action("Down", FocusPaneDown),
                ],
            }),
            MenuItem::action(TOGGLE_PANE_ZOOM_TITLE, TogglePaneZoom),
        ],
    }
}

#[cfg(feature = "developer-tools")]
fn develop_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Develop".into(),
        items: vec![
            MenuItem::action(
                "Developer Workbench",
                crate::ui::developer_workbench::OpenDeveloperWorkbench,
            ),
            MenuItem::action(
                "Toggle Appearance",
                crate::ui::developer_workbench::ToggleAppearancePreview,
            ),
        ],
    }
}

pub(crate) fn window_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Window".into(),
        items: vec![
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomActiveWindow),
            MenuItem::separator(),
            MenuItem::action("Previous Tab", PreviousTab),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Move Tab Left", MoveTabLeft),
            MenuItem::action("Move Tab Right", MoveTabRight),
            MenuItem::separator(),
            MenuItem::action("Previous Worktree", PreviousWorktree),
            MenuItem::action("Next Worktree", NextWorktree),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllWindowsToFront),
        ],
    }
}

pub(crate) fn help_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Help".into(),
        items: vec![
            MenuItem::action("SpaceTerm Help", OpenApplicationHelp),
            MenuItem::separator(),
            MenuItem::action(RELEASE_NOTES_TITLE, OpenReleaseNotes),
            MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
        ],
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Action, Keymap};

    use super::*;
    use crate::keybindings::{Command, KeybindingPreferences, Shortcut};

    const MENU_COMMANDS: [(&str, &str, Command); 11] = [
        (
            "SpaceTerm",
            "Keyboard Shortcuts…",
            Command::KeyboardShortcuts,
        ),
        ("View", "Scroll to Top", Command::ScrollToTop),
        ("View", "Scroll to Bottom", Command::ScrollToBottom),
        ("View", "Scroll Page Up", Command::ScrollPageUp),
        ("View", "Scroll Page Down", Command::ScrollPageDown),
        ("Window", "Previous Tab", Command::PreviousTab),
        ("Window", "Next Tab", Command::NextTab),
        ("Window", "Move Tab Left", Command::MoveTabLeft),
        ("Window", "Move Tab Right", Command::MoveTabRight),
        ("Window", "Previous Worktree", Command::PreviousWorktree),
        ("Window", "Next Worktree", Command::NextWorktree),
    ];

    fn menu_action<'a>(menus: &'a [Menu], menu: &str, title: &str) -> &'a dyn Action {
        menus
            .iter()
            .filter(|candidate| candidate.name.as_ref() == menu)
            .flat_map(|menu| &menu.items)
            .find_map(|item| match item {
                MenuItem::Action { name, action, .. } if name.as_ref() == title => {
                    Some(action.as_ref())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {menu} > {title}"))
    }

    fn labels(menu: &Menu) -> Vec<&str> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Action { name, .. } => name.as_ref(),
                MenuItem::Separator => "|",
                MenuItem::Submenu(submenu) => submenu.name.as_ref(),
                MenuItem::SystemMenu(menu) => menu.name.as_ref(),
            })
            .collect()
    }

    #[test]
    fn tab_scroll_and_keyboard_shortcuts_commands_dispatch_from_their_menus() {
        let menus = menus("SpaceTerm");
        for (menu, title, command) in MENU_COMMANDS {
            assert!(
                command
                    .action()
                    .partial_eq(menu_action(&menus, menu, title)),
                "{menu} > {title} should dispatch {command:?}"
            );
        }
    }

    #[test]
    fn tab_and_scroll_commands_form_their_own_menu_groups() {
        assert_eq!(
            labels(&application_menu("SpaceTerm"))[..5],
            [
                "About SpaceTerm",
                "Check for Updates…",
                "|",
                "Settings…",
                "Keyboard Shortcuts…"
            ]
        );
        assert_eq!(
            labels(&view_menu())[6..12],
            [
                "|",
                "Scroll to Top",
                "Scroll to Bottom",
                "Scroll Page Up",
                "Scroll Page Down",
                "|"
            ]
        );
        assert_eq!(
            labels(&window_menu()),
            [
                "Minimize",
                "Zoom",
                "|",
                "Previous Tab",
                "Next Tab",
                "Move Tab Left",
                "Move Tab Right",
                "|",
                "Previous Worktree",
                "Next Worktree",
                "|",
                "Bring All to Front"
            ]
        );
    }

    fn menu_shortcuts(preferences: &str) -> Vec<(&'static str, Option<Shortcut>)> {
        let profile = crate::desktop_profile::default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            Vec::new(),
        )
        .unwrap();
        let preferences: KeybindingPreferences = serde_json::from_str(preferences).unwrap();
        let keymap = Keymap::new(profile.resolve(&preferences).key_bindings());
        let menus = menus("SpaceTerm");
        MENU_COMMANDS
            .into_iter()
            .map(|(menu, title, _)| {
                (
                    title,
                    crate::desktop_profile::installed_shortcut(
                        &keymap,
                        menu_action(&menus, menu, title),
                    ),
                )
            })
            .collect()
    }

    fn shortcuts(
        expected: [(&'static str, Option<&str>); 11],
    ) -> Vec<(&'static str, Option<Shortcut>)> {
        expected
            .into_iter()
            .map(|(title, shortcut)| (title, shortcut.map(|s| Shortcut::parse(s).unwrap())))
            .collect()
    }

    #[test]
    fn tab_scroll_and_keyboard_shortcuts_items_show_their_default_shortcuts() {
        assert_eq!(
            menu_shortcuts("{}"),
            shortcuts([
                ("Keyboard Shortcuts…", None),
                ("Scroll to Top", Some("cmd-home")),
                ("Scroll to Bottom", Some("cmd-end")),
                ("Scroll Page Up", Some("cmd-pageup")),
                ("Scroll Page Down", Some("cmd-pagedown")),
                ("Previous Tab", Some("cmd-{")),
                ("Next Tab", Some("cmd-}")),
                ("Move Tab Left", None),
                ("Move Tab Right", None),
                ("Previous Worktree", Some("cmd-alt-[")),
                ("Next Worktree", Some("cmd-alt-]")),
            ])
        );
    }

    #[test]
    fn tab_scroll_and_keyboard_shortcuts_items_follow_rebound_and_unbound_shortcuts() {
        assert_eq!(
            menu_shortcuts(
                r#"{"keyboard_shortcuts":"cmd-alt-k","scroll_to_top":null,
                    "scroll_page_down":"cmd-alt-pagedown","previous_tab":null,
                    "next_tab":"ctrl-tab","move_tab_right":"cmd-alt-shift-right"}"#
            ),
            shortcuts([
                ("Keyboard Shortcuts…", Some("cmd-alt-k")),
                ("Scroll to Top", None),
                ("Scroll to Bottom", Some("cmd-end")),
                ("Scroll Page Up", Some("cmd-pageup")),
                ("Scroll Page Down", Some("cmd-alt-pagedown")),
                ("Previous Tab", None),
                ("Next Tab", Some("ctrl-tab")),
                ("Move Tab Left", None),
                ("Move Tab Right", Some("cmd-alt-shift-right")),
                ("Previous Worktree", Some("cmd-alt-[")),
                ("Next Worktree", Some("cmd-alt-]")),
            ])
        );
    }

    #[test]
    fn shared_menus_preserve_the_development_actions_and_order() {
        let menus = menus("SpaceTerm Development");
        let names = menus
            .iter()
            .map(|menu| menu.name.as_ref())
            .collect::<Vec<_>>();
        let expected: &[&str] = if cfg!(feature = "developer-tools") {
            &[
                "SpaceTerm Development",
                "File",
                "Edit",
                "View",
                "Develop",
                "Window",
                "Help",
            ]
        } else {
            &[
                "SpaceTerm Development",
                "File",
                "Edit",
                "View",
                "Window",
                "Help",
            ]
        };
        assert_eq!(names, expected);
        #[cfg(feature = "developer-tools")]
        {
            let develop = &menus[4];
            assert!(matches!(&develop.items[0], MenuItem::Action { action, .. }
                if action.as_any().is::<crate::ui::developer_workbench::OpenDeveloperWorkbench>()));
            assert!(matches!(&develop.items[1], MenuItem::Action { action, .. }
                if action.as_any().is::<crate::ui::developer_workbench::ToggleAppearancePreview>()));
        }
    }
}
