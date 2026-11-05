//! The launch prompts about the desktop state root: the one-time notice after rebuildable data
//! reset, the confirmation before every other reset, and the refusal of data a newer SpaceTerm
//! wrote. [`crate::migration`] owns the reset itself.
use spaceterm_core::migration::{self, DataKind, ResetReason};
use spaceterm_ui::{
    Alert, AlertIntent, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataAction {
    Reset,
    Quit,
    Dismiss,
}

/// Names the rebuildable data a reset replaced, once.
pub(crate) fn notice_alert(reset: &[DataKind]) -> Alert<DataAction> {
    Alert::new(
        ModalId::new("data-reset-notice"),
        "Some saved data was reset",
        "Some Saved Data Was Reset",
        format!(
            "SpaceTerm couldn\u{2019}t carry your {} forward from an earlier version, so it started over.",
            migration::nouns(reset)
        ),
        vec![
            ModalAction::new(
                DataAction::Dismiss,
                "OK",
                ModalActionRole::Affirmative,
                "data-reset-notice-ok",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
        ],
    )
    .intent(AlertIntent::Informational)
}

/// Asks before resetting data the person may not expect to lose. Quit leaves it unchanged.
pub(crate) fn confirmation_alert(reason: ResetReason, kinds: &[DataKind]) -> Alert<DataAction> {
    let nouns = migration::nouns(kinds);
    let mut detail = if reason == ResetReason::TooOld {
        format!(
            "Your {nouns} are from a SpaceTerm too old to update directly. To keep them, quit and install an earlier SpaceTerm release, then update from it."
        )
    } else {
        format!(
            "SpaceTerm couldn\u{2019}t carry your {nouns} forward from the version that saved them."
        )
    };
    detail.push_str(" Reset moves them aside and starts over.");
    if kinds.contains(&DataKind::SavedConnections) {
        detail.push_str(" Machines you added must be added again.");
    }
    Alert::new(
        ModalId::new("data-reset-confirm"),
        "Can\u{2019}t read saved data",
        "SpaceTerm Can\u{2019}t Read Some Saved Data",
        detail,
        vec![
            ModalAction::new(
                DataAction::Reset,
                "Reset",
                ModalActionRole::Affirmative,
                "data-reset-confirm-reset",
            )
            .with_intent(ModalActionIntent::Destructive),
            ModalAction::new(
                DataAction::Quit,
                "Quit",
                ModalActionRole::Cancel,
                "data-reset-confirm-quit",
            ),
        ],
    )
    .intent(AlertIntent::Warning)
}

/// Refuses data a newer SpaceTerm wrote. Nothing in it changes.
pub(crate) fn newer_alert() -> Alert<DataAction> {
    Alert::new(
        ModalId::new("data-newer"),
        "Saved data from a newer SpaceTerm",
        "Saved Data From a Newer SpaceTerm",
        "Update SpaceTerm to open your Workspaces. SpaceTerm hasn\u{2019}t changed this data.",
        vec![
            ModalAction::new(
                DataAction::Quit,
                "Quit",
                ModalActionRole::Affirmative,
                "data-newer-quit",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
        ],
    )
    .intent(AlertIntent::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_data_prompt_satisfies_the_desktop_alert_policy() {
        let policy = spaceterm_ui::ModalDesktopPolicy::mac_os();
        assert_eq!(notice_alert(&[DataKind::View]).validate(&policy), Ok(()));
        assert_eq!(
            confirmation_alert(ResetReason::TooOld, &[DataKind::SavedConnections])
                .validate(&policy),
            Ok(())
        );
        assert_eq!(newer_alert().validate(&policy), Ok(()));
    }
}
