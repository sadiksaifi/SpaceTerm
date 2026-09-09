use gpui::prelude::*;
use gpui::px;
use spaceterm_ui::{Icon, IconName, MenuEntry};

use crate::terminal::NativeContextActions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalContextMenuCommand {
    Copy,
    Paste,
    Find,
    OpenLink,
    FilePreview,
    PinDirectory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalContextPresentation {
    copy_shortcut: &'static str,
    file_preview_label: &'static str,
}

fn terminal_context_presentation(
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> TerminalContextPresentation {
    TerminalContextPresentation {
        copy_shortcut: presentation.shortcut(&spaceterm_ui::EditCopy),
        file_preview_label: presentation.wording().file_preview,
    }
}

pub(crate) fn terminal_context_menu_entries(
    actions: NativeContextActions,
    pin_enabled: bool,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<MenuEntry<TerminalContextMenuCommand>> {
    let paste_shortcut = presentation.shortcut(&spaceterm_ui::EditPaste);
    let find_shortcut = presentation.shortcut(&crate::ui::OpenTerminalFind);
    let presentation = terminal_context_presentation(presentation);
    vec![
        menu_entry(TerminalContextMenuCommand::Copy, "Copy", actions.copy)
            .shortcut(presentation.copy_shortcut),
        menu_entry(TerminalContextMenuCommand::Paste, "Paste", true).shortcut(paste_shortcut),
        menu_entry(TerminalContextMenuCommand::Find, "Find", true).shortcut(find_shortcut),
        MenuEntry::separator(),
        menu_entry(
            TerminalContextMenuCommand::PinDirectory,
            "Pin Workspace to This Directory",
            pin_enabled,
        ),
        MenuEntry::separator(),
        menu_entry(
            TerminalContextMenuCommand::OpenLink,
            "Open Link",
            actions.open_link,
        ),
        menu_entry(
            TerminalContextMenuCommand::FilePreview,
            presentation.file_preview_label,
            actions.file_preview,
        ),
    ]
}

fn command_icon(command: TerminalContextMenuCommand) -> IconName {
    match command {
        TerminalContextMenuCommand::Copy => IconName::Copy,
        TerminalContextMenuCommand::Paste => IconName::Clipboard,
        TerminalContextMenuCommand::Find => IconName::Search,
        TerminalContextMenuCommand::OpenLink => IconName::ExternalLink,
        TerminalContextMenuCommand::FilePreview => IconName::Eye,
        TerminalContextMenuCommand::PinDirectory => IconName::Pin,
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
            Self::Paste => "paste",
            Self::Find => "find",
            Self::OpenLink => "open-link",
            Self::FilePreview => "file-preview",
            Self::PinDirectory => "pin-directory",
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
            command_icon(TerminalContextMenuCommand::FilePreview),
            IconName::Eye
        ));
    }

    #[test]
    fn terminal_surface_should_use_host_neutral_profile_presentation() {
        assert_eq!(
            terminal_context_presentation(&crate::desktop_profile::testing_presentation()),
            TerminalContextPresentation {
                copy_shortcut: "Primary+C",
                file_preview_label: "Preview File",
            }
        );
    }
}
