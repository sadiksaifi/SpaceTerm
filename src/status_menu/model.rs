//! What the status menu shows and offers, modeled on Apple's menu bar extras: a `SpaceTerm`
//! header with SpaceTerm's own row, SpaceTerm's step when it has one, a `SpaceTerm Core` section
//! with one row per Machine, then Settings and Quit. There is no "Open SpaceTerm". Every status
//! and action comes from the one update view ([`crate::updates::state`]), so the menu says what
//! the title-bar list and Settings say; hosts render this tree natively.
use gpui::SharedString;
use spaceterm_core::update::facts::Platform;

use crate::updates::copy::rows;
use crate::updates::copy::settings as copy;
use crate::updates::machines::{MachineAction, MachineId, MachineLink, MachineUpdate};
use crate::updates::state::{self, DesktopAction, RowView, UpdateView};

/// One person-chosen operation the menu offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// An update surface command: a row's action, or a Start or Stop of this computer's Core.
    Update(state::Command),
    Settings,
    /// Exits the whole client, status item included. Every Core and its sessions keep running.
    Quit,
}

/// The kind of computer a Machine row's symbol shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MachineKind {
    /// This Mac or This Computer.
    Local,
    Mac,
    Linux,
}

/// One item that performs a command.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub(crate) title: SharedString,
    pub(crate) subtitle: Option<SharedString>,
    pub(crate) command: Command,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MachineRow {
    pub(crate) machine: MachineId,
    pub(crate) kind: MachineKind,
    pub(crate) name: SharedString,
    /// The row's versions and short status, or "{running} · {n} Terminal Sessions" when the
    /// versions alone say everything.
    pub(crate) line: String,
    /// The row's status alone, the part a warning colors.
    pub(crate) status: Option<SharedString>,
    /// The row's full sentence, for the tooltip and assistive technology.
    pub(crate) accessibility: SharedString,
    /// What the row leaves out, for its submenu: when the update applies itself and how long
    /// it waited.
    pub(crate) details: Vec<state::Detail>,
    /// An offline Machine offers nothing, so its row dims.
    pub(crate) offline: bool,
    /// The submenu: the row's action first, then Stop or Start for this computer's Core.
    pub(crate) items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Menu {
    /// The item's accessibility label and tooltip.
    pub(crate) summary: String,
    /// An update has waited until Insist, or a Machine is blocked: the item's symbol shows a dot.
    pub(crate) attention: bool,
    /// The header's subtitle: SpaceTerm's versions and short status.
    pub(crate) header: String,
    /// SpaceTerm's row action, such as Relaunch to Update, Download Update or Try Again.
    pub(crate) step: Option<Item>,
    pub(crate) machines: Vec<MachineRow>,
}

/// One row of a flat menu, for status areas whose menus have no subtitles or custom rows.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    allow(dead_code, reason = "only the Linux status area shows a flat menu")
)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FlatRow {
    Text(String),
    Separator,
    Action { title: String, command: Command },
}

impl Menu {
    /// The menu for `view`. `machines` gives each row its order, symbol, Terminal Sessions and
    /// whether this computer's Core runs.
    pub(crate) fn new(view: &UpdateView, machines: &[MachineUpdate]) -> Self {
        let app = view.app_row();
        let header = copy::caption(&app);
        // SpaceTerm's step is its row's action, so a failure offers the same `Try Again` here as
        // in Settings and the title-bar list. What's New stays in the title bar and Help.
        let step = app.action.and_then(|action| {
            if action.command == state::Command::Desktop(DesktopAction::WhatsNew) {
                return None;
            }
            let title = if action.label.as_ref() == rows::DOWNLOAD {
                copy::DOWNLOAD_UPDATE.into()
            } else {
                action.label
            };
            Some(Item {
                title,
                subtitle: action.subtitle,
                command: Command::Update(action.command),
            })
        });
        // Local first, then store order, as every update surface lists them.
        let mut ordered: Vec<&MachineUpdate> = machines.iter().collect();
        ordered.sort_by_key(|update| !update.local);
        let machines = ordered
            .into_iter()
            .filter_map(|update| {
                view.machine_row(update.machine)
                    .map(|row| machine_row(update, row))
            })
            .collect();
        Self {
            summary: copy::summary(&header),
            attention: view.insists(),
            header,
            step,
            machines,
        }
    }

    /// Every command the menu offers, in menu order.
    pub(crate) fn commands(&self) -> impl Iterator<Item = Command> + '_ {
        self.step
            .iter()
            .chain(self.machines.iter().flat_map(|machine| &machine.items))
            .map(|item| item.command.clone())
            .chain([Command::Settings, Command::Quit])
    }

    /// Whether the menu still offers `command`. A host can deliver a row it drew before a change.
    pub(crate) fn offers(&self, command: &Command) -> bool {
        self.commands().any(|offered| offered == *command)
    }

    /// The same menu without submenus or subtitles: SpaceTerm's line and step, the `SpaceTerm
    /// Core` heading with one short line per Machine, then every Machine action, naming its
    /// Machine, then Settings and Quit.
    #[cfg_attr(
        not(any(target_os = "linux", test)),
        allow(dead_code, reason = "only the Linux status area shows a flat menu")
    )]
    pub(crate) fn flat(&self) -> Vec<FlatRow> {
        let mut rows = vec![FlatRow::Text(format!(
            "{} \u{b7} {}",
            copy::SPACETERM,
            self.header
        ))];
        rows.extend(self.step.iter().map(|step| FlatRow::Action {
            title: step.title.to_string(),
            command: step.command.clone(),
        }));
        if !self.machines.is_empty() {
            rows.push(FlatRow::Separator);
            rows.push(FlatRow::Text(copy::CORE_HEADING.to_owned()));
        }
        for machine in &self.machines {
            rows.push(FlatRow::Text(format!(
                "{} \u{b7} {}",
                machine.name, machine.line
            )));
        }
        let actions: Vec<FlatRow> = self
            .machines
            .iter()
            .flat_map(|machine| {
                machine.items.iter().map(|item| FlatRow::Action {
                    title: copy::flat_action(
                        &item.title,
                        &machine.name,
                        machine_action(&item.command),
                    ),
                    command: item.command.clone(),
                })
            })
            .collect();
        if !actions.is_empty() {
            rows.push(FlatRow::Separator);
            rows.extend(actions);
        }
        rows.extend([
            FlatRow::Separator,
            FlatRow::Action {
                title: copy::SETTINGS.to_owned(),
                command: Command::Settings,
            },
            FlatRow::Action {
                title: copy::QUIT.to_owned(),
                command: Command::Quit,
            },
        ]);
        rows
    }
}

fn machine_row(update: &MachineUpdate, row: RowView) -> MachineRow {
    let sessions = update.facts.as_ref().map_or(0, |facts| facts.sessions);
    // A settled Machine's versions say everything, so the extra shows its Terminal Sessions
    // after them.
    let settled = row.status.is_none() && row.target.is_none();
    let line = if settled {
        copy::line(row.version.as_deref(), Some(&copy::sessions(sessions)))
    } else {
        copy::caption(&row)
    };
    let machine = update.machine;
    let mut items: Vec<Item> = row
        .action
        .map(|action| Item {
            title: action.label,
            subtitle: action.subtitle,
            command: Command::Update(action.command),
        })
        .into_iter()
        .collect();
    if update.local {
        match update.link {
            MachineLink::Stopped => items.push(Item {
                title: copy::START_CORE.into(),
                subtitle: None,
                command: Command::Update(state::Command::Machine(machine, MachineAction::Start)),
            }),
            MachineLink::Offline { .. } => {}
            _ => items.push(Item {
                title: copy::STOP_CORE.into(),
                subtitle: copy::ends(sessions).map(Into::into),
                command: Command::Update(state::Command::Machine(machine, MachineAction::Stop)),
            }),
        }
    }
    MachineRow {
        machine,
        kind: kind(update),
        name: row.name,
        line,
        status: row.status,
        accessibility: row.accessibility,
        details: row.details,
        offline: matches!(update.link, MachineLink::Offline { .. }),
        items,
    }
}

/// The Machine action `command` performs, if it performs one.
fn machine_action(command: &Command) -> Option<&MachineAction> {
    match command {
        Command::Update(state::Command::Machine(_, action)) => Some(action),
        _ => None,
    }
}

fn kind(update: &MachineUpdate) -> MachineKind {
    if update.local {
        return MachineKind::Local;
    }
    match update.facts.as_ref().map(|facts| facts.platform) {
        Some(Platform::MacosArm64) => MachineKind::Mac,
        Some(Platform::LinuxX86_64) | None => MachineKind::Linux,
    }
}
