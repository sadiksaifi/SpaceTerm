//! The Command Palette: every Command and Add Server, found by name from one search field.
//!
//! The palette belongs to the Workspace Window, where Commands act. Opening it from another
//! window, such as Settings, brings the Workspace Window forward first. Choosing an entry closes
//! the palette, returns focus to where it was, and runs the entry there, exactly as its Shortcut
//! or menu item would.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{App, Context, Entity, Global, SharedString, Window, WindowId, actions};
use spaceterm_ui::{
    CommandPalette, CommandPaletteAccessory, CommandPaletteEvent, CommandPaletteItem,
};

use super::WorkspaceManager;
use crate::keybindings::{Command, CommandGroup};

actions!(spaceterm, [OpenCommandPalette]);

/// One entry the palette offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaletteEntry {
    Command(Command),
    /// Opens the Add Server sheet. It is an application action rather than a Command, so it has
    /// no configurable Shortcut.
    AddServer,
}

/// The palette each Workspace Window presents, keyed by that window.
#[derive(Default)]
struct ApplicationPalettes(HashMap<WindowId, Entity<ApplicationPalette>>);

impl Global for ApplicationPalettes {}

/// Registers the application-scoped action that opens the palette.
pub(crate) fn init(cx: &mut App) {
    // A Shortcut delivers the action while its window is updating, and GPUI can't update a window
    // inside its own update. Opening waits until that update ends.
    cx.on_action(|_: &OpenCommandPalette, cx| cx.defer(open));
}

/// The palette `window` presents, for the window to mount above its content.
pub(crate) fn overlay(window: &Window, cx: &App) -> Option<Entity<ApplicationPalette>> {
    cx.try_global::<ApplicationPalettes>()?
        .0
        .get(&window.window_handle().window_id())
        .cloned()
}

/// Opens the palette in the active Workspace Window, or in the first one when another window is
/// active.
fn open(cx: &mut App) {
    let target = cx
        .active_window()
        .and_then(|window| window.downcast::<WorkspaceManager>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<WorkspaceManager>())
        });
    let Some(target) = target else {
        return;
    };
    // A window that closed takes its palette with it.
    let open_windows: Vec<WindowId> = cx
        .windows()
        .into_iter()
        .map(|window| window.window_id())
        .collect();
    cx.default_global::<ApplicationPalettes>()
        .0
        .retain(|window, _| open_windows.contains(window));
    let _ = target.update(cx, |_, window, cx| {
        window.activate_window();
        let id = window.window_handle().window_id();
        let existing = cx
            .try_global::<ApplicationPalettes>()
            .and_then(|palettes| palettes.0.get(&id).cloned());
        let palette = match existing {
            Some(palette) => palette,
            None => {
                let palette = cx.new(|cx| ApplicationPalette::new(window, cx));
                cx.default_global::<ApplicationPalettes>()
                    .0
                    .insert(id, palette.clone());
                palette
            }
        };
        palette.update(cx, |palette, cx| palette.open(window, cx));
        // The window mounts the palette on its next frame.
        cx.notify();
    });
}

/// One Workspace Window's palette.
pub(crate) struct ApplicationPalette {
    palette: Entity<CommandPalette<PaletteEntry>>,
}

impl ApplicationPalette {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let palette = cx.new(|cx| CommandPalette::new("Search commands", Vec::new(), window, cx));
        cx.subscribe_in(
            &palette,
            window,
            |_, _, event: &CommandPaletteEvent<PaletteEntry>, window, cx| {
                if let CommandPaletteEvent::Activated(activation) = event {
                    run(*activation.item_id(), window, cx);
                }
            },
        )
        .detach();
        Self { palette }
    }

    /// Lists the entries afresh, so each Shortcut reads as it is assigned now.
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = entries(cx);
        self.palette.update(cx, |palette, cx| {
            palette.set_items(items, cx);
            palette.set_query("", cx);
            palette.open(window, cx);
        });
    }
}

impl Render for ApplicationPalette {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.palette.clone()
    }
}

/// Runs `entry` where focus returned when the palette closed.
fn run(entry: PaletteEntry, window: &mut Window, cx: &mut App) {
    match entry {
        PaletteEntry::Command(command) => window.dispatch_action(command.action(), cx),
        PaletteEntry::AddServer => {
            cx.defer(|cx| crate::ui::settings_window::open_add_server(None, None, cx));
        }
    }
}

/// Every entry in presentation order, grouped as the menus group them. Add Server follows the
/// Workspace Commands, because a server is where a new Workspace runs.
fn entry_order() -> Vec<PaletteEntry> {
    let workspace = Command::ALL
        .into_iter()
        .filter(|command| command.group() == CommandGroup::Workspace);
    let others = Command::ALL
        .into_iter()
        .filter(|command| command.group() != CommandGroup::Workspace);
    workspace
        .map(PaletteEntry::Command)
        .chain([PaletteEntry::AddServer])
        .chain(others.map(PaletteEntry::Command))
        .collect()
}

fn entries(cx: &App) -> Vec<CommandPaletteItem<PaletteEntry>> {
    entry_order()
        .into_iter()
        .map(|entry| match entry {
            PaletteEntry::AddServer => add_server_entry(),
            PaletteEntry::Command(command) => {
                let item = CommandPaletteItem::new(entry, command.label())
                    .section(section_title(command.group()))
                    .debug_selector(format!("command-palette-{}", command.id()));
                match shortcut_text(command, cx) {
                    Some(shortcut) => item.trailing(CommandPaletteAccessory::Shortcut(shortcut)),
                    None => item,
                }
            }
        })
        .collect()
}

fn add_server_entry() -> CommandPaletteItem<PaletteEntry> {
    CommandPaletteItem::new(PaletteEntry::AddServer, "Add Server\u{2026}")
        .section("Servers")
        .keywords([
            "ssh",
            "remote",
            "invitation",
            "pair",
            "join",
            "connect",
            "computer",
        ])
        .debug_selector("command-palette-add-server")
}

const fn section_title(group: CommandGroup) -> &'static str {
    match group {
        CommandGroup::Workspace => "Workspaces",
        CommandGroup::Tab => "Tabs",
        CommandGroup::Pane => "Panes",
        CommandGroup::Terminal => "Terminal",
        CommandGroup::View => "View",
        CommandGroup::Help => "Help",
    }
}

/// The Shortcut assigned to `command` now, written the way this desktop writes Shortcuts.
fn shortcut_text(command: Command, cx: &App) -> Option<SharedString> {
    crate::desktop_profile::DesktopPresentation::get(cx).shortcut(command.action().as_ref())
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::settings::sidebar::SidebarState;
    use crate::ui::workspace_manager::WorkspaceManagerAdapters;

    /// A Workspace Window presenting one Workspace, with the application's actions and Shortcuts
    /// installed as at launch.
    fn workspace_window(
        cx: &mut TestAppContext,
    ) -> (Entity<WorkspaceManager>, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        cx.update(|cx| {
            crate::app::init(
                cx,
                Rc::new(
                    crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
                ),
                Rc::new(
                    crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
                ),
            )
            .expect("application initialization should succeed");
        });
        let (manager, cx) = cx.add_window_view(|window, cx| {
            crate::ui::updates::server_rows::testing::install_up_to_date(cx);
            let mut manager = WorkspaceManager::new_with_adapters(
                std::env::temp_dir(),
                WorkspaceManagerAdapters {
                    local_filesystem:
                        crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
                    key_input: Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                    accessibility: Rc::new(
                        crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
                    ),
                    native_services: crate::terminal::native_services::testing::adapters(),
                    lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
                    directory_selection: Rc::new(crate::directory_selection::GpuiDirectorySelection),
                    window_drag: Rc::new(
                        crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform::default(),
                    ),
                    local_server: crate::ui::launch_testing::unanswered(),
                    server_directories:
                        crate::ui::directory_picker::server_directory_testing::older(),
                },
                SidebarState::Expanded,
                window,
                cx,
            );
            manager.present_desktop_workspace(window, cx);
            manager
        });
        cx.update(|window, cx| {
            window.activate_window();
            manager.update(cx, |manager, cx| manager.focus(window, cx));
        });
        cx.run_until_parked();
        (manager, cx)
    }

    fn palette_open(cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| {
            overlay(window, cx).is_some_and(|palette| palette.read(cx).palette.read(cx).is_open())
        })
    }

    #[gpui::test]
    fn choosing_new_tab_in_the_palette_opens_a_tab_in_the_workspace_window(
        cx: &mut TestAppContext,
    ) {
        let (manager, cx) = workspace_window(cx);
        let panes = |cx: &mut VisualTestContext| {
            manager.read_with(cx, |manager, cx| manager.application_pane_facts(cx).len())
        };
        let before = panes(cx);

        cx.dispatch_action(OpenCommandPalette);
        cx.run_until_parked();
        assert!(
            palette_open(cx),
            "the Workspace Window presents the palette"
        );

        cx.simulate_input("New Tab");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(!palette_open(cx), "choosing an entry closes the palette");
        assert_eq!(
            panes(cx),
            before + 1,
            "New Tab runs where focus returned, adding one Tab and its Pane"
        );
    }
}
