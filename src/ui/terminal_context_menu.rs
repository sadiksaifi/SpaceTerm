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

#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalContextPresentation {
    copy_shortcut: Option<gpui::SharedString>,
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
        menu_entry(
            TerminalContextMenuCommand::Copy,
            "Copy",
            actions.copy,
            presentation.copy_shortcut,
        ),
        menu_entry(
            TerminalContextMenuCommand::Paste,
            "Paste",
            true,
            paste_shortcut,
        ),
        menu_entry(
            TerminalContextMenuCommand::Find,
            "Find",
            true,
            find_shortcut,
        ),
        MenuEntry::separator(),
        menu_entry(
            TerminalContextMenuCommand::OpenLink,
            "Open Link",
            actions.open_link,
            None,
        ),
        menu_entry(
            TerminalContextMenuCommand::FilePreview,
            presentation.file_preview_label,
            actions.file_preview,
            None,
        ),
    ]
}

fn menu_entry(
    command: TerminalContextMenuCommand,
    label: &'static str,
    enabled: bool,
    shortcut: Option<gpui::SharedString>,
) -> MenuEntry<TerminalContextMenuCommand> {
    let entry = MenuEntry::action(label, command)
        .disabled(!enabled)
        .debug_selector(format!(
            "terminal-context-menu-row-{}-{}",
            command.debug_name(),
            if enabled { "enabled" } else { "disabled" },
        ));
    match shortcut {
        Some(shortcut) => entry.shortcut(shortcut),
        None => entry,
    }
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
    fn terminal_context_presentation_should_resolve_host_neutral_profile_values() {
        assert_eq!(
            terminal_context_presentation(&crate::desktop_profile::testing_presentation()),
            TerminalContextPresentation {
                copy_shortcut: Some("Primary+C".into()),
                file_preview_label: "Preview File",
            }
        );
    }
}
