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
    FocusPaneRight, FocusPaneUp, IncreaseTerminalFontSize, NewWorkspace, OpenTerminalFind,
    ResetTerminalFontSize, SplitDown, SplitRight, SwitchWorkspace, TogglePaneZoom, ToggleSidebar,
    ToggleSidebarFocus,
};

pub(crate) const TOGGLE_PANE_ZOOM_TITLE: &str = "Toggle Pane Zoom";
pub(crate) const ABOUT_DESCRIPTION: &str = "A native, keyboard-first desktop terminal multiplexer.";
pub(crate) const HELP_URL: &str = "https://github.com/sadiksaifi/SpaceTerm";

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

#[cfg(test)]
mod tests {
    use super::*;

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
