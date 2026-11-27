//! The Command Line Tool row: whether this build's command is in `~/.local/bin` on the selected
//! Machine, with Install, or Repair and Uninstall in its menu. This computer's command is read
//! whenever Machines comes into view. A paired Machine's command is read and changed over its
//! service lane, and only when the person asks, because opening or selecting in Settings
//! connects to nothing. [`crate::command_line_tool`] owns the launcher policy.
use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Global, SharedString, Task, Window, div};
use spaceterm_ui::{
    AnchoredAlignment, AnchoredPlacement, AnchoredPlacementConfig, Button, ButtonSize,
    ButtonVariant, Icon, IconName, Menu, MenuEntry, ProgressRing, ProgressSize, ProgressState,
};

use super::SettingsWindow;
use super::machine_list::MachineChoice;
use crate::command_line_tool::{CommandLineTool, Launcher, ToolAction, ToolFailure};
use crate::core_context::MachineKey;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_icons::IconRole;
use crate::ui::sidebar_window::form::{CaptionTone, badge};

pub(super) const ACTION_SELECTOR: &str = "settings-command-line-tool-action";
pub(super) const MENU_SELECTOR: &str = "settings-command-line-tool-menu";
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ToolCheck {
    #[default]
    NotChecked,
    Checked(Launcher),
    Failed(ToolFailure),
}

/// The background step the row runs. One runs at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolWork {
    Check,
    Install,
    Repair,
    Uninstall,
}

/// The row's complete presentation, derived from its state alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ToolRowPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) caption: Option<(SharedString, CaptionTone)>,
    /// The action the row's button offers, and its label.
    pub(super) action: Option<(ToolAction, &'static str)>,
    /// The actions the row's menu offers, and their labels. Empty omits the menu.
    pub(super) menu: Vec<(ToolAction, &'static str)>,
    pub(super) working: bool,
}

/// Where the row's command lives, which decides when SpaceTerm reads it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolPlace {
    /// This computer: read whenever Machines comes into view.
    Local,
    /// A paired Machine: read only when the person asks.
    Paired,
}

/// Each Machine's command and what SpaceTerm learned about it in this window.
pub(super) struct CommandLineTools {
    /// The command people type on every Machine. A paired Machine's SpaceTerm Core has this
    /// build's identity, so its command has the same name.
    name: &'static str,
    /// This computer's command. `None` omits the row while This Mac or This Computer is
    /// selected.
    local: Option<Arc<CommandLineTool>>,
    /// Whether this desktop reaches paired Machines' service lanes. Without them, a paired
    /// Machine offers no row.
    paired: bool,
    rows: BTreeMap<MachineKey, CommandLineToolRow>,
}

impl CommandLineTools {
    pub(super) fn new(local: Option<Arc<CommandLineTool>>, paired: bool) -> Self {
        Self {
            name: local.as_ref().map_or_else(
                || crate::application_identity::ApplicationIdentity::current().directory_name(),
                |tool| tool.name(),
            ),
            local,
            paired,
            rows: BTreeMap::new(),
        }
    }

    /// Whether `choice` offers the row.
    pub(super) fn offered(&self, choice: &MachineChoice) -> bool {
        match choice {
            MachineChoice::Local => self.local.is_some(),
            MachineChoice::Direct(_) => self.paired,
        }
    }

    /// What SpaceTerm last learned about `choice`'s command, if it read or changed it.
    pub(super) fn row(&self, choice: &MachineChoice) -> Option<&CommandLineToolRow> {
        self.rows.get(&choice.key())
    }
}

/// The last status of one Machine's command, and the running step.
#[derive(Default)]
pub(super) struct CommandLineToolRow {
    pub(super) check: ToolCheck,
    pub(super) working: Option<ToolWork>,
    /// Why the last Install, Repair or Uninstall didn't complete. The next action clears it.
    pub(super) failure: Option<ToolFailure>,
    /// A service operation or update finished while a step ran, so that step's read may predate
    /// it and the command is read again afterwards.
    read_outdated: bool,
    /// Returns the background result to GPUI. Dropping the window drops it.
    _operation: Option<Task<()>>,
}

/// The presentation of one state.
pub(super) fn present(
    name: &str,
    place: ToolPlace,
    check: ToolCheck,
    working: Option<ToolWork>,
    failure: Option<ToolFailure>,
) -> ToolRowPresentation {
    // A paired Machine's command is read only when the person asks, first and after a failed
    // read.
    let check_action: &[ToolAction] = match place {
        ToolPlace::Local => &[],
        ToolPlace::Paired => &[ToolAction::Check],
    };
    let (state, state_selector, explanation, actions) = match check {
        ToolCheck::NotChecked => (
            "Not checked",
            "settings-command-line-tool-state-not-checked",
            SharedString::new_static(match place {
                ToolPlace::Local => "SpaceTerm checks the command when you open this section.",
                ToolPlace::Paired => "Check this Machine to install or update its command.",
            }),
            check_action,
        ),
        ToolCheck::Failed(failure) => (
            "Unavailable",
            "settings-command-line-tool-state-unavailable",
            failure_text(failure, name),
            check_action,
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
                        "\u{201C}{name}\u{201D} names a SpaceTerm Core build that is no longer installed."
                    ),
                ),
                Launcher::Outdated => (
                    "Out of date",
                    "settings-command-line-tool-state-outdated",
                    format!(
                        "\u{201C}{name}\u{201D} runs a different SpaceTerm Core build than \
                         this Machine\u{2019}s SpaceTerm Core."
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
                    format!("Installed at {DIRECTORY}/{name}."),
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
            (
                state,
                selector,
                SharedString::from(explanation),
                launcher.actions(),
            )
        }
    };
    // Install is the row's button; the actions on a working command wait in its menu.
    let mut action = None;
    let mut menu = Vec::new();
    for &offered in actions {
        match offered {
            ToolAction::Check => {
                let label = match check {
                    ToolCheck::Failed(_) => "Try Again",
                    _ => "Check",
                };
                action = Some((offered, label));
            }
            ToolAction::Install => {
                let label = match check {
                    ToolCheck::Checked(Launcher::Stale | Launcher::Outdated) => "Reinstall",
                    _ => "Install",
                };
                action = Some((offered, label));
            }
            ToolAction::Repair => menu.push((offered, "Repair")),
            ToolAction::Uninstall => menu.push((offered, "Uninstall")),
        }
    }
    // SpaceTerm runs no shell to learn PATH, so the row names the directory PATH must include
    // for a command it would install. An installed command's explanation names its path.
    let ours = matches!(
        check,
        ToolCheck::Checked(Launcher::Missing | Launcher::Stale | Launcher::Outdated)
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
        menu,
        working: working.is_some(),
    }
}

fn failure_text(failure: ToolFailure, name: &str) -> SharedString {
    match failure {
        ToolFailure::Busy => SharedString::new_static(
            "Another SpaceTerm Core operation is running. Try again when it finishes.",
        ),
        ToolFailure::NoCorePackage => SharedString::new_static(
            "This SpaceTerm build doesn\u{2019}t include a SpaceTerm Core package.",
        ),
        ToolFailure::RestartRequired => SharedString::new_static(
            "SpaceTerm was updated or moved after it opened. Quit and reopen SpaceTerm to \
             continue.",
        ),
        ToolFailure::Unverified => {
            SharedString::new_static("Couldn\u{2019}t verify this SpaceTerm Core package.")
        }
        ToolFailure::UnsafeStorage => SharedString::from(format!(
            "{DIRECTORY} or SpaceTerm Core storage has permissions SpaceTerm doesn\u{2019}t accept."
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
        ToolFailure::Unreachable => SharedString::new_static(
            "SpaceTerm couldn\u{2019}t reach this Machine. Check that it\u{2019}s connected, then \
             try again.",
        ),
        ToolFailure::Refused => SharedString::new_static(
            "This Machine didn\u{2019}t accept this device. Reconnect, then try again.",
        ),
        ToolFailure::Unsupported => SharedString::from(format!(
            "Installing \u{201C}{name}\u{201D} from here needs a newer SpaceTerm Core on this \
             Machine. Update it first."
        )),
        ToolFailure::Failed => {
            SharedString::new_static("This Machine couldn\u{2019}t finish the change.")
        }
    }
}

impl SettingsWindow {
    /// The selected Machine's row, or `None` when that Machine offers none.
    pub(super) fn command_line_tool_presentation(&self) -> Option<ToolRowPresentation> {
        let choice = self.selected_machine()?;
        let tools = &self.command_line_tools;
        if !tools.offered(&choice) {
            return None;
        }
        let place = match choice {
            MachineChoice::Local => ToolPlace::Local,
            MachineChoice::Direct(_) => ToolPlace::Paired,
        };
        let row = tools.row(&choice);
        Some(present(
            tools.name,
            place,
            row.map_or(ToolCheck::NotChecked, |row| row.check),
            row.and_then(|row| row.working),
            row.and_then(|row| row.failure),
        ))
    }

    /// Reads this computer's command on the background executor as Machines comes into view. A
    /// running step keeps its place.
    pub(super) fn check_command_line_tool(&mut self, cx: &mut Context<Self>) {
        self.run_command_line_tool(MachineChoice::Local, ToolWork::Check, cx);
    }

    /// Reads this computer's command again after a service operation or update finished.
    /// Repointing the launcher never blocks those, so one that couldn't follow the new server
    /// build shows only here. A row not yet read waits for Machines to come into view.
    pub(super) fn reread_command_line_tool(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.command_line_tools.rows.get_mut(&MachineKey::Local) else {
            return;
        };
        if row.working.is_some() {
            row.read_outdated = true;
        } else if row.check != ToolCheck::NotChecked {
            self.check_command_line_tool(cx);
        }
    }

    /// Runs the selected Machine's action on the background executor.
    pub(super) fn request_command_line_tool(&mut self, action: ToolAction, cx: &mut Context<Self>) {
        let Some(choice) = self.selected_machine() else {
            return;
        };
        let work = match action {
            ToolAction::Check => ToolWork::Check,
            ToolAction::Install => ToolWork::Install,
            ToolAction::Repair => ToolWork::Repair,
            ToolAction::Uninstall => ToolWork::Uninstall,
        };
        self.run_command_line_tool(choice, work, cx);
    }

    /// `choice`'s command: this computer's own, or a paired Machine's over the lane of the
    /// connection SpaceTerm holds to it. `None` for a paired Machine SpaceTerm doesn't reach now.
    fn command_line_tool_for(
        &self,
        choice: &MachineChoice,
        cx: &App,
    ) -> Option<Arc<CommandLineTool>> {
        match choice {
            MachineChoice::Local => self.command_line_tools.local.clone(),
            MachineChoice::Direct(route) => {
                if !self.remote_reachable(route, cx) {
                    return None;
                }
                let (control, _) = self.remote_control(route, cx)?;
                Some(Arc::new(CommandLineTool::new(
                    self.command_line_tools.name,
                    control.command_line_tool(),
                )))
            }
        }
    }

    fn run_command_line_tool(&mut self, choice: MachineChoice, work: ToolWork, cx: &mut Context<Self>) {
        if !self.command_line_tools.offered(&choice) {
            return;
        }
        let tool = self.command_line_tool_for(&choice, cx);
        let key = choice.key();
        let row = self.command_line_tools.rows.entry(key.clone()).or_default();
        if row.working.is_some() {
            return;
        }
        if work != ToolWork::Check {
            row.failure = None;
        }
        let Some(tool) = tool else {
            // Nothing reaches a paired Machine SpaceTerm isn't connected to, so the row says so
            // without trying.
            match work {
                ToolWork::Check => row.check = ToolCheck::Failed(ToolFailure::Unreachable),
                _ => row.failure = Some(ToolFailure::Unreachable),
            }
            cx.notify();
            return;
        };
        row.working = Some(work);
        let result = cx.background_spawn(async move {
            match work {
                ToolWork::Check => tool.status(),
                ToolWork::Install | ToolWork::Repair => tool.install(),
                ToolWork::Uninstall => tool.uninstall(),
            }
        });
        row._operation = Some(cx.spawn(async move |settings, cx| {
            let result = result.await;
            let _ = settings.update(cx, |settings, cx| {
                let Some(row) = settings.command_line_tools.rows.get_mut(&key) else {
                    return;
                };
                row.working = None;
                let mut again = std::mem::take(&mut row.read_outdated);
                match (work, result) {
                    (_, Ok(status)) => row.check = ToolCheck::Checked(status),
                    (ToolWork::Check, Err(failure)) => row.check = ToolCheck::Failed(failure),
                    // A failed change says why and reads the command again: it may have landed
                    // before the failure, or found another program's file meanwhile.
                    (ToolWork::Install | ToolWork::Repair | ToolWork::Uninstall, Err(failure)) => {
                        row.failure = Some(failure);
                        again = true;
                    }
                }
                if again {
                    settings.run_command_line_tool(choice, ToolWork::Check, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The row's state badge, progress while a step runs, its button and its menu.
    pub(super) fn render_command_line_tool(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(presentation) = self.command_line_tool_presentation() else {
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
        let menu = (!presentation.menu.is_empty()).then(|| {
            let entries = presentation
                .menu
                .iter()
                .enumerate()
                .flat_map(|(index, &(action, label))| {
                    // Uninstall sits apart from the step that keeps the command.
                    let separator =
                        (index > 0 && action == ToolAction::Uninstall).then(MenuEntry::separator);
                    let entry = MenuEntry::action(label, action).disabled(presentation.working);
                    separator.into_iter().chain([entry])
                })
                .collect();
            let owner = cx.weak_entity();
            let icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
            Menu::new(
                MENU_SELECTOR,
                SharedString::new_static("More for Command Line Tool"),
                entries,
            )
            .icon_trigger(move |foreground| {
                Icon::new(IconName::Ellipsis, icon_size, foreground).into_any_element()
            })
            .placement(AnchoredPlacementConfig::new(
                AnchoredPlacement::Bottom,
                AnchoredAlignment::End,
            ))
            .debug_selector(MENU_SELECTOR)
            .on_activate(move |activation, _: &mut Window, cx| {
                let action = *activation.action();
                let _ = owner.update(cx, |settings, cx| {
                    settings.request_command_line_tool(action, cx);
                });
            })
        });
        let state_selector = presentation.state_selector;
        let working = self
            .selected_machine()
            .and_then(|choice| self.command_line_tools.row(&choice)?.working);
        let progress = match working {
            Some(ToolWork::Install) => "Installing the command",
            Some(ToolWork::Repair) => "Repairing the command",
            Some(ToolWork::Uninstall) => "Uninstalling the command",
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
            .children(menu)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "spaceterm-development";

    fn checked(launcher: Launcher) -> ToolRowPresentation {
        present(NAME, ToolPlace::Local, ToolCheck::Checked(launcher), None, None)
    }

    #[test]
    fn each_launcher_state_reads_one_way_and_offers_its_actions() {
        let missing = checked(Launcher::Missing);
        assert_eq!(
            (missing.state, missing.action, missing.menu),
            (
                "Not installed",
                Some((ToolAction::Install, "Install")),
                vec![]
            )
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
        let blocked = checked(Launcher::Blocked);
        assert_eq!(
            (blocked.state, blocked.action, blocked.menu, blocked.caption),
            ("Blocked", None, vec![], None)
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
                (other.state, other.action, other.menu, other.caption),
                (state, None, vec![], None)
            );
            assert!(
                other
                    .explanation
                    .contains("~/.local/bin/spaceterm-development")
            );
        }
    }

    #[test]
    fn an_installed_command_names_its_path_and_offers_repair_and_uninstall_in_its_menu() {
        let installed = checked(Launcher::Installed);
        assert_eq!(
            (
                installed.state,
                installed.explanation.as_ref(),
                installed.caption,
                installed.action,
                installed.menu
            ),
            (
                "Installed",
                "Installed at ~/.local/bin/spaceterm-development.",
                None,
                None,
                vec![
                    (ToolAction::Repair, "Repair"),
                    (ToolAction::Uninstall, "Uninstall")
                ]
            )
        );
    }

    #[test]
    fn the_row_names_the_directory_path_must_include_without_claiming_it_does() {
        for launcher in [Launcher::Missing, Launcher::Stale, Launcher::Outdated] {
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
            ToolPlace::Local,
            ToolCheck::Checked(Launcher::Missing),
            None,
            Some(ToolFailure::NoCorePackage),
        );
        assert_eq!(
            refused.caption,
            Some((
                SharedString::new_static(
                    "This SpaceTerm build doesn\u{2019}t include a SpaceTerm Core package."
                ),
                CaptionTone::Error
            ))
        );
        let unreadable = present(
            NAME,
            ToolPlace::Local,
            ToolCheck::Failed(ToolFailure::UnsafeStorage),
            None,
            None,
        );
        assert_eq!(
            (unreadable.state, unreadable.action, unreadable.menu),
            ("Unavailable", None, vec![])
        );
        let checking = present(
            NAME,
            ToolPlace::Local,
            ToolCheck::NotChecked,
            Some(ToolWork::Check),
            None,
        );
        assert_eq!((checking.state, checking.working), ("Not checked", true));
    }
}

#[cfg(test)]
#[path = "command_line_tool_tests.rs"]
mod window_tests;
