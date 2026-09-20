use spaceterm_ui::MenuEntry;

use crate::terminal::NativeContextActions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalContextMenuCommand {
    Copy,
    Paste,
    Find,
    OpenLink,
    FilePreview,
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

/// Ordinary editing commands use labels and shortcuts without decorative icons.
pub(crate) fn terminal_context_menu_entries(
    actions: NativeContextActions,
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

fn menu_entry(
    command: TerminalContextMenuCommand,
    label: &'static str,
    enabled: bool,
) -> MenuEntry<TerminalContextMenuCommand> {
    MenuEntry::action(label, command)
        .disabled(!enabled)
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
