//! Every person-facing update string of the desktop. Each submodule renders one family of
//! surfaces from [`super::state::UpdateView`]; no other module writes update text. Apostrophes
//! are straight everywhere; ellipses are U+2026 and the middle dot is U+00B7.
#![allow(
    dead_code,
    reason = "the title-bar, surfaces, ladder and pane slices each render part of this copy"
)]

pub(crate) mod dialogs;
pub(crate) mod pane;
pub(crate) mod reminders;
pub(crate) mod rows;
pub(crate) mod settings;
pub(crate) mod title_bar;

#[cfg(test)]
mod deck;

/// "1 Terminal Session", "3 Terminal Sessions".
pub(crate) fn sessions(n: u32) -> String {
    match n {
        1 => "1 Terminal Session".to_owned(),
        n => format!("{n} Terminal Sessions"),
    }
}

/// The consequence after the action is named: "This ends 2 Terminal Sessions." or "No Terminal
/// Session is open."
pub(crate) fn this_ends(n: u32) -> String {
    match n {
        0 => "No Terminal Session is open.".to_owned(),
        n => format!("This ends {}.", sessions(n)),
    }
}

/// "2 days". The ladder asks from two days, so a waiting update never reads as zero days.
pub(crate) fn days(n: u64) -> String {
    match n {
        1 => "1 day".to_owned(),
        n => format!("{n} days"),
    }
}

/// "An update has waited 2 days."
pub(crate) fn waited(n: u64) -> String {
    format!("An update has waited {}.", days(n))
}

/// "iPad", "iPad and Work Mac", "iPad, Work Mac and Studio".
pub(crate) fn list_and(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
