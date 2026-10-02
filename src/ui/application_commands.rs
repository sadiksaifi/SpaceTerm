//! Application commands reuse the native menu model and the focused window's action routes.
use std::collections::HashMap;

use gpui::{App, AppContext, Context, Entity, Global, Subscription, Window, WindowId, actions};
use spaceterm_ui::{
    Alert, AlertOutcome, CommandPalette, CommandPaletteAccessory, CommandPaletteEvent,
    CommandPaletteItem, ModalAction, ModalActionEmphasis, ModalActionRole, ModalId,
};

use crate::application_identity::ApplicationIdentity;
use crate::platform::application_menu::{ApplicationMenuCommand, ApplicationMenuError};
use crate::platform::application_menu_model::{ABOUT_DESCRIPTION, HELP_URL, application_commands};

actions!(application, [OpenApplicationCommands]);

struct WindowCommands {
    palette: Entity<CommandPalette<usize>>,
    _events: Subscription,
}

#[derive(Default)]
struct ApplicationCommands {
    windows: HashMap<WindowId, WindowCommands>,
}
impl Global for ApplicationCommands {}

pub(crate) fn install(cx: &mut App) {
    if cx.has_global::<ApplicationCommands>() {
        return;
    }
    cx.set_global(ApplicationCommands::default());
    cx.on_action(|_: &OpenApplicationCommands, cx| {
        let Some(handle) = cx.active_window() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| open(window, cx));
        });
    });
    cx.on_window_closed(|cx, id| {
        cx.global_mut::<ApplicationCommands>().windows.remove(&id);
    })
    .detach();
}

pub(crate) fn layer(window: &Window, cx: &App) -> Option<Entity<CommandPalette<usize>>> {
    cx.try_global::<ApplicationCommands>()?
        .windows
        .get(&window.window_handle().window_id())
        .map(|entry| entry.palette.clone())
}

fn open(window: &mut Window, cx: &mut App) {
    if spaceterm_ui::window_modal_is_open(window, cx) {
        return;
    }
    if let Some(palette) = layer(window, cx)
        && palette.read(cx).is_open()
    {
        palette.update(cx, |palette, cx| palette.focus_editor(window, cx));
        return;
    }
    let commands = application_commands(ApplicationIdentity::current().display_name());
    let available = window.available_actions(cx);
    let items = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let mut item = CommandPaletteItem::new(index, command.label.clone())
                .disabled(
                    !available
                        .iter()
                        .any(|action| action.partial_eq(command.action.as_ref())),
                )
                .debug_selector(format!("application-command-{index}"));
            if let Some(shortcut) = crate::desktop_profile::DesktopPresentation::get(cx)
                .shortcut(command.action.as_ref())
            {
                item = item.trailing(CommandPaletteAccessory::Shortcut(shortcut));
            }
            item
        })
        .collect();
    let palette =
        cx.new(|cx| CommandPalette::new("Search application commands", items, window, cx));
    let handle = window.window_handle();
    let events = cx.subscribe(&palette, move |_, event, cx| {
        if let CommandPaletteEvent::Activated(activation) = event
            && let Some(command) = commands.get(*activation.item_id())
        {
            let action = command.action.boxed_clone();
            // The palette restores its predecessor's focus before emitting activation.
            // Defer until that focus has a complete dispatch tree again.
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| window.dispatch_action(action, cx));
            });
        }
    });
    cx.global_mut::<ApplicationCommands>().windows.insert(
        handle.window_id(),
        WindowCommands {
            palette: palette.clone(),
            _events: events,
        },
    );
    palette.update(cx, |palette, cx| {
        palette.open(window, cx);
    });
    window.refresh();
}

pub(crate) fn perform(
    command: ApplicationMenuCommand,
    cx: &mut App,
) -> Result<(), ApplicationMenuError> {
    match command {
        ApplicationMenuCommand::OpenHelp => {
            cx.open_url(HELP_URL);
            Ok(())
        }
        ApplicationMenuCommand::BringAllWindowsToFront => Err(ApplicationMenuError::Unavailable),
        ApplicationMenuCommand::ZoomActiveWindow => {
            let handle = cx
                .active_window()
                .ok_or(ApplicationMenuError::MissingActiveWindow)?;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.zoom_window());
            });
            Ok(())
        }
        ApplicationMenuCommand::ShowAbout => {
            let handle = cx
                .active_window()
                .ok_or(ApplicationMenuError::MissingActiveWindow)?;
            cx.defer(move |cx| {
                let result = handle.update(cx, |root, window, cx| {
                    // A retained GPUI entity owns Modal presentation and window-close cleanup.
                    if let Ok(manager) = root.clone().downcast::<super::WorkspaceManager>() {
                        manager.update(cx, |_, cx| present_about(window, cx))
                    } else if let Ok(settings) =
                        root.downcast::<super::settings_window::SettingsWindow>()
                    {
                        settings.update(cx, |_, cx| present_about(window, cx))
                    } else {
                        Err(ApplicationMenuError::MissingActiveWindow)
                    }
                });
                if !matches!(result, Ok(Ok(()))) {
                    eprintln!("failed to present the application About dialog");
                }
            });
            Ok(())
        }
    }
}

fn present_about<T: 'static>(
    window: &Window,
    cx: &mut Context<T>,
) -> Result<(), ApplicationMenuError> {
    about_alert()
        .present(window, cx, |result, cx| {
            if matches!(
                result,
                AlertOutcome::Activated {
                    action_id: AboutAction::Help,
                    ..
                }
            ) {
                cx.open_url(HELP_URL);
            }
        })
        .map(|_| ())
        .map_err(|_| ApplicationMenuError::Rejected)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AboutAction {
    Close,
    Help,
}

fn about_alert() -> Alert<AboutAction> {
    let name = ApplicationIdentity::current().display_name();
    Alert::new(
        ModalId::new("application-about"),
        format!("About {name}"),
        name,
        format!(
            "Version {}\n\n{ABOUT_DESCRIPTION}",
            env!("SPACETERM_VERSION")
        ),
        vec![
            ModalAction::new(
                AboutAction::Close,
                "OK",
                ModalActionRole::Cancel,
                "application-about-close",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
        ],
    )
    .help_action(ModalAction::new(
        AboutAction::Help,
        "Help",
        ModalActionRole::Help,
        "application-about-help",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        FocusHandle, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        TestAppContext, div,
    };
    use spaceterm_ui::ModalLayer;

    struct CommandsWindow {
        focus: FocusHandle,
        invoked: Vec<&'static str>,
    }
    impl Render for CommandsWindow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.focus)
                    .on_action(cx.listener(
                        |view, _: &crate::app::ShowAboutApplication, window, _| {
                            assert!(view.focus.is_focused(window));
                            view.invoked.push("About");
                        },
                    ))
                    .on_action(cx.listener(
                        |view, _: &crate::app::OpenApplicationHelp, window, _| {
                            assert!(view.focus.is_focused(window));
                            view.invoked.push("Help");
                        },
                    ))
                    .on_action(cx.listener(
                        |view, _: &crate::ui::updates::OpenReleaseNotes, window, _| {
                            assert!(view.focus.is_focused(window));
                            view.invoked.push("Release Notes");
                        },
                    ))
                    .on_action(cx.listener(
                        |view, _: &crate::ui::ExportTerminalDiagnostics, window, _| {
                            assert!(view.focus.is_focused(window));
                            view.invoked.push("Diagnostics");
                        },
                    )),
            )
            .transient(div().absolute().inset_0().children(layer(window, cx)))
        }
    }

    #[gpui::test]
    fn about_presents_help_and_closes_with_return_or_escape(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).unwrap();
        let (root, cx) = cx.add_window_view(|window, cx| {
            window.activate_window();
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            CommandsWindow {
                focus,
                invoked: Vec::new(),
            }
        });
        cx.run_until_parked();
        for key in ["enter", "escape"] {
            root.update_in(cx, |_, window, cx| present_about(window, cx))
                .expect("About must present a valid acknowledgement");
            cx.run_until_parked();
            assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
            assert!(
                cx.debug_bounds("modal-action-application-about-close")
                    .is_some()
            );
            assert!(
                cx.debug_bounds("modal-action-application-about-help")
                    .is_some()
            );
            cx.simulate_keystrokes(key);
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).unwrap(),
            });
            cx.run_until_parked();
            assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
            assert!(cx.update(|window, cx| root.read(cx).focus.is_focused(window)));
        }
    }

    #[gpui::test]
    fn application_palette_dispatches_to_its_original_focus(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).unwrap();
        cx.update(install);
        let (root, cx) = cx.add_window_view(|window, cx| {
            window.activate_window();
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            CommandsWindow {
                focus,
                invoked: Vec::new(),
            }
        });
        cx.run_until_parked();
        for query in ["About", "Help", "Release Notes", "Diagnostics"] {
            cx.update(|window, cx| window.dispatch_action(Box::new(OpenApplicationCommands), cx));
            cx.run_until_parked();
            assert!(cx.update(|window, cx| layer(window, cx).unwrap().read(cx).is_open()));
            cx.simulate_input(query);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(!cx.update(|window, cx| layer(window, cx).unwrap().read(cx).is_open()));
            assert_eq!(
                root.read_with(cx, |root, _| root.invoked.last().copied()),
                Some(query)
            );
        }
        assert_eq!(
            root.read_with(cx, |root, _| root.invoked.clone()),
            ["About", "Help", "Release Notes", "Diagnostics"]
        );
    }
}
