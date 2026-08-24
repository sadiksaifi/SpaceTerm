use gpui::prelude::*;
use gpui::px;
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::MenuEntry;

use crate::terminal::NativeContextActions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalContextMenuCommand {
    Copy,
    OpenLink,
    QuickLook,
}

pub(crate) fn terminal_context_menu_entries(
    actions: NativeContextActions,
) -> Vec<MenuEntry<TerminalContextMenuCommand>> {
    vec![
        menu_entry(
            TerminalContextMenuCommand::Copy,
            "Copy",
            "doc.on.doc",
            actions.copy,
        )
        .shortcut("⌘C"),
        menu_entry(
            TerminalContextMenuCommand::OpenLink,
            "Open Link",
            "arrow.up.right.square",
            actions.open_link,
        ),
        menu_entry(
            TerminalContextMenuCommand::QuickLook,
            "Quick Look",
            "eye",
            actions.quick_look,
        ),
    ]
}

fn menu_entry(
    command: TerminalContextMenuCommand,
    label: &'static str,
    symbol: &'static str,
    enabled: bool,
) -> MenuEntry<TerminalContextMenuCommand> {
    MenuEntry::action(label, command)
        .disabled(!enabled)
        .icon(move |foreground| {
            Icon::new(symbol)
                .weight(SymbolWeight::Regular)
                .size(px(14.0))
                .color(foreground)
                .into_any_element()
        })
        .debug_selector(format!(
            "terminal-context-menu-row-{}-{}",
            command.debug_name(),
            if enabled { "enabled" } else { "disabled" },
        ))
}

impl TerminalContextMenuCommand {
    const fn debug_name(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::OpenLink => "open-link",
            Self::QuickLook => "quick-look",
        }
    }
}
