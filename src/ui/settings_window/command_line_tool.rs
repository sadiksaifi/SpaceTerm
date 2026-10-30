//! The Command Line Tool row: whether this build's command is in `~/.local/bin`, with Install or
//! a way to show it. The row belongs to this computer, so it ignores the server picker.
//! [`crate::command_line_tool`] owns the launcher policy.
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Global, SharedString, Task, Window, div};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, ProgressRing, ProgressSize, ProgressState};

use super::SettingsWindow;
use crate::command_line_tool::{CommandLineTool, Launcher, ToolAction, ToolFailure};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{CaptionTone, badge};

pub(super) const ACTION_SELECTOR: &str = "settings-command-line-tool-action";
pub(super) const PROGRESS_SELECTOR: &str = "settings-command-line-tool-progress";
/// Where the launcher lives on every supported desktop, as people type it.
const DIRECTORY: &str = "~/.local/bin";

/// The tool Settings windows present.
struct CommandLineToolAccess(Arc<CommandLineTool>);

impl Global for CommandLineToolAccess {}

/// Offers the Command Line Tool row in Settings windows opened from now on.
pub(crate) fn configure_command_line_tool(tool: Arc<CommandLineTool>, cx: &mut App) {
    cx.set_global(CommandLineToolAccess(tool));
}

/// The tool a new Settings window presents. `None` omits the row.
pub(super) fn offered(cx: &App) -> Option<Arc<CommandLineTool>> {
    Some(Arc::clone(&cx.try_global::<CommandLineToolAccess>()?.0))
}

/// What SpaceTerm last learned about the command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolCheck {
    NotChecked,
    Checked(Launcher),
    Failed(ToolFailure),
}

/// The background step the row runs. One runs at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolWork {
    Check,
    Install,
}

/// The row's complete presentation, derived from its state alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ToolRowPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) caption: Option<(SharedString, CaptionTone)>,
    /// The one action the row offers, and its label.
    pub(super) action: Option<(ToolAction, &'static str)>,
    pub(super) working: bool,
}

/// Owns the tool, the last status, and the running step.
pub(super) struct CommandLineToolRow {
    tool: Arc<CommandLineTool>,
    pub(super) check: ToolCheck,
    pub(super) working: Option<ToolWork>,
    /// Why the last Install or reveal didn't complete. The next action clears it.
    pub(super) failure: Option<ToolFailure>,
    /// A service operation or update finished while a step ran, so that step's read may predate
    /// it and the command is read again afterwards.
    read_outdated: bool,
    /// Returns the background result to GPUI. Dropping the window drops it.
    _operation: Option<Task<()>>,
    /// Waits for the file manager's answer to a reveal.
    _reveal: Option<Task<()>>,
}

impl CommandLineToolRow {
    pub(super) fn new(tool: Arc<CommandLineTool>) -> Self {
        Self {
            tool,
            check: ToolCheck::NotChecked,
            working: None,
            failure: None,
            read_outdated: false,
            _operation: None,
            _reveal: None,
        }
    }

    pub(super) fn presentation(&self, reveal: &'static str) -> ToolRowPresentation {
        present(
            self.tool.name(),
            self.check,
            self.working,
            self.failure,
            reveal,
        )
    }
}

/// The presentation of one state. `reveal` labels the action that shows the command in the
/// desktop's file manager.
pub(super) fn present(
    name: &str,
    check: ToolCheck,
    working: Option<ToolWork>,
    failure: Option<ToolFailure>,
    reveal: &'static str,
) -> ToolRowPresentation {
    let (state, state_selector, explanation, action) = match check {
        ToolCheck::NotChecked => (
            "Not checked",
            "settings-command-line-tool-state-not-checked",
            SharedString::new_static("SpaceTerm checks the command when you open this section."),
            None,
        ),
        ToolCheck::Failed(failure) => (
            "Unavailable",
            "settings-command-line-tool-state-unavailable",
            failure_text(failure, name),
            None,
        ),
        ToolCheck::Checked(launcher) => {
            let (state, selector, explanation) = match launcher {
                Launcher::Missing => (
                    "Not installed",
                    "settings-command-line-tool-state-missing",
                    format!(
                        "Install \u{201C}{name}\u{201D} in {DIRECTORY} to use SpaceTerm from any \
                         terminal."
                    ),
                ),
                Launcher::Stale => (
                    "Broken",
                    "settings-command-line-tool-state-stale",
                    format!(
                        "\u{201C}{name}\u{201D} names a server build that is no longer installed."
                    ),
                ),
                Launcher::Outdated => (
                    "Out of date",
                    "settings-command-line-tool-state-outdated",
                    format!(
                        "\u{201C}{name}\u{201D} runs a different server build than this \
                         computer\u{2019}s server."
                    ),
                ),
                Launcher::Blocked => (
                    "Blocked",
                    "settings-command-line-tool-state-blocked",
                    format!(
                        "Something else is using SpaceTerm\u{2019}s temporary name \
                         {DIRECTORY}/.launcher-{name}. Move it to update \u{201C}{name}\u{201D}."
                    ),
                ),
                Launcher::Installed => (
                    "Installed",
                    "settings-command-line-tool-state-installed",
                    format!("Run \u{201C}{name}\u{201D} in any terminal."),
                ),
                Launcher::OtherLink => (
                    "Other link",
                    "settings-command-line-tool-state-other-link",
                    format!(
                        "Another program\u{2019}s link is at {DIRECTORY}/{name}. SpaceTerm leaves \
                         it in place."
                    ),
                ),
                Launcher::OtherFile => (
                    "Other file",
                    "settings-command-line-tool-state-other-file",
                    format!(
                        "A file SpaceTerm didn\u{2019}t create is at {DIRECTORY}/{name}. \
                         SpaceTerm leaves it in place."
                    ),
                ),
            };
            let action = launcher.action();
            let label = match (action, launcher) {
                (ToolAction::Install, Launcher::Stale | Launcher::Outdated) => "Reinstall",
                (ToolAction::Install, _) => "Install",
                (ToolAction::Reveal, _) => reveal,
            };
            (
                state,
                selector,
                SharedString::from(explanation),
                Some((action, label)),
            )
        }
    };
    // SpaceTerm runs no shell to learn PATH, so the row names the directory PATH must include
    // for a command SpaceTerm owns or would install.
    let ours = matches!(
        check,
        ToolCheck::Checked(
            Launcher::Missing | Launcher::Stale | Launcher::Outdated | Launcher::Installed
        )
    );
    let caption = match failure {
        Some(failure) => Some((failure_text(failure, name), CaptionTone::Error)),
        None if ours => Some((
            SharedString::from(format!(
                "Terminals run \u{201C}{name}\u{201D} by name when {DIRECTORY} is on PATH."
            )),
            CaptionTone::Guidance,
        )),
        None => None,
    };
    ToolRowPresentation {
        state,
        state_selector,
        explanation,
        caption,
        action,
        working: working.is_some(),
    }
}

fn failure_text(failure: ToolFailure, name: &str) -> SharedString {
    match failure {
        ToolFailure::Busy => SharedString::new_static(
            "Another server operation is running. Try again when it finishes.",
        ),
        ToolFailure::NoServerPackage => SharedString::new_static(
            "This SpaceTerm build doesn\u{2019}t include a server package.",
        ),
        ToolFailure::RestartRequired => SharedString::new_static(
            "SpaceTerm was updated or moved after it opened. Quit and reopen SpaceTerm to \
             continue.",
        ),
        ToolFailure::Unverified => {
            SharedString::new_static("Couldn\u{2019}t verify this server package.")
        }
        ToolFailure::UnsafeStorage => SharedString::from(format!(
            "{DIRECTORY} or server storage has permissions SpaceTerm doesn\u{2019}t accept."
        )),
        ToolFailure::StorageUnavailable => SharedString::from(format!(
            "SpaceTerm couldn\u{2019}t read or write {DIRECTORY}."
        )),
        ToolFailure::Occupied => SharedString::from(format!(
            "Another program\u{2019}s file is at {DIRECTORY}/{name}. SpaceTerm left it in place."
        )),
        ToolFailure::Blocked => SharedString::from(format!(
            "Something else is using SpaceTerm\u{2019}s temporary name {DIRECTORY}/.launcher-{name}."
        )),
        ToolFailure::RevealUnavailable => {
            SharedString::new_static("The file manager couldn\u{2019}t show the command.")
        }
    }
}

impl SettingsWindow {
    pub(super) fn command_line_tool_presentation(&self, cx: &App) -> Option<ToolRowPresentation> {
        let reveal = crate::desktop_profile::DesktopPresentation::get(cx)
            .wording()
            .reveal_file;
        self.command_line_tool
            .as_ref()
            .map(|row| row.presentation(reveal))
    }

    /// Reads the command on the background executor as Servers comes into view. A running step
    /// keeps its place.
    pub(super) fn check_command_line_tool(&mut self, cx: &mut Context<Self>) {
        self.run_command_line_tool(ToolWork::Check, cx);
    }

    /// Reads the command again after a service operation or update finished. Repointing the
    /// launcher never blocks those, so one that couldn't follow the new server build shows only
    /// here. A row not yet read waits for Servers to come into view.
    pub(super) fn reread_command_line_tool(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.command_line_tool.as_mut() else {
            return;
        };
        if row.working.is_some() {
            row.read_outdated = true;
        } else if row.check != ToolCheck::NotChecked {
            self.check_command_line_tool(cx);
        }
    }

    /// Runs the row's action. Install runs on the background executor; showing the command asks
    /// the file manager, whose answer arrives later.
    pub(super) fn request_command_line_tool(&mut self, action: ToolAction, cx: &mut Context<Self>) {
        let Some(row) = self.command_line_tool.as_mut() else {
            return;
        };
        if row.working.is_some() {
            return;
        }
        match action {
            ToolAction::Install => self.run_command_line_tool(ToolWork::Install, cx),
            ToolAction::Reveal => {
                let ToolCheck::Checked(launcher) = row.check else {
                    return;
                };
                row.failure = None;
                let shown = row.tool.reveal(launcher);
                row._reveal = Some(cx.spawn(async move |settings, cx| {
                    let shown = shown.await;
                    let _ = settings.update(cx, |settings, cx| {
                        if let Some(row) = settings.command_line_tool.as_mut() {
                            row.failure = shown.err();
                        }
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        }
    }

    fn run_command_line_tool(&mut self, work: ToolWork, cx: &mut Context<Self>) {
        let Some(row) = self.command_line_tool.as_mut() else {
            return;
        };
        if row.working.is_some() {
            return;
        }
        row.working = Some(work);
        if work == ToolWork::Install {
            row.failure = None;
        }
        let tool = Arc::clone(&row.tool);
        let result = cx.background_spawn(async move {
            match work {
                ToolWork::Check => tool.status(),
                ToolWork::Install => tool.install(),
            }
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                let Some(row) = settings.command_line_tool.as_mut() else {
                    return;
                };
                row.working = None;
                let mut again = std::mem::take(&mut row.read_outdated);
                match (work, result) {
                    (_, Ok(status)) => row.check = ToolCheck::Checked(status),
                    (ToolWork::Check, Err(failure)) => row.check = ToolCheck::Failed(failure),
                    // A refused Install leaves the last status in place and says why; another
                    // program's file found meanwhile is read again, so the row shows it.
                    (ToolWork::Install, Err(failure)) => {
                        row.failure = Some(failure);
                        again |= matches!(failure, ToolFailure::Occupied | ToolFailure::Blocked);
                    }
                }
                if again {
                    settings.check_command_line_tool(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The row's state badge, progress while a step runs, and its one action.
    pub(super) fn render_command_line_tool(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(presentation) = self.command_line_tool_presentation(cx) else {
            return div().into_any_element();
        };
        let owner = cx.weak_entity();
        let button = presentation.action.map(|(action, label)| {
            Button::new(ACTION_SELECTOR, label)
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Regular)
                .disabled(presentation.working)
                .tab_stop(true)
                .debug_selector(ACTION_SELECTOR)
                .on_activate(move |_, _: &mut Window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.request_command_line_tool(action, cx);
                    });
                })
        });
        let state_selector = presentation.state_selector;
        let progress = match self.command_line_tool.as_ref().and_then(|row| row.working) {
            Some(ToolWork::Install) => "Installing the command",
            _ => "Checking the command",
        };
        div()
            .debug_selector(|| "settings-row-command-line-tool-control".to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .debug_selector(move || state_selector.to_owned())
                    .child(badge(state_selector, presentation.state, appearance)),
            )
            .when(presentation.working, |row| {
                row.child(
                    ProgressRing::new(PROGRESS_SELECTOR, progress, ProgressState::Indeterminate)
                        .size(ProgressSize::Compact)
                        .debug_selector(PROGRESS_SELECTOR),
                )
            })
            .children(button)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "spaceterm-development";

    fn checked(launcher: Launcher) -> ToolRowPresentation {
        present(
            NAME,
            ToolCheck::Checked(launcher),
            None,
            None,
            "Show in Finder",
        )
    }

    #[test]
    fn each_launcher_state_reads_one_way_and_offers_one_action() {
        let missing = checked(Launcher::Missing);
        assert_eq!(
            (missing.state, missing.action),
            ("Not installed", Some((ToolAction::Install, "Install")))
        );
        let stale = checked(Launcher::Stale);
        assert_eq!(
            (stale.state, stale.action),
            ("Broken", Some((ToolAction::Install, "Reinstall")))
        );
        let outdated = checked(Launcher::Outdated);
        assert_eq!(
            (outdated.state, outdated.action),
            ("Out of date", Some((ToolAction::Install, "Reinstall")))
        );
        let installed = checked(Launcher::Installed);
        assert_eq!(
            (
                installed.state,
                installed.action,
                installed.explanation.as_ref()
            ),
            (
                "Installed",
                Some((ToolAction::Reveal, "Show in Finder")),
                "Run \u{201C}spaceterm-development\u{201D} in any terminal."
            )
        );
        let blocked = checked(Launcher::Blocked);
        assert_eq!(
            (blocked.state, blocked.action, blocked.caption),
            (
                "Blocked",
                Some((ToolAction::Reveal, "Show in Finder")),
                None
            )
        );
        assert!(
            blocked
                .explanation
                .contains("temporary name ~/.local/bin/.launcher-spaceterm-development")
        );
        for (launcher, state) in [
            (Launcher::OtherLink, "Other link"),
            (Launcher::OtherFile, "Other file"),
        ] {
            let other = checked(launcher);
            assert_eq!(
                (other.state, other.action, other.caption),
                (state, Some((ToolAction::Reveal, "Show in Finder")), None)
            );
            assert!(
                other
                    .explanation
                    .contains("~/.local/bin/spaceterm-development")
            );
        }
    }

    #[test]
    fn the_row_names_the_directory_path_must_include_without_claiming_it_does() {
        for launcher in [
            Launcher::Missing,
            Launcher::Stale,
            Launcher::Outdated,
            Launcher::Installed,
        ] {
            assert_eq!(
                checked(launcher).caption,
                Some((
                    SharedString::new_static(
                        "Terminals run \u{201C}spaceterm-development\u{201D} by name when \
                         ~/.local/bin is on PATH."
                    ),
                    CaptionTone::Guidance
                ))
            );
        }
    }

    #[test]
    fn a_failed_action_replaces_the_path_note_and_a_failed_read_offers_nothing() {
        let refused = present(
            NAME,
            ToolCheck::Checked(Launcher::Missing),
            None,
            Some(ToolFailure::NoServerPackage),
            "Show in Finder",
        );
        assert_eq!(
            refused.caption,
            Some((
                SharedString::new_static(
                    "This SpaceTerm build doesn\u{2019}t include a server package."
                ),
                CaptionTone::Error
            ))
        );
        let unreadable = present(
            NAME,
            ToolCheck::Failed(ToolFailure::UnsafeStorage),
            None,
            None,
            "Show in Finder",
        );
        assert_eq!((unreadable.state, unreadable.action), ("Unavailable", None));
        let checking = present(
            NAME,
            ToolCheck::NotChecked,
            Some(ToolWork::Check),
            None,
            "Show in Finder",
        );
        assert_eq!((checking.state, checking.working), ("Not checked", true));
    }
}

#[cfg(test)]
#[path = "command_line_tool_tests.rs"]
mod window_tests;
