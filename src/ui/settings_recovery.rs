//! Settings Recovery prompts: the launch offer and the Settings window's confirmation.
//! [`crate::settings::recovery`] owns the reset and its backup.

use gpui::{App, WindowHandle};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::WorkspaceManager;
use super::appearance_runtime::AppearanceRuntime;
use crate::settings::SettingsError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryAction {
    Reset,
    OpenSettings,
    NotNow,
}

/// Offers Settings Recovery in the first Workspace window when the retained document is Malformed
/// Settings. Launch opens that window only after any Update launch window releases it.
pub(crate) fn offer_at_launch(workspace: WindowHandle<WorkspaceManager>, cx: &mut App) {
    let Some(runtime) = cx.try_global::<AppearanceRuntime>() else {
        return;
    };
    if !runtime
        .settings
        .snapshot()
        .status
        .is_some_and(SettingsError::is_malformed)
    {
        return;
    }
    // The prompt attaches to the window after its first frame, as any other window-modal alert.
    cx.defer(move |cx| {
        let _ = workspace.update(cx, |_, window, cx| {
            let result = launch_alert().present(window, cx, move |outcome, cx| {
                if let AlertOutcome::Activated {
                    action_id: RecoveryAction::Reset,
                    ..
                } = outcome
                {
                    reset(workspace, cx);
                }
            });
            if result.is_err() {
                eprintln!("failed to present the SpaceTerm settings recovery prompt");
            }
        });
    });
}

fn reset(workspace: WindowHandle<WorkspaceManager>, cx: &mut App) {
    let Some(settings) = cx
        .try_global::<AppearanceRuntime>()
        .map(|runtime| runtime.settings.clone())
    else {
        return;
    };
    if settings.recover_by_reset().is_ok() {
        return;
    }
    eprintln!("SpaceTerm Settings could not be reset");
    let _ = workspace.update(cx, |_, window, cx| {
        let result = failure_alert().present(window, cx, |outcome, cx| {
            if matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: RecoveryAction::OpenSettings,
                    ..
                }
            ) {
                super::settings_window::open_or_activate(None, cx);
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings recovery failure");
        }
    });
}

fn launch_alert() -> Alert<RecoveryAction> {
    Alert::new(
        ModalId::new("settings-recovery"),
        "SpaceTerm couldn't read your settings",
        "SpaceTerm Couldn't Read Your Settings",
        "SpaceTerm is using default settings and has changed nothing. Reset Settings keeps the unreadable file as a backup.",
        vec![
            ModalAction::new(
                RecoveryAction::Reset,
                "Reset Settings",
                ModalActionRole::Affirmative,
                "settings-recovery-reset",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
            ModalAction::new(
                RecoveryAction::NotNow,
                "Not Now",
                ModalActionRole::Cancel,
                "settings-recovery-not-now",
            ),
        ],
    )
    .intent(AlertIntent::Warning)
}

fn failure_alert() -> Alert<RecoveryAction> {
    Alert::new(
        ModalId::new("settings-recovery-failed"),
        "SpaceTerm couldn't reset your settings",
        "SpaceTerm Couldn't Reset Your Settings",
        "Your settings file could not be replaced. Open Settings to see what stopped the reset and try again.",
        vec![
            ModalAction::new(
                RecoveryAction::OpenSettings,
                "Open Settings",
                ModalActionRole::Affirmative,
                "settings-recovery-failed-open-settings",
            ),
            ModalAction::new(
                RecoveryAction::NotNow,
                "OK",
                ModalActionRole::Cancel,
                "settings-recovery-failed-ok",
            ),
        ],
    )
    .intent(AlertIntent::Warning)
}

/// The Settings window's confirmation before Settings Recovery. `true` confirms.
pub(super) fn confirmation_alert() -> Alert<bool> {
    Alert::new(
        ModalId::new("settings-recovery-confirm"),
        "Reset settings",
        "Reset Settings?",
        "SpaceTerm renames the unreadable settings file to settings.json.bak and starts from default settings.",
        vec![
            ModalAction::new(
                true,
                "Reset",
                ModalActionRole::Affirmative,
                "settings-recovery-confirm-reset",
            )
            .with_intent(ModalActionIntent::Destructive)
            .with_emphasis(ModalActionEmphasis::Prominent),
            ModalAction::new(
                false,
                "Cancel",
                ModalActionRole::Cancel,
                "settings-recovery-confirm-cancel",
            ),
        ],
    )
    .intent(AlertIntent::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recovery_prompt_should_satisfy_the_desktop_alert_policy() {
        let policy = spaceterm_ui::ModalDesktopPolicy::mac_os();
        assert_eq!(launch_alert().validate(&policy), Ok(()));
        assert_eq!(failure_alert().validate(&policy), Ok(()));
        assert_eq!(confirmation_alert().validate(&policy), Ok(()));
    }
}
