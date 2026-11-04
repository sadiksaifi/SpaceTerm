//! What the status menu shows and offers, modeled on Apple's menu bar extras: a header with the
//! desktop's own status, a Machines section with one row per Machine, Recent Updates, the actions
//! that apply now, then Settings and Quit. There is no "Open SpaceTerm". Each row's wording comes
//! from [`crate::updates::presentation`]; hosts render this tree natively.
use crate::updates::machines::{MachineId, MachineUpdate};
use crate::updates::presentation::{self, ExtraHeader, ExtraItem, ExtraRow, MachineState, Tone};
use spaceterm_core::update::facts::Platform;

pub(crate) const HEADER_TITLE: &str = "SpaceTerm";
pub(crate) const SETTINGS_TITLE: &str = "SpaceTerm Settings\u{2026}";
pub(crate) const QUIT_TITLE: &str = "Quit SpaceTerm";

/// One person-chosen operation the menu offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// An update surface command: a Machine action, a confirmation, an explanation, or the
    /// desktop's own update.
    Update(presentation::Command),
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MachineRow {
    pub(crate) row: ExtraRow,
    pub(crate) kind: MachineKind,
    /// The Machine waits on the person, as the title-bar control counts it, or is stopped and
    /// offers Start. A flat menu lists the first action of these rows only.
    pub(crate) pending: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Menu {
    /// The item's accessibility label and tooltip.
    pub(crate) summary: String,
    /// Something in the menu is a problem, so the item's symbol shows it.
    pub(crate) attention: bool,
    pub(crate) header: ExtraHeader,
    pub(crate) machines: Vec<MachineRow>,
    /// Recent history, newest first. Hosts show Recent Updates only when it has any.
    pub(crate) recent: Vec<String>,
    pub(crate) actions: Vec<ExtraItem>,
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
    /// The menu for `update`, with each Machine row's symbol read from `machines`.
    pub(crate) fn new(
        update: &presentation::Presentation,
        machines: &[MachineUpdate],
        recent: Vec<String>,
    ) -> Self {
        let header = update.extra_header();
        let machines: Vec<MachineRow> = update
            .extra_rows()
            .into_iter()
            .map(|row| MachineRow {
                kind: kind(row.machine, row.local, machines),
                pending: update.machine(row.machine).is_some_and(|view| {
                    view.state.pending() || view.state == MachineState::Stopped
                }),
                row,
            })
            .collect();
        let attention = header.tone == Tone::Problem
            || machines
                .iter()
                .any(|machine| machine.row.tone == Tone::Problem);
        Self {
            summary: format!("{HEADER_TITLE}, {}", header.line),
            attention,
            header,
            machines,
            recent,
            actions: update.extra_actions(),
        }
    }

    /// Every command the menu offers, in menu order.
    pub(crate) fn commands(&self) -> impl Iterator<Item = Command> + '_ {
        self.machines
            .iter()
            .filter_map(|machine| machine.row.items.as_deref())
            .flatten()
            .chain(&self.actions)
            .filter_map(|item| match item {
                ExtraItem::Action { command, .. } => Some(Command::Update(command.clone())),
                ExtraItem::Separator => None,
            })
            .chain([Command::Settings, Command::Quit])
    }

    /// Whether the menu still offers `command`. A host can deliver a row it drew before a change.
    pub(crate) fn offers(&self, command: &Command) -> bool {
        self.commands().any(|offered| offered == *command)
    }

    /// The same menu without submenus or subtitles: one status line each for the desktop and every
    /// Machine, then each Machine's first action while it needs the person, naming the Machine,
    /// then the actions group, Settings and Quit.
    #[cfg_attr(
        not(any(target_os = "linux", test)),
        allow(dead_code, reason = "only the Linux status area shows a flat menu")
    )]
    pub(crate) fn flat(&self) -> Vec<FlatRow> {
        let mut rows = vec![FlatRow::Text(format!(
            "{HEADER_TITLE} \u{b7} {}",
            self.header.line
        ))];
        rows.extend(self.machines.iter().map(|machine| {
            FlatRow::Text(format!("{} \u{b7} {}", machine.row.name, machine.row.line))
        }));
        let machines = self
            .machines
            .iter()
            .filter(|machine| machine.pending)
            .filter_map(|machine| {
                let (title, command) = first_action(machine.row.items.as_deref()?)?;
                let (title, asks) = match title.strip_suffix('\u{2026}') {
                    Some(title) => (title, "\u{2026}"),
                    None => (title, ""),
                };
                Some(FlatRow::Action {
                    title: format!("{title} on {}{asks}", machine.row.name),
                    command: Command::Update(command.clone()),
                })
            });
        let actions = self.actions.iter().filter_map(|item| {
            let (title, command) = first_action(std::slice::from_ref(item))?;
            Some(FlatRow::Action {
                title: title.to_owned(),
                command: Command::Update(command.clone()),
            })
        });
        let actions: Vec<FlatRow> = machines.chain(actions).collect();
        if !actions.is_empty() {
            rows.push(FlatRow::Separator);
            rows.extend(actions);
        }
        rows.extend([
            FlatRow::Separator,
            FlatRow::Action {
                title: SETTINGS_TITLE.to_owned(),
                command: Command::Settings,
            },
            FlatRow::Action {
                title: QUIT_TITLE.to_owned(),
                command: Command::Quit,
            },
        ]);
        rows
    }
}

fn first_action(items: &[ExtraItem]) -> Option<(&str, &presentation::Command)> {
    items.iter().find_map(|item| match item {
        ExtraItem::Action { title, command, .. } => Some((title.as_ref(), command)),
        ExtraItem::Separator => None,
    })
}

fn kind(machine: MachineId, local: bool, machines: &[MachineUpdate]) -> MachineKind {
    if local {
        return MachineKind::Local;
    }
    let platform = machines
        .iter()
        .find(|update| update.machine == machine)
        .and_then(|update| update.facts.as_ref())
        .map(|facts| facts.platform);
    match platform {
        Some(Platform::MacosArm64) => MachineKind::Mac,
        Some(Platform::LinuxX86_64) | None => MachineKind::Linux,
    }
}
