use gpui::prelude::*;
use gpui::px;
use spaceterm_ui::{Icon, IconName, MenuEntry};

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
        menu_entry(TerminalContextMenuCommand::Copy, "Copy", actions.copy).shortcut("⌘C"),
        menu_entry(
            TerminalContextMenuCommand::OpenLink,
            "Open Link",
            actions.open_link,
        ),
        menu_entry(
            TerminalContextMenuCommand::QuickLook,
            "Quick Look",
            actions.quick_look,
        ),
    ]
}

fn command_icon(command: TerminalContextMenuCommand) -> IconName {
    match command {
        TerminalContextMenuCommand::Copy => IconName::Copy,
        TerminalContextMenuCommand::OpenLink => IconName::ExternalLink,
        TerminalContextMenuCommand::QuickLook => IconName::Eye,
    }
}

fn menu_entry(
    command: TerminalContextMenuCommand,
    label: &'static str,
    enabled: bool,
) -> MenuEntry<TerminalContextMenuCommand> {
    let icon = command_icon(command);
    MenuEntry::action(label, command)
        .disabled(!enabled)
        .icon(move |foreground| Icon::new(icon, px(14.0), foreground).into_any_element())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_context_actions_should_use_typed_semantic_icons() {
        assert!(matches!(
            command_icon(TerminalContextMenuCommand::Copy),
            IconName::Copy
        ));
        assert!(matches!(
            command_icon(TerminalContextMenuCommand::OpenLink),
            IconName::ExternalLink
        ));
        assert!(matches!(
            command_icon(TerminalContextMenuCommand::QuickLook),
            IconName::Eye
        ));
    }
}
