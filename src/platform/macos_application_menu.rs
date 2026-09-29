use gpui::{App, Keymap, Menu, MenuItem, Modifiers, SharedString, SystemMenuType};
use spaceterm_ui::{EditCopy, EditCut, EditPaste, EditRedo, EditSelectAll, EditUndo};

use super::application_menu::{
    ApplicationMenuAdapter, ApplicationMenuCommand, ApplicationMenuError,
};
use crate::app::{
    BringAllWindowsToFront, HideApplication, HideOtherApplications, MinimizeWindow,
    OpenApplicationHelp, QuitApplication, ShowAboutApplication, ShowAllApplications,
    ZoomActiveWindow,
};
use crate::application_identity::ApplicationIdentity;
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

pub(crate) struct MacosApplicationMenuAdapter {
    identity: ApplicationIdentity,
}

impl MacosApplicationMenuAdapter {
    pub(crate) const fn new(identity: ApplicationIdentity) -> Self {
        Self { identity }
    }
}

const TOGGLE_PANE_ZOOM_TITLE: &str = "Toggle Pane Zoom";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuItemIcon<'a> {
    menu: &'a str,
    submenu: Option<&'a str>,
    item: &'a str,
    symbol: &'a str,
}

const MENU_ITEM_ICONS: &[MenuItemIcon] = &[
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "New Workspace",
        symbol: "folder.badge.plus",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "New Remote Workspace",
        symbol: "globe",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Switch Workspace",
        symbol: "magnifyingglass",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "New Tab",
        symbol: "plus.square",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Close Pane",
        symbol: "xmark.square",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Close Tab",
        symbol: "xmark",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Close Workspace",
        symbol: "rectangle.portrait.and.arrow.right",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Export Terminal Diagnostics…",
        symbol: "doc.text.magnifyingglass",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Undo",
        symbol: "arrow.uturn.backward",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Redo",
        symbol: "arrow.uturn.forward",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Cut",
        symbol: "scissors",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Copy",
        symbol: "doc.on.doc",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Paste",
        symbol: "doc.on.clipboard",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Select All",
        symbol: "selection.pin.in.out",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: None,
        item: "Find",
        symbol: "magnifyingglass",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: Some("Find"),
        item: "Find…",
        symbol: "magnifyingglass",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: Some("Find"),
        item: "Find Next",
        symbol: "chevron.down",
    },
    MenuItemIcon {
        menu: "Edit",
        submenu: Some("Find"),
        item: "Find Previous",
        symbol: "chevron.up",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Toggle Sidebar",
        symbol: "sidebar.left",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Toggle Sidebar Focus",
        symbol: "sidebar.leading",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Increase Terminal Font Size",
        symbol: "textformat.size.larger",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Decrease Terminal Font Size",
        symbol: "textformat.size.smaller",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Reset Terminal Font Size",
        symbol: "textformat.size",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Split Right",
        symbol: "rectangle.split.2x1",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Split Down",
        symbol: "rectangle.split.1x2",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: "Focus Pane",
        symbol: "arrow.up.and.down.and.arrow.left.and.right",
    },
    MenuItemIcon {
        menu: "View",
        submenu: Some("Focus Pane"),
        item: "Left",
        symbol: "arrow.left",
    },
    MenuItemIcon {
        menu: "View",
        submenu: Some("Focus Pane"),
        item: "Right",
        symbol: "arrow.right",
    },
    MenuItemIcon {
        menu: "View",
        submenu: Some("Focus Pane"),
        item: "Up",
        symbol: "arrow.up",
    },
    MenuItemIcon {
        menu: "View",
        submenu: Some("Focus Pane"),
        item: "Down",
        symbol: "arrow.down",
    },
    MenuItemIcon {
        menu: "View",
        submenu: None,
        item: TOGGLE_PANE_ZOOM_TITLE,
        symbol: "arrow.up.left.and.arrow.down.right",
    },
    MenuItemIcon {
        menu: "Window",
        submenu: None,
        item: "Minimize",
        symbol: "minus.square",
    },
    MenuItemIcon {
        menu: "Window",
        submenu: None,
        item: "Zoom",
        symbol: "arrow.up.left.and.arrow.down.right",
    },
    MenuItemIcon {
        menu: "Window",
        submenu: None,
        item: "Bring All to Front",
        symbol: "macwindow.on.rectangle",
    },
    MenuItemIcon {
        menu: "Help",
        submenu: None,
        item: "SpaceTerm Help",
        symbol: "questionmark.circle",
    },
    MenuItemIcon {
        menu: "Help",
        submenu: None,
        item: RELEASE_NOTES_TITLE,
        symbol: "doc.text",
    },
    MenuItemIcon {
        menu: "Help",
        submenu: None,
        item: "Export Terminal Diagnostics…",
        symbol: "doc.text.magnifyingglass",
    },
];

impl ApplicationMenuAdapter for MacosApplicationMenuAdapter {
    fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
        let application_name = self.identity.display_name();
        let menus = menus(application_name);
        let native_keys = native_key_equivalents(&menus, &cx.key_bindings().borrow());
        cx.set_menus(menus);
        native::decorate(application_name, &native_keys)
    }

    fn perform(&self, command: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
        native::perform(command, self.identity.display_name())
    }
}

fn menus(application_name: &str) -> Vec<Menu> {
    vec![
        application_menu(application_name),
        file_menu(),
        edit_menu(),
        view_menu(),
        window_menu(),
        help_menu(),
    ]
}

fn application_menu(application_name: &str) -> Menu {
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

fn file_menu() -> Menu {
    Menu {
        disabled: false,
        name: "File".into(),
        items: vec![
            MenuItem::action("New Workspace", NewWorkspace),
            MenuItem::action("New Remote Workspace", crate::ui::NewRemoteWorkspace),
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

fn view_menu() -> Menu {
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

fn window_menu() -> Menu {
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

fn help_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Help".into(),
        items: vec![
            MenuItem::action("SpaceTerm Help", OpenApplicationHelp),
            MenuItem::action(RELEASE_NOTES_TITLE, OpenReleaseNotes),
            MenuItem::separator(),
            MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
        ],
    }
}

fn application_menu_item_icons(application_name: &str) -> [(String, &'static str); 7] {
    [
        (format!("About {application_name}"), "info.circle"),
        (CHECK_FOR_UPDATES_TITLE.to_owned(), "arrow.down.circle"),
        ("Settings…".to_owned(), "gearshape"),
        (format!("Hide {application_name}"), "eye.slash"),
        ("Hide Others".to_owned(), "eye.slash.fill"),
        ("Show All".to_owned(), "eye"),
        (format!("Quit {application_name}"), "power"),
    ]
}

#[derive(Debug, Eq, PartialEq)]
struct NativeKeyEquivalent {
    path: Vec<SharedString>,
    modifiers: Modifiers,
    key: &'static str,
}

fn missing_native_key_equivalent(key: &str) -> Option<&'static str> {
    match key {
        "enter" => Some("\r"),
        "tab" => Some("\t"),
        _ => None,
    }
}

fn native_key_equivalents(menus: &[Menu], keymap: &Keymap) -> Vec<NativeKeyEquivalent> {
    fn collect(
        menu: &Menu,
        path: &mut Vec<SharedString>,
        keymap: &Keymap,
        keys: &mut Vec<NativeKeyEquivalent>,
    ) {
        path.push(menu.name.clone());
        for item in &menu.items {
            match item {
                MenuItem::Submenu(submenu) => collect(submenu, path, keymap, keys),
                MenuItem::Action { name, action, .. } => {
                    if crate::keybindings::Command::ALL
                        .into_iter()
                        .any(|command| command.action().partial_eq(action.as_ref()))
                        && let Some(shortcut) =
                            crate::desktop_profile::installed_shortcut(keymap, action.as_ref())
                        && let Some(key) = missing_native_key_equivalent(shortcut.key())
                    {
                        let mut item_path = path.clone();
                        item_path.push(name.clone());
                        keys.push(NativeKeyEquivalent {
                            path: item_path,
                            modifiers: shortcut.modifiers(),
                            key,
                        });
                    }
                }
                MenuItem::Separator | MenuItem::SystemMenu(_) => {}
            }
        }
        path.pop();
    }
    let mut keys = Vec::new();
    let mut path = Vec::new();
    for menu in menus {
        collect(menu, &mut path, keymap, &mut keys);
    }
    keys
}

#[cfg(not(test))]
mod native {
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, msg_send};
    use objc2_app_kit::{
        NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName,
        NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits, NSApplication,
        NSEventModifierFlags, NSHelpManager, NSImage, NSMenu, NSMenuItem, NSTextAlignment, NSWorkspace,
    };
    use objc2_foundation::{NSDictionary, NSMutableAttributedString, NSRange, NSString, NSURL};

    use super::{
        ApplicationMenuCommand, ApplicationMenuError, MENU_ITEM_ICONS, MenuItemIcon,
        NativeKeyEquivalent,
    };

    const ABOUT_DESCRIPTION: &str = "A native, keyboard-first desktop terminal multiplexer.";
    const HELP_URL: &str = "https://github.com/sadiksaifi/SpaceTerm";

    pub(super) fn decorate(
        application_name: &str,
        native_keys: &[NativeKeyEquivalent],
    ) -> Result<(), ApplicationMenuError> {
        let mtm = MainThreadMarker::new().ok_or(ApplicationMenuError::OffMainThread)?;
        decorate_main_menu(
            &NSApplication::sharedApplication(mtm),
            application_name,
            native_keys,
        )
    }

    pub(super) fn perform(
        command: ApplicationMenuCommand,
        application_name: &str,
    ) -> Result<(), ApplicationMenuError> {
        let mtm = MainThreadMarker::new().ok_or(ApplicationMenuError::OffMainThread)?;
        let application = NSApplication::sharedApplication(mtm);
        match command {
            ApplicationMenuCommand::ShowAbout => show_about(&application, application_name),
            ApplicationMenuCommand::ZoomActiveWindow => zoom_active_window(&application),
            ApplicationMenuCommand::BringAllWindowsToFront => {
                application.arrangeInFront(None);
                Ok(())
            }
            ApplicationMenuCommand::OpenHelp => open_help(),
        }
    }

    fn show_about(
        application: &NSApplication,
        application_name: &str,
    ) -> Result<(), ApplicationMenuError> {
        let name = NSString::from_str(application_name);
        let version = NSString::from_str(env!("SPACETERM_VERSION"));
        let description = NSString::from_str(ABOUT_DESCRIPTION);
        let credits = NSMutableAttributedString::initWithString(
            NSMutableAttributedString::alloc(),
            &description,
        );
        // SAFETY: AppKit implements this category method. The range spans this live string.
        let _: () = unsafe {
            msg_send![&*credits, setAlignment: NSTextAlignment::Center,
                                 range: NSRange::new(0, credits.length())]
        };
        let icon = application
            .applicationIconImage()
            .ok_or(ApplicationMenuError::Unavailable)?;
        // SAFETY: AppKit exports these four immutable option keys.
        let keys = unsafe {
            [
                NSAboutPanelOptionApplicationName,
                NSAboutPanelOptionApplicationVersion,
                NSAboutPanelOptionCredits,
                NSAboutPanelOptionApplicationIcon,
            ]
        };
        let values: [&AnyObject; 4] = [&name, &version, &credits, &icon];
        let options = NSDictionary::from_slices(&keys, &values);
        // SAFETY: Every dictionary value has the type AppKit expects for its corresponding key.
        unsafe { application.orderFrontStandardAboutPanelWithOptions(&options) };
        Ok(())
    }

    fn decorate_main_menu(
        application: &NSApplication,
        application_name: &str,
        native_keys: &[NativeKeyEquivalent],
    ) -> Result<(), ApplicationMenuError> {
        let main_menu = application
            .mainMenu()
            .ok_or(ApplicationMenuError::Unavailable)?;
        let help_menu = main_menu
            .itemWithTitle(&NSString::from_str("Help"))
            .and_then(|item| item.submenu())
            .ok_or(ApplicationMenuError::Unavailable)?;
        // GPUI creates its menus without a nib. Initialize AppKit's Help support so
        // it installs native menu search and handles the system Help shortcut.
        let _ = NSHelpManager::sharedHelpManager(application.mtm());
        application.setHelpMenu(Some(&help_menu));
        for (item_title, symbol) in super::application_menu_item_icons(application_name) {
            let decoration = MenuItemIcon {
                menu: application_name,
                submenu: None,
                item: &item_title,
                symbol,
            };
            let item = find_menu_item(&main_menu, &decoration, application_name)
                .ok_or(ApplicationMenuError::Unavailable)?;
            set_symbol_image(&item, decoration.symbol)?;
        }
        for decoration in MENU_ITEM_ICONS {
            let item = find_menu_item(&main_menu, decoration, application_name)
                .ok_or(ApplicationMenuError::Unavailable)?;
            set_symbol_image(&item, decoration.symbol)?;
        }
        // Complete the named keys omitted by GPUI's native translation.
        for equivalent in native_keys {
            let (title, menu_path) = equivalent
                .path
                .split_last()
                .ok_or(ApplicationMenuError::Unavailable)?;
            let mut menu = main_menu.clone();
            for title in menu_path {
                menu = menu
                    .itemWithTitle(&NSString::from_str(title))
                    .and_then(|item| item.submenu())
                    .ok_or(ApplicationMenuError::Unavailable)?;
            }
            let item = menu
                .itemWithTitle(&NSString::from_str(title))
                .ok_or(ApplicationMenuError::Unavailable)?;
            let key_equivalent = NSString::from_str(equivalent.key);
            let mut modifiers = NSEventModifierFlags::empty();
            for (enabled, flag) in [
                (equivalent.modifiers.control, NSEventModifierFlags::Control),
                (equivalent.modifiers.alt, NSEventModifierFlags::Option),
                (equivalent.modifiers.shift, NSEventModifierFlags::Shift),
                (equivalent.modifiers.platform, NSEventModifierFlags::Command),
            ] {
                if enabled {
                    modifiers |= flag;
                }
            }
            item.setKeyEquivalent(&key_equivalent);
            item.setKeyEquivalentModifierMask(modifiers);
            if item.keyEquivalent() != key_equivalent
                || item.keyEquivalentModifierMask() != modifiers
            {
                return Err(ApplicationMenuError::Unavailable);
            }
        }
        Ok(())
    }

    fn find_menu_item(
        main_menu: &NSMenu,
        decoration: &MenuItemIcon<'_>,
        application_name: &str,
    ) -> Option<objc2::rc::Retained<NSMenuItem>> {
        let top_item = main_menu
            .itemWithTitle(&NSString::from_str(decoration.menu))
            .or_else(|| {
                (decoration.menu == application_name)
                    .then(|| main_menu.itemAtIndex(0))
                    .flatten()
            })?;
        let mut menu = top_item.submenu()?;
        if let Some(submenu_title) = decoration.submenu {
            menu = menu
                .itemWithTitle(&NSString::from_str(submenu_title))?
                .submenu()?;
        }
        menu.itemWithTitle(&NSString::from_str(decoration.item))
    }

    fn set_symbol_image(item: &NSMenuItem, symbol: &str) -> Result<(), ApplicationMenuError> {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            None,
        )
        .ok_or(ApplicationMenuError::Unavailable)?;
        item.setImage(Some(&image));
        item.image().ok_or(ApplicationMenuError::Unavailable)?;
        Ok(())
    }

    fn zoom_active_window(application: &NSApplication) -> Result<(), ApplicationMenuError> {
        let window = application
            .keyWindow()
            .or_else(|| application.mainWindow())
            .ok_or(ApplicationMenuError::MissingActiveWindow)?;
        window.performZoom(None);
        Ok(())
    }

    fn open_help() -> Result<(), ApplicationMenuError> {
        let url = NSURL::URLWithString(&NSString::from_str(HELP_URL))
            .ok_or(ApplicationMenuError::Unavailable)?;
        NSWorkspace::sharedWorkspace()
            .openURL(&url)
            .then_some(())
            .ok_or(ApplicationMenuError::Rejected)
    }
}

#[cfg(test)]
mod native {
    use super::{ApplicationMenuCommand, ApplicationMenuError};

    pub(super) fn decorate(
        _: &str,
        _: &[super::NativeKeyEquivalent],
    ) -> Result<(), ApplicationMenuError> {
        Ok(())
    }

    pub(super) fn perform(_: ApplicationMenuCommand, _: &str) -> Result<(), ApplicationMenuError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui::{Action, OwnedMenu, OwnedMenuItem};

    use super::*;

    fn labels(menu: OwnedMenu) -> Vec<String> {
        menu.items
            .into_iter()
            .map(|item| match item {
                OwnedMenuItem::Action { name, .. } => name,
                OwnedMenuItem::Separator => "|".to_owned(),
                OwnedMenuItem::Submenu(submenu) => submenu.name.to_string(),
                OwnedMenuItem::SystemMenu(menu) => menu.name.to_string(),
            })
            .collect()
    }

    fn custom_menu_item_paths(
        application_name: &str,
    ) -> BTreeSet<(String, Option<String>, String)> {
        let mut paths = BTreeSet::new();
        for menu in menus(application_name).into_iter().map(Menu::owned) {
            let menu_name = menu.name.to_string();
            for item in menu.items {
                match item {
                    OwnedMenuItem::Action { name, .. } => {
                        paths.insert((menu_name.clone(), None, name));
                    }
                    OwnedMenuItem::Submenu(submenu) => {
                        let submenu_name = submenu.name.to_string();
                        paths.insert((menu_name.clone(), None, submenu_name.clone()));
                        for item in submenu.items {
                            if let OwnedMenuItem::Action { name, .. } = item {
                                paths.insert((menu_name.clone(), Some(submenu_name.clone()), name));
                            }
                        }
                    }
                    OwnedMenuItem::Separator | OwnedMenuItem::SystemMenu(_) => {}
                }
            }
        }
        paths
    }

    #[test]
    fn native_equivalents_follow_overrides_for_top_level_and_nested_commands() {
        let profile = crate::desktop_profile::default_keymap::profile(
            super::super::macos_reserved_shortcuts::shortcuts(),
        )
        .unwrap();
        for (preferences, expected) in [
            (
                "{}",
                vec![(vec!["View", TOGGLE_PANE_ZOOM_TITLE], "shift-cmd-enter")],
            ),
            (
                r#"{"toggle_pane_zoom":"alt-cmd-enter"}"#,
                vec![(vec!["View", TOGGLE_PANE_ZOOM_TITLE], "alt-cmd-enter")],
            ),
            (
                r#"{"toggle_pane_zoom":"cmd-y","focus_pane_left":"ctrl-shift-cmd-enter"}"#,
                vec![(vec!["View", "Focus Pane", "Left"], "ctrl-shift-cmd-enter")],
            ),
            (r#"{"toggle_pane_zoom":null}"#, vec![]),
            (
                r#"{"toggle_pane_zoom":"ctrl-tab"}"#,
                vec![(vec!["View", TOGGLE_PANE_ZOOM_TITLE], "ctrl-tab")],
            ),
            (
                r#"{"toggle_pane_zoom":null,"focus_pane_left":"ctrl-shift-tab"}"#,
                vec![(vec!["View", "Focus Pane", "Left"], "ctrl-shift-tab")],
            ),
        ] {
            let preferences = serde_json::from_str(preferences).unwrap();
            let keymap = Keymap::new(profile.resolve(&preferences).key_bindings());
            let expected = expected
                .into_iter()
                .map(|(path, shortcut)| NativeKeyEquivalent {
                    path: path.into_iter().map(SharedString::from).collect(),
                    key: if shortcut.ends_with("tab") {
                        "\t"
                    } else {
                        "\r"
                    },
                    modifiers: crate::keybindings::Shortcut::parse(shortcut)
                        .unwrap()
                        .modifiers(),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                native_key_equivalents(&menus("SpaceTerm"), &keymap),
                expected
            );
        }
    }

    #[test]
    fn macos_menu_bar_should_expose_the_standard_top_level_structure() {
        let names = menus("SpaceTerm")
            .into_iter()
            .map(|menu| menu.name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            ["SpaceTerm", "File", "Edit", "View", "Window", "Help"]
        );
    }

    #[test]
    fn every_custom_menu_item_should_have_a_native_icon() {
        let mut decorated = MENU_ITEM_ICONS
            .iter()
            .map(|decoration| {
                (
                    decoration.menu.to_owned(),
                    decoration.submenu.map(str::to_owned),
                    decoration.item.to_owned(),
                )
            })
            .collect::<BTreeSet<_>>();
        decorated.extend(
            application_menu_item_icons("SpaceTerm")
                .map(|(item, _)| ("SpaceTerm".to_owned(), None, item)),
        );

        assert_eq!(decorated, custom_menu_item_paths("SpaceTerm"));
    }

    #[test]
    fn application_menu_should_include_about_and_standard_macos_commands() {
        assert_eq!(
            labels(application_menu("SpaceTerm").owned()),
            [
                "About SpaceTerm",
                "Check for Updates…",
                "|",
                "Settings…",
                "|",
                "Services",
                "|",
                "Hide SpaceTerm",
                "Hide Others",
                "Show All",
                "|",
                "Quit SpaceTerm",
            ]
        );
    }

    #[test]
    fn development_application_menu_should_use_its_visible_name() {
        assert_eq!(
            labels(application_menu("SpaceTerm Dev").owned()),
            [
                "About SpaceTerm Dev",
                "Check for Updates…",
                "|",
                "Settings…",
                "|",
                "Services",
                "|",
                "Hide SpaceTerm Dev",
                "Hide Others",
                "Show All",
                "|",
                "Quit SpaceTerm Dev",
            ]
        );
    }

    #[test]
    fn view_menu_should_expose_terminal_and_pane_commands() {
        assert_eq!(
            labels(view_menu().owned()),
            [
                "Toggle Sidebar",
                "Toggle Sidebar Focus",
                "|",
                "Increase Terminal Font Size",
                "Decrease Terminal Font Size",
                "Reset Terminal Font Size",
                "|",
                "Split Right",
                "Split Down",
                "Focus Pane",
                "Toggle Pane Zoom",
            ]
        );
    }

    #[test]
    fn window_and_help_menus_should_expose_native_and_support_commands() {
        assert_eq!(
            (labels(window_menu().owned()), labels(help_menu().owned())),
            (
                ["Minimize", "Zoom", "|", "Bring All to Front"]
                    .map(str::to_owned)
                    .to_vec(),
                [
                    "SpaceTerm Help",
                    "Release Notes",
                    "|",
                    "Export Terminal Diagnostics…"
                ]
                .map(str::to_owned)
                .to_vec(),
            )
        );
    }

    #[test]
    fn file_menu_should_preserve_semantic_actions_and_grouping() {
        let file = file_menu().owned();
        let actions = file
            .items
            .iter()
            .filter_map(|item| match item {
                OwnedMenuItem::Action { action, .. } => Some(action.name()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            actions,
            [
                NewWorkspace.name(),
                crate::ui::NewRemoteWorkspace.name(),
                SwitchWorkspace.name(),
                CreateTab.name(),
                ClosePane.name(),
                CloseTab.name(),
                CloseWorkspace.name(),
                ExportTerminalDiagnostics.name(),
            ]
        );
    }
}
