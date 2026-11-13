//! Text of the surfaces that render the update rows on demand: Settings > Updates, the Machine
//! card in Settings > Machines, the menu bar extra on macOS and the status item on Linux. The
//! rows' own statuses and actions come from [`super::rows`] through
//! [`crate::updates::state::RowView`]; this module only frames them.

use gpui::SharedString;
use spaceterm_core::update::facts::CheckFailure;

use super::rows::APP_NAME;
use crate::updates::machines::MachineAction;
use crate::updates::state::{
    AppRow, AppStage, AskedCheck, ItemCommand, LastCheck, MachineRow, MachineStage, RowView, Target,
};

/// The product name, which heads the SpaceTerm row and the menu bar extra.
pub(crate) const SPACETERM: &str = "SpaceTerm";
/// The heading over the Machine rows on every surface.
pub(crate) const CORE_HEADING: &str = "SpaceTerm Core";

/// Settings > Updates, under the page title: the one place the page names what an update ends.
pub(crate) const UPDATES_DESCRIPTION: &str = "Relaunching SpaceTerm keeps Terminal Sessions \
     running. Restarting SpaceTerm Core ends that Machine\u{2019}s Terminal Sessions.";
pub(crate) const AUTOMATIC_DOWNLOADS: &str = "Download SpaceTerm updates automatically";
pub(crate) const CHECK_INTERVAL: &str = "Check for SpaceTerm updates";
pub(crate) const CHECK_NOW: &str = "Check Now";
/// Check Now's accessibility description and tooltip: what it checks.
pub(crate) const CHECK_NOW_DESCRIPTION: &str = "Checks for a newer SpaceTerm and asks each connected Machine\u{2019}s SpaceTerm Core to check.";
/// The entry in an item's menu that checks that item alone. The menu names the item.
const CHECK_ITEM: &str = "Check for Updates";
/// Each item's menu, by the item's name.
pub(crate) fn item_menu_label(name: &str) -> String {
    format!("More for {name}")
}
/// Settings > Updates when no Machine is known yet.
pub(crate) const NO_MACHINES: &str = "No Machines yet";

/// The Machine card's ⋯ item for a same-version restart. Offered only while no Pending Version
/// waits, because a restart moves into a waiting one.
pub(crate) const RESTART_SAME_VERSION: &str = "Restart SpaceTerm Core\u{2026}";
/// The same item when the restart ends no Terminal Session, so no confirmation follows.
pub(crate) const RESTART_SAME_VERSION_NOW: &str = "Restart SpaceTerm Core";

/// The menu bar extra's name for the SpaceTerm row's `Download`, the label the pill gives it.
pub(crate) const DOWNLOAD_UPDATE: &str = "Download Update";
pub(crate) const STOP_CORE: &str = "Stop SpaceTerm Core\u{2026}";
pub(crate) const START_CORE: &str = "Start SpaceTerm Core";
pub(crate) const SETTINGS: &str = "SpaceTerm Settings\u{2026}";
pub(crate) const QUIT: &str = "Quit SpaceTerm";

/// A version and a status on one line: "0.11.3 · Restarts into 0.11.4 when 2 Terminal Sessions
/// end". Either part alone stands by itself.
pub(crate) fn line(version: Option<&str>, status: Option<&str>) -> String {
    match (version, status) {
        (Some(version), Some(status)) if version != status => {
            format!("{version} \u{b7} {status}")
        }
        (Some(version), _) => version.to_owned(),
        (None, Some(status)) => status.to_owned(),
        (None, None) => String::new(),
    }
}

/// The menu bar extra's count after a settled Machine's version.
pub(crate) fn sessions(n: u32) -> String {
    match n {
        0 => "No Terminal Sessions".to_owned(),
        1 => "1 Terminal Session".to_owned(),
        n => format!("{n} Terminal Sessions"),
    }
}

/// A menu item's subtitle for an action that ends Terminal Sessions. None when none is open, so
/// no consequence carries a zero count.
pub(crate) fn ends(n: u32) -> Option<String> {
    match n {
        0 => None,
        1 => Some("Ends 1 Terminal Session".to_owned()),
        n => Some(format!("Ends {n} Terminal Sessions")),
    }
}

/// A flat menu's item, which has no submenu or row to name its subject, so it names SpaceTerm
/// Core and the Machine: "Restart SpaceTerm Core on This Computer…", "Download SpaceTerm Core to
/// penguin", "Check for SpaceTerm Core Updates on penguin", "How to Update SpaceTerm Core on
/// penguin…".
/// `action` is the item's Machine action, if it performs one.
pub(crate) fn flat_action(label: &str, machine: &str, action: Option<&MachineAction>) -> String {
    let (label, asks) = match label.strip_suffix('\u{2026}') {
        Some(label) => (label, "\u{2026}"),
        None => (label, ""),
    };
    match action {
        Some(MachineAction::Restart) => format!("Restart SpaceTerm Core on {machine}{asks}"),
        Some(MachineAction::Install) => format!("Download SpaceTerm Core to {machine}{asks}"),
        Some(MachineAction::CheckAgain) => {
            format!("Check for SpaceTerm Core Updates on {machine}{asks}")
        }
        _ if label.contains("SpaceTerm Core") => format!("{label} on {machine}{asks}"),
        _ => format!("{label} SpaceTerm Core on {machine}{asks}"),
    }
}

/// The status item's accessibility label and tooltip.
pub(crate) fn summary(line: &str) -> String {
    format!("{SPACETERM}, {line}")
}

/// "just now", "5 minutes ago", "yesterday": how long ago something happened, from its age in
/// seconds.
pub(crate) fn ago(age: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    match age {
        age if age < MINUTE => "just now".to_owned(),
        age if age < HOUR => plural(age / MINUTE, "minute"),
        age if age < DAY => plural(age / HOUR, "hour"),
        age if age < 2 * DAY => "yesterday".to_owned(),
        age => plural(age / DAY, "day"),
    }
}

/// One entry of an item's menu: readable information when `command` is None.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemEntry {
    pub(crate) label: SharedString,
    pub(crate) command: Option<ItemCommand>,
    pub(crate) enabled: bool,
    pub(crate) separator_before: bool,
    /// A stable name for tests and accessibility tooling.
    pub(crate) selector: &'static str,
}

impl ItemEntry {
    fn info(label: String) -> Self {
        Self {
            label: label.into(),
            command: None,
            enabled: true,
            separator_before: false,
            selector: "info",
        }
    }

    fn command(label: String, command: ItemCommand, selector: &'static str) -> Self {
        Self {
            label: label.into(),
            command: Some(command),
            enabled: true,
            separator_before: false,
            selector,
        }
    }
}

/// When the item last checked, and why its checks fail, as readable lines.
fn check_lines(last: LastCheck, name: &str) -> Vec<ItemEntry> {
    let checked = |age: Option<u64>| match age {
        Some(age) => format!("Last checked {}", ago(age)),
        None => "Not checked yet".to_owned(),
    };
    match last {
        // Unknown facts say nothing about a check.
        LastCheck::Unknown => Vec::new(),
        LastCheck::Never => vec![ItemEntry::info(checked(None))],
        LastCheck::Succeeded { age } => vec![ItemEntry::info(checked(Some(age)))],
        LastCheck::Failing { reason, success } => {
            let mut lines = vec![ItemEntry::info(checked(success))];
            lines.push(ItemEntry::info(match reason {
                Some(CheckFailure::Network) => {
                    "Checks fail: the release feed can\u{2019}t be reached".to_owned()
                }
                Some(CheckFailure::Proxy) => {
                    "Checks fail: the update proxy doesn\u{2019}t answer".to_owned()
                }
                Some(CheckFailure::Verification) => {
                    "Checks fail: the release feed doesn\u{2019}t verify".to_owned()
                }
                Some(CheckFailure::SystemTooOld) => {
                    format!("The newest release needs a newer system than {name}\u{2019}s")
                }
                Some(CheckFailure::ClockBehind) => {
                    format!("Checks fail: {name}\u{2019}s clock is behind")
                }
                Some(CheckFailure::SteppingStoneSkipped) => {
                    "Checks fail: a required update couldn\u{2019}t start".to_owned()
                }
                None => "The last check failed".to_owned(),
            }));
            lines
        }
    }
}

/// The notes entry for `version`: this build's own notes when it carries them for that
/// version, otherwise its release page. A build that names no release offers neither.
/// `selector` tells the running version's entry from the next one's.
fn notes_entry(version: &str, embedded: Option<&str>, selector: &'static str) -> Option<ItemEntry> {
    if embedded == Some(version) {
        return Some(ItemEntry::command(
            format!("What\u{2019}s New in {version}"),
            ItemCommand::WhatsNew,
            selector,
        ));
    }
    crate::updates::stable_version(version)?;
    Some(ItemEntry::command(
        format!("Release Notes for {version}"),
        ItemCommand::ReleasePage(version.to_owned().into()),
        selector,
    ))
}

/// Separates the notes from the check above them.
fn notes_section(mut notes: Vec<ItemEntry>) -> Vec<ItemEntry> {
    if let Some(first) = notes.first_mut() {
        first.separator_before = true;
    }
    notes
}

/// SpaceTerm's menu: when it last checked, a check of SpaceTerm alone, and the notes of the
/// running and the waiting version. `embedded` is the version this build carries notes for.
/// `checks` is false without a desktop update service.
pub(crate) fn app_menu(
    app: &AppRow,
    row: &RowView,
    embedded: Option<&str>,
    checks: bool,
) -> Vec<ItemEntry> {
    let mut entries = check_lines(app.last_check, APP_NAME);
    entries.extend(
        row.description
            .iter()
            .map(|text| ItemEntry::info(text.to_string())),
    );
    let idle = matches!(
        app.stage,
        AppStage::Current { .. } | AppStage::DownloadFailed { .. }
    );
    let mut check = ItemEntry::command(
        CHECK_ITEM.to_owned(),
        ItemCommand::Check(Target::App),
        "check",
    );
    check.enabled = checks && idle && app.asked_check != Some(AskedCheck::Running);
    entries.push(check);
    let waiting = match &app.stage {
        AppStage::Available { version }
        | AppStage::Downloading { version, .. }
        | AppStage::Downloaded { version }
        | AppStage::Relaunching { version } => Some(version.to_string()),
        AppStage::DownloadFailed { version, .. }
        | AppStage::RelaunchFailed { version, .. }
        | AppStage::NeedsNewerSystem { version } => version.as_ref().map(ToString::to_string),
        AppStage::Current { .. } | AppStage::CantReplace => None,
    };
    let notes = [
        (Some(app.running.to_string()), "notes"),
        (waiting, "next-notes"),
    ]
    .into_iter()
    .filter_map(|(version, selector)| notes_entry(&version?, embedded, selector))
    .collect();
    entries.extend(notes_section(notes));
    entries
}

/// A Machine's menu: when its Core last checked, a check of that Core alone, and the notes of
/// its running and its next version. `embedded` is the version this build carries notes for,
/// which a Core of the same release shares.
pub(crate) fn machine_menu(
    machine: &MachineRow,
    row: &RowView,
    embedded: Option<&str>,
) -> Vec<ItemEntry> {
    let mut entries = check_lines(machine.last_check, &machine.name);
    entries.extend(
        row.description
            .iter()
            .map(|text| ItemEntry::info(text.to_string())),
    );
    let (label, enabled) = match &machine.stage {
        MachineStage::Offline => ("Connect and Check", true),
        MachineStage::Stopped
        | MachineStage::Restarting { .. }
        | MachineStage::Downloading { .. } => (CHECK_ITEM, false),
        _ => (CHECK_ITEM, true),
    };
    let mut check = ItemEntry::command(
        label.to_owned(),
        ItemCommand::Check(Target::Machine(machine.id)),
        "check",
    );
    check.enabled = enabled && machine.asked_check != Some(AskedCheck::Running);
    entries.push(check);
    let next = match &machine.stage {
        MachineStage::WaitingForApp { version }
        | MachineStage::Downloading { version, .. }
        | MachineStage::Downloaded { version, .. }
        | MachineStage::StartFailed { version, .. }
        | MachineStage::DownloadFailed { version, .. }
        | MachineStage::NotConfirmed { version, .. }
        | MachineStage::NeedsFloor { version, .. } => Some(version.clone()),
        MachineStage::Restarting { to, .. } => to.clone(),
        _ => None,
    };
    let running = machine.running.as_ref().map(ToString::to_string);
    let next = next
        .map(|version| version.to_string())
        .filter(|version| Some(version) != running.as_ref());
    let notes = [(running, "notes"), (next, "next-notes")]
        .into_iter()
        .filter_map(|(version, selector)| notes_entry(&version?, embedded, selector))
        .collect();
    entries.extend(notes_section(notes));
    entries
}
