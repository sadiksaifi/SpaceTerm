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
    cx.on_action(|_: &OpenCommandPalette, cx| open(cx));
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
    use super::*;

    #[test]
    fn the_palette_offers_every_command_once_and_add_server_after_the_workspace_commands() {
        let order = entry_order();
        assert_eq!(order.len(), Command::ALL.len() + 1);
        for command in Command::ALL {
            assert_eq!(
                order
                    .iter()
                    .filter(|entry| **entry == PaletteEntry::Command(command))
                    .count(),
                1,
                "{command:?}"
            );
        }
        let add_server = order
            .iter()
            .position(|entry| *entry == PaletteEntry::AddServer)
            .expect("Add Server is offered");
        assert!(order[..add_server].iter().all(|entry| matches!(
            entry,
            PaletteEntry::Command(command) if command.group() == CommandGroup::Workspace
        )));
        assert!(order[add_server + 1..].iter().all(|entry| matches!(
            entry,
            PaletteEntry::Command(command) if command.group() != CommandGroup::Workspace
        )));
    }
}
