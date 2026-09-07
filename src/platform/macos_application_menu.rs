use gpui::{App, Menu, MenuItem, SystemMenuType};
use spaceterm_ui::{EditCopy, EditCut, EditPaste, EditRedo, EditSelectAll, EditUndo};

use super::application_menu::{
    ApplicationMenuAdapter, ApplicationMenuCommand, ApplicationMenuError,
};
use crate::app::{
    BringAllWindowsToFront, HideApplication, HideOtherApplications, MinimizeWindow,
    OpenApplicationHelp, QuitApplication, ShowAboutApplication, ShowAllApplications,
    ZoomActiveWindow,
};
use crate::ui::{
    ClosePane, CloseTab, CloseWorkspace, CreateScratchWorkspace, CreateTab,
    DecreaseTerminalFontSize, ExportTerminalDiagnostics, FindNext, FindPrevious, FocusPaneDown,
    FocusPaneLeft, FocusPaneRight, FocusPaneUp, IncreaseTerminalFontSize, NewWorkspace,
    OpenLocalProject, OpenTerminalFind, ResetTerminalFontSize, SearchWorkspaces, SplitDown,
    SplitRight, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
};

pub(crate) struct MacosApplicationMenuAdapter;

const TOGGLE_PANE_ZOOM_TITLE: &str = "Toggle Pane Zoom";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuItemIcon {
    menu: &'static str,
    submenu: Option<&'static str>,
    item: &'static str,
    symbol: &'static str,
}

const MENU_ITEM_ICONS: &[MenuItemIcon] = &[
    MenuItemIcon {
        menu: "SpaceTerm",
        submenu: None,
        item: "About SpaceTerm",
        symbol: "info.circle",
    },
    MenuItemIcon {
        menu: "SpaceTerm",
        submenu: None,
        item: "Hide SpaceTerm",
        symbol: "eye.slash",
    },
    MenuItemIcon {
        menu: "SpaceTerm",
        submenu: None,
        item: "Hide Others",
        symbol: "eye.slash.fill",
    },
    MenuItemIcon {
        menu: "SpaceTerm",
        submenu: None,
        item: "Show All",
        symbol: "eye",
    },
    MenuItemIcon {
        menu: "SpaceTerm",
        submenu: None,
        item: "Quit SpaceTerm",
        symbol: "power",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "New Workspace…",
        symbol: "folder.badge.plus",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "New Scratch Workspace",
        symbol: "plus.rectangle",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Open Local Project…",
        symbol: "folder",
    },
    MenuItemIcon {
        menu: "File",
        submenu: None,
        item: "Search Workspaces…",
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
        item: "Export Terminal Diagnostics…",
        symbol: "doc.text.magnifyingglass",
    },
];

impl ApplicationMenuAdapter for MacosApplicationMenuAdapter {
    fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
        cx.set_menus(menus());
        native::decorate()
    }

    fn perform(&self, command: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
        native::perform(command)
    }
}

fn menus() -> Vec<Menu> {
    vec![
        application_menu(),
        file_menu(),
        edit_menu(),
        view_menu(),
        window_menu(),
        help_menu(),
    ]
}

fn application_menu() -> Menu {
    Menu {
        name: "SpaceTerm".into(),
        items: vec![
            MenuItem::action("About SpaceTerm", ShowAboutApplication),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide SpaceTerm", HideApplication),
            MenuItem::action("Hide Others", HideOtherApplications),
            MenuItem::action("Show All", ShowAllApplications),
            MenuItem::separator(),
            MenuItem::action("Quit SpaceTerm", QuitApplication),
        ],
    }
}

fn file_menu() -> Menu {
    Menu {
        name: "File".into(),
        items: vec![
            MenuItem::action("New Workspace…", NewWorkspace),
            MenuItem::action("New Scratch Workspace", CreateScratchWorkspace),
            MenuItem::action("Open Local Project…", OpenLocalProject),
            MenuItem::action("Search Workspaces…", SearchWorkspaces),
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
        name: "Help".into(),
        items: vec![
            MenuItem::action("SpaceTerm Help", OpenApplicationHelp),
            MenuItem::separator(),
            MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
        ],
    }
}

#[cfg(not(test))]
mod native {
    use cocoa::appkit::{NSApp, NSEventModifierFlags};
    use cocoa::base::{BOOL, YES, id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSDictionary, NSString, NSUInteger};
    use objc::{class, msg_send, sel, sel_impl};

    use super::{
        ApplicationMenuCommand, ApplicationMenuError, MENU_ITEM_ICONS, MenuItemIcon,
        TOGGLE_PANE_ZOOM_TITLE,
    };

    const HELP_URL: &str = "https://github.com/sadiksaifi/SpaceTerm";

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "NSAboutPanelOptionApplicationName"]
        static ABOUT_APPLICATION_NAME: id;
        #[link_name = "NSAboutPanelOptionApplicationVersion"]
        static ABOUT_APPLICATION_VERSION: id;
    }

    pub(super) fn decorate() -> Result<(), ApplicationMenuError> {
        if !main_thread() {
            return Err(ApplicationMenuError::OffMainThread);
        }

        // SAFETY: The main-thread check confines all AppKit objects to AppKit's thread. GPUI has
        // synchronously installed the main menu before this function runs, and every transient
        // string is used before the local autorelease pool drains.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = decorate_main_menu();
            pool.drain();
            result
        }
    }

    pub(super) fn perform(command: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
        if !main_thread() {
            return Err(ApplicationMenuError::OffMainThread);
        }

        // SAFETY: The main-thread check confines all AppKit objects to AppKit's thread. Every
        // transient object is used synchronously before the local autorelease pool drains.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = match command {
                ApplicationMenuCommand::ShowAbout => show_about(),
                ApplicationMenuCommand::ZoomActiveWindow => zoom_active_window(),
                ApplicationMenuCommand::BringAllWindowsToFront => bring_all_windows_to_front(),
                ApplicationMenuCommand::OpenHelp => open_help(),
            };
            pool.drain();
            result
        }
    }

    unsafe fn show_about() -> Result<(), ApplicationMenuError> {
        let application = unsafe { NSApp() };
        if application == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let name = unsafe { NSString::alloc(nil).init_str("SpaceTerm").autorelease() };
        let version = unsafe {
            NSString::alloc(nil)
                .init_str(env!("CARGO_PKG_VERSION"))
                .autorelease()
        };
        let values = [name, version];
        let keys = unsafe { [ABOUT_APPLICATION_NAME, ABOUT_APPLICATION_VERSION] };
        let options = unsafe {
            NSDictionary::dictionaryWithObjects_forKeys_count_(
                nil,
                values.as_ptr(),
                keys.as_ptr(),
                values.len() as NSUInteger,
            )
        };
        let _: () =
            unsafe { msg_send![application, orderFrontStandardAboutPanelWithOptions: options] };
        Ok(())
    }

    unsafe fn decorate_main_menu() -> Result<(), ApplicationMenuError> {
        let application = unsafe { NSApp() };
        if application == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let main_menu: id = unsafe { msg_send![application, mainMenu] };
        if main_menu == nil {
            return Err(ApplicationMenuError::Unavailable);
        }

        for decoration in MENU_ITEM_ICONS {
            let item = unsafe { find_menu_item(main_menu, decoration) }
                .ok_or(ApplicationMenuError::Unavailable)?;
            unsafe { set_symbol_image(item, decoration.symbol) }?;
        }

        let zoom_decoration = MenuItemIcon {
            menu: "View",
            submenu: None,
            item: TOGGLE_PANE_ZOOM_TITLE,
            symbol: "",
        };
        let zoom_item = unsafe { find_menu_item(main_menu, &zoom_decoration) }
            .ok_or(ApplicationMenuError::Unavailable)?;
        let key_equivalent = unsafe { NSString::alloc(nil).init_str("\r").autorelease() };
        let modifiers =
            NSEventModifierFlags::NSCommandKeyMask | NSEventModifierFlags::NSShiftKeyMask;
        let _: () = unsafe { msg_send![zoom_item, setKeyEquivalent: key_equivalent] };
        let _: () = unsafe { msg_send![zoom_item, setKeyEquivalentModifierMask: modifiers] };
        let installed_equivalent: id = unsafe { msg_send![zoom_item, keyEquivalent] };
        let equivalent_matches: BOOL =
            unsafe { msg_send![installed_equivalent, isEqualToString: key_equivalent] };
        let installed_modifiers: NSUInteger =
            unsafe { msg_send![zoom_item, keyEquivalentModifierMask] };
        if equivalent_matches != YES || installed_modifiers != modifiers.bits() {
            return Err(ApplicationMenuError::Unavailable);
        }

        Ok(())
    }

    unsafe fn find_menu_item(main_menu: id, decoration: &MenuItemIcon) -> Option<id> {
        let menu_title = unsafe { NSString::alloc(nil).init_str(decoration.menu).autorelease() };
        let mut top_item: id = unsafe { msg_send![main_menu, itemWithTitle: menu_title] };
        if top_item == nil && decoration.menu == "SpaceTerm" {
            top_item = unsafe { msg_send![main_menu, itemAtIndex: 0_isize] };
        }
        if top_item == nil {
            return None;
        }

        let mut menu: id = unsafe { msg_send![top_item, submenu] };
        if menu == nil {
            return None;
        }
        if let Some(submenu_title) = decoration.submenu {
            let submenu_title =
                unsafe { NSString::alloc(nil).init_str(submenu_title).autorelease() };
            let submenu_item: id = unsafe { msg_send![menu, itemWithTitle: submenu_title] };
            if submenu_item == nil {
                return None;
            }
            menu = unsafe { msg_send![submenu_item, submenu] };
            if menu == nil {
                return None;
            }
        }

        let item_title = unsafe { NSString::alloc(nil).init_str(decoration.item).autorelease() };
        let item: id = unsafe { msg_send![menu, itemWithTitle: item_title] };
        (item != nil).then_some(item)
    }

    unsafe fn set_symbol_image(item: id, symbol: &str) -> Result<(), ApplicationMenuError> {
        let symbol = unsafe { NSString::alloc(nil).init_str(symbol).autorelease() };
        let image: id = unsafe {
            msg_send![class!(NSImage), imageWithSystemSymbolName: symbol
                                               accessibilityDescription: nil]
        };
        if image == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let _: () = unsafe { msg_send![item, setImage: image] };
        let installed_image: id = unsafe { msg_send![item, image] };
        if installed_image == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        Ok(())
    }

    unsafe fn zoom_active_window() -> Result<(), ApplicationMenuError> {
        let application = unsafe { NSApp() };
        if application == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let mut window: id = unsafe { msg_send![application, keyWindow] };
        if window == nil {
            window = unsafe { msg_send![application, mainWindow] };
        }
        if window == nil {
            return Err(ApplicationMenuError::MissingActiveWindow);
        }
        let _: () = unsafe { msg_send![window, performZoom: nil] };
        Ok(())
    }

    unsafe fn bring_all_windows_to_front() -> Result<(), ApplicationMenuError> {
        let application = unsafe { NSApp() };
        if application == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let _: () = unsafe { msg_send![application, arrangeInFront: nil] };
        Ok(())
    }

    unsafe fn open_help() -> Result<(), ApplicationMenuError> {
        let string = unsafe { NSString::alloc(nil).init_str(HELP_URL).autorelease() };
        let url: id = unsafe { msg_send![class!(NSURL), URLWithString: string] };
        if url == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let workspace: id = unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] };
        if workspace == nil {
            return Err(ApplicationMenuError::Unavailable);
        }
        let opened: BOOL = unsafe { msg_send![workspace, openURL: url] };
        if opened == YES {
            Ok(())
        } else {
            Err(ApplicationMenuError::Rejected)
        }
    }

    fn main_thread() -> bool {
        // SAFETY: `NSThread.isMainThread` is a process query with no object lifetime transfer.
        unsafe {
            let is_main: BOOL = msg_send![class!(NSThread), isMainThread];
            is_main == YES
        }
    }
}

#[cfg(test)]
mod native {
    use super::{ApplicationMenuCommand, ApplicationMenuError};

    pub(super) fn decorate() -> Result<(), ApplicationMenuError> {
        Ok(())
    }

    pub(super) fn perform(_: ApplicationMenuCommand) -> Result<(), ApplicationMenuError> {
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

    fn custom_menu_item_paths() -> BTreeSet<(String, Option<String>, String)> {
        let mut paths = BTreeSet::new();
        for menu in menus().into_iter().map(Menu::owned) {
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
    fn macos_menu_bar_should_expose_the_standard_top_level_structure() {
        let names = menus()
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
        let decorated = MENU_ITEM_ICONS
            .iter()
            .map(|decoration| {
                (
                    decoration.menu.to_owned(),
                    decoration.submenu.map(str::to_owned),
                    decoration.item.to_owned(),
                )
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(decorated, custom_menu_item_paths());
    }

    #[test]
    fn application_menu_should_include_about_and_standard_macos_commands() {
        assert_eq!(
            labels(application_menu().owned()),
            [
                "About SpaceTerm",
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
                ["SpaceTerm Help", "|", "Export Terminal Diagnostics…"]
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
                CreateScratchWorkspace.name(),
                OpenLocalProject.name(),
                SearchWorkspaces.name(),
                CreateTab.name(),
                ClosePane.name(),
                CloseTab.name(),
                CloseWorkspace.name(),
                ExportTerminalDiagnostics.name(),
            ]
        );
    }
}
