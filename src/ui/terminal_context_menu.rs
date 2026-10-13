use spaceterm_ui::MenuEntry;

use crate::terminal::NativeContextActions;
use crate::ui::server_pane_actions::PanePresentationMenuCommand;

/// Builds content-free entries from the Pane owner's offered menu actions.
/// The owner passes Take Over first, then the screen's recovery actions in screen order.
/// Availability is captured for this menu instance. The `ContextMenu::on_activate` callback
/// receives `MenuActivation<PanePresentationMenuCommand>`; its owner revalidates the Pane token
/// and arming before effects. A menu opened before arming must keep Take Over disabled.
/// The owner uses `ContextMenu::open_when_all_disabled` to show unarmed Take Over and freeze
/// the accepted opening's entries before the first popup redraw.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the desktop Pane owner installs the presentation menu"
    )
)]
pub(crate) fn pane_presentation_context_menu_entries(
    actions: &[(PanePresentationMenuCommand, bool)],
) -> Vec<MenuEntry<PanePresentationMenuCommand>> {
    actions
        .iter()
        .map(|&(command, enabled)| {
            let (label, selector) = match command {
                PanePresentationMenuCommand::TakeOver => ("Take Over", "take-over"),
                PanePresentationMenuCommand::Retry => ("Retry", "retry"),
                PanePresentationMenuCommand::RetryConnection => {
                    ("Retry Connection", "retry-connection")
                }
                PanePresentationMenuCommand::ServerSettings => {
                    ("Server Settings…", "server-settings")
                }
            };
            MenuEntry::action(label, command)
                .disabled(!enabled)
                .debug_selector(format!("pane-presentation-menu-{selector}"))
        })
        .collect()
}

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
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::prelude::*;
    use gpui::{Context, FocusHandle, Render, TestAppContext, Window, div, px};
    use spaceterm_ui::a11y_testing::{A11yTree, perform, supports};
    use spaceterm_ui::{ContextMenu, ContextMenuTarget};

    use super::*;

    struct PresentationMenu {
        focus: FocusHandle,
        offered: Vec<(PanePresentationMenuCommand, bool)>,
        selected: Rc<RefCell<Vec<PanePresentationMenuCommand>>>,
    }

    impl Render for PresentationMenu {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let selected = Rc::clone(&self.selected);
            ContextMenu::new(
                "pane-presentation-menu",
                "Pane actions",
                div().w(px(240.0)).h(px(160.0)),
                pane_presentation_context_menu_entries(&self.offered),
            )
            .open_when_all_disabled()
            .keyboard_trigger(&self.focus)
            .target(ContextMenuTarget::new(gpui::accesskit::Role::Group, "Pane"))
            .on_activate(move |activation, _, _| {
                selected.borrow_mut().push(*activation.action());
            })
        }
    }

    #[gpui::test]
    fn desktop_server_takeover_context_menu_only_exposes_its_offered_presentation_commands(
        cx: &mut TestAppContext,
    ) {
        use PanePresentationMenuCommand::{Retry, RetryConnection, ServerSettings, TakeOver};
        use gpui::accesskit::Action;

        cx.update(crate::ui::init).unwrap();
        for (offered, expected, activated, label) in [
            (
                vec![(TakeOver, true)],
                vec!["Take Over"],
                TakeOver,
                "Take Over",
            ),
            (
                vec![(TakeOver, false), (Retry, true)],
                vec!["Take Over", "Retry"],
                Retry,
                "Retry",
            ),
            (
                vec![(TakeOver, false), (ServerSettings, true)],
                vec!["Take Over", "Server Settings…"],
                ServerSettings,
                "Server Settings…",
            ),
            (
                vec![
                    (TakeOver, false),
                    (RetryConnection, true),
                    (ServerSettings, true),
                ],
                vec!["Take Over", "Retry Connection", "Server Settings…"],
                RetryConnection,
                "Retry Connection",
            ),
        ] {
            let selected = Rc::new(RefCell::new(Vec::new()));
            let recorded = Rc::clone(&selected);
            let (_, cx) = cx.add_window_view(move |window, cx| {
                let focus = cx.focus_handle();
                window.activate_window();
                focus.focus(window, cx);
                PresentationMenu {
                    focus,
                    offered,
                    selected: recorded,
                }
            });
            let tree = A11yTree::read(cx);
            perform(cx, tree.node("Pane"), Action::ShowContextMenu);
            let tree = A11yTree::read(cx);
            assert_eq!(
                tree.with_role("MenuItem")
                    .into_iter()
                    .map(|item| item["aria"]["label"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected,
            );
            perform(cx, tree.node(label), Action::Click);
            assert_eq!(*selected.borrow(), [activated]);
        }
    }

    #[gpui::test]
    fn desktop_server_unarmed_takeover_opens_a_disabled_context_menu(cx: &mut TestAppContext) {
        use PanePresentationMenuCommand::TakeOver;
        use gpui::accesskit::Action;
        use gpui::{Modifiers, MouseButton};

        cx.update(crate::ui::init).unwrap();
        let selected = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&selected);
        let (menu, cx) = cx.add_window_view(move |window, cx| {
            window.activate_window();
            PresentationMenu {
                focus: cx.focus_handle(),
                offered: vec![(TakeOver, false)],
                selected: recorded,
            }
        });
        cx.update(|window, cx| menu.read(cx).focus.clone().focus(window, cx));
        cx.run_until_parked();
        for opening in ["shift-f10", "menu", "pointer", "accessibility"] {
            match opening {
                "pointer" => {
                    let position = cx.debug_bounds("Pane actions").unwrap().center();
                    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());
                    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::none());
                }
                "accessibility" => {
                    let tree = A11yTree::read(cx);
                    assert!(supports(tree.node("Pane"), Action::ShowContextMenu));
                    perform(cx, tree.node("Pane"), Action::ShowContextMenu);
                }
                key => cx.simulate_keystrokes(key),
            }
            let tree = A11yTree::read(cx);
            assert_eq!(tree.with_role("Menu").len(), 1, "{opening}");
            assert_eq!(tree.with_role("MenuItem").len(), 1, "{opening}");
            assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);
            assert!(!supports(tree.node("Take Over"), Action::Click));
            perform(cx, tree.node("Take Over"), Action::Click);
            cx.simulate_keystrokes("down enter");
            let position = cx
                .debug_bounds("pane-presentation-menu-take-over")
                .unwrap()
                .center();
            cx.simulate_click(position, Modifiers::none());
            assert!(selected.borrow().is_empty(), "{opening}");
            cx.simulate_keystrokes("escape");
            let tree = A11yTree::read(cx);
            assert!(tree.with_role("Menu").is_empty());
            assert_eq!(tree.focused().unwrap()["aria"]["label"], "Pane");
        }
    }

    #[gpui::test]
    fn desktop_server_takeover_context_menu_keeps_its_opening_availability(
        cx: &mut TestAppContext,
    ) {
        use PanePresentationMenuCommand::{Retry, TakeOver};
        use gpui::accesskit::Action;
        use gpui::{Keystroke, Modifiers};

        cx.update(crate::ui::init).unwrap();
        for arm_before_first_draw in [true, false] {
            for offered in [
                vec![(TakeOver, false)],
                vec![(TakeOver, false), (Retry, true)],
            ] {
                let selected = Rc::new(RefCell::new(Vec::new()));
                let recorded = Rc::clone(&selected);
                let (menu, cx) = cx.add_window_view(move |window, cx| {
                    let focus = cx.focus_handle();
                    window.activate_window();
                    focus.focus(window, cx);
                    PresentationMenu {
                        focus,
                        offered,
                        selected: recorded,
                    }
                });
                cx.update(|window, cx| menu.read(cx).focus.clone().focus(window, cx));
                A11yTree::read(cx);
                cx.update(|window, cx| {
                    assert!(window.dispatch_keystroke(Keystroke::parse("shift-f10").unwrap(), cx));
                    // Do not drain the opening redraw before arming in this interleaving.
                    if arm_before_first_draw {
                        menu.update(cx, |menu, cx| {
                            menu.offered[0].1 = true;
                            cx.notify();
                        });
                    }
                });
                if !arm_before_first_draw {
                    let tree = A11yTree::read(cx);
                    assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);
                    menu.update(cx, |menu, cx| {
                        menu.offered[0].1 = true;
                        cx.notify();
                    });
                }
                let tree = A11yTree::read(cx);
                assert_eq!(tree.node("Take Over")["aria"]["disabled"], true);
                assert!(!supports(tree.node("Take Over"), Action::Click));
                perform(cx, tree.node("Take Over"), Action::Click);
                let position = cx
                    .debug_bounds("pane-presentation-menu-take-over")
                    .unwrap()
                    .center();
                cx.simulate_click(position, Modifiers::none());
                assert!(selected.borrow().is_empty());
                cx.simulate_keystrokes("home enter");
                assert!(!selected.borrow().contains(&TakeOver));

                cx.simulate_keystrokes("escape");
                selected.borrow_mut().clear();
                cx.simulate_keystrokes("shift-f10");
                let tree = A11yTree::read(cx);
                assert_ne!(tree.node("Take Over")["aria"]["disabled"], true);
                perform(cx, tree.node("Take Over"), Action::Click);
                assert_eq!(*selected.borrow(), [TakeOver]);
            }
        }
    }

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
