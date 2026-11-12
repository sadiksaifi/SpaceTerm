//! Text of the surfaces that render the update rows on demand: Settings > Updates, the Machine
//! card in Settings > Machines, the menu bar extra on macOS and the status item on Linux. The
//! rows' own statuses and actions come from [`super::rows`] through
//! [`crate::updates::state::RowView`]; this module only frames them.

use crate::updates::machines::MachineAction;

/// The product name, which heads the SpaceTerm row and the menu bar extra.
pub(crate) const SPACETERM: &str = "SpaceTerm";
/// The heading over the Machine rows on every surface.
pub(crate) const CORE_HEADING: &str = "SpaceTerm Core";

/// Settings > Updates, under the page title.
pub(crate) const UPDATES_DESCRIPTION: &str = "SpaceTerm downloads updates in the background and \
     asks more often the longer one waits. It never ends a Terminal Session that has work going on.";
pub(crate) const AUTOMATIC_DOWNLOADS: &str = "Download SpaceTerm updates automatically";
pub(crate) const AUTOMATIC_DOWNLOADS_DESCRIPTION: &str =
    "Relaunch to update SpaceTerm. Terminal Sessions keep running.";
pub(crate) const CHECK_INTERVAL_DESCRIPTION: &str = "SpaceTerm checks when it opens.";
pub(crate) const CHECK_NOW: &str = "Check Now";
pub(crate) const CHECK_NOW_DESCRIPTION: &str =
    "Checks for a newer SpaceTerm and asks each connected Machine to check.";
/// Settings > Updates when no Machine is known yet.
pub(crate) const NO_MACHINES: &str = "No Machines yet";

/// The Machine card's ⋯ item for a same-version restart. Offered only while no Pending Version
/// waits, because a restart moves into a waiting one.
pub(crate) const RESTART_SAME_VERSION: &str = "Restart SpaceTerm Core\u{2026}";

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
