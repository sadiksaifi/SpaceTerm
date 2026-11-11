//! Update Reminder text: one title and one body per reminder. The stage changes how often a
//! reminder comes, never its words.

use super::super::policy::DAY;

/// The title and body of a reminder about SpaceTerm itself. `downloaded` is false while the
/// version is only available, as with automatic downloads off.
pub(crate) fn app(version: &str, downloaded: bool, waited: u64) -> (String, String) {
    let waited = waited_sentence(waited);
    if downloaded {
        (
            format!("SpaceTerm {version} is downloaded"),
            format!("{waited} Relaunching keeps Terminal Sessions running."),
        )
    } else {
        (format!("SpaceTerm {version} is available"), waited)
    }
}

/// The title and body of a reminder about a SpaceTerm Core's Pending Version on `machine`.
pub(crate) fn core(version: &str, machine: &str, sessions: u32, waited: u64) -> (String, String) {
    let waited = waited_sentence(waited);
    let body = match sessions {
        0 => waited,
        1 => format!("{waited} Restarting ends 1 Terminal Session."),
        n => format!("{waited} Restarting ends {n} Terminal Sessions."),
    };
    (
        format!("SpaceTerm Core {version} is downloaded on {machine}"),
        body,
    )
}

/// "An update has waited 2 days." for `waited` seconds, in whole days.
fn waited_sentence(waited: u64) -> String {
    match waited / DAY {
        1 => "An update has waited 1 day.".to_owned(),
        days => format!("An update has waited {days} days."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_name_the_software_the_wait_and_what_restarting_ends() {
        assert_eq!(
            app("0.11.4", true, 2 * DAY + 5),
            (
                "SpaceTerm 0.11.4 is downloaded".to_owned(),
                "An update has waited 2 days. Relaunching keeps Terminal Sessions running."
                    .to_owned()
            )
        );
        assert_eq!(
            app("0.11.4", false, 7 * DAY),
            (
                "SpaceTerm 0.11.4 is available".to_owned(),
                "An update has waited 7 days.".to_owned()
            )
        );
        assert_eq!(
            core("0.11.4", "penguin", 3, 2 * DAY),
            (
                "SpaceTerm Core 0.11.4 is downloaded on penguin".to_owned(),
                "An update has waited 2 days. Restarting ends 3 Terminal Sessions.".to_owned()
            )
        );
        assert_eq!(
            core("0.11.4", "This Mac", 1, 3 * DAY).1,
            "An update has waited 3 days. Restarting ends 1 Terminal Session."
        );
        assert_eq!(
            core("0.11.4", "This Mac", 0, 2 * DAY).1,
            "An update has waited 2 days."
        );
    }
}
