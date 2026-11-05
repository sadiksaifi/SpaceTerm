//! What the person sees about the desktop's retained data when a Data Generation reset is
//! involved: the one-time notice after rebuildable data reset, the confirmation before a reset
//! that needs one, and the launch refusals of data a newer SpaceTerm wrote or storage that is
//! unavailable. Every text names kinds of data and versions, never a path or a raw error.
//! [`crate::migration`] owns the reset itself.
use std::sync::Arc;

use gpui::{App, AppContext as _, WindowHandle};
use spaceterm_core::migration::{self, DataKind, ResetReason};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::WorkspaceManager;
use crate::command_line_tool::FileViewer;

/// The notice an automatic reset left for the first Workspace window.
pub(crate) struct PendingNotice {
    pub(crate) reset: Vec<DataKind>,
    /// The desktop state root, which keeps the notice until the person acknowledges it, and
    /// whose reset directory the file manager shows. Its path is never named in text.
    pub(crate) store: Arc<migration::DirectoryStore>,
    pub(crate) viewer: Option<Arc<dyn FileViewer>>,
}

impl gpui::Global for PendingNotice {}

/// Shows the notice an automatic reset left, once, in the first Workspace window.
pub(crate) fn offer_notice(workspace: WindowHandle<WorkspaceManager>, cx: &mut App) {
    if !cx.has_global::<PendingNotice>() {
        return;
    }
    // The root keeps the notice, so a launch that never presents it shows it next time.
    let PendingNotice {
        reset,
        store,
        viewer,
    } = cx.remove_global::<PendingNotice>();
    // The prompt attaches to the window after its first frame, as any other window-modal alert.
    cx.defer(move |cx| {
        let _ = workspace.update(cx, |_, window, cx| {
            let alert = notice_alert(&reset, viewer.is_some());
            let result = alert.present(window, cx, move |outcome, cx| {
                let AlertOutcome::Activated { action_id, .. } = outcome else {
                    return;
                };
                acknowledge(&store);
                if action_id == DataAction::ShowFiles
                    && let Some(viewer) = viewer
                {
                    let reveal = viewer.reveal(&store.reset_directory());
                    cx.background_spawn(async move {
                        let _ = reveal.await;
                    })
                    .detach();
                }
            });
            if result.is_err() {
                eprintln!("failed to present the SpaceTerm data reset notice");
            }
        });
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataAction {
    Reset,
    TryAgain,
    ShowFiles,
    Quit,
    Dismiss,
}

/// Forgets the notice the person acknowledged with either action.
pub(crate) fn acknowledge(store: &migration::DirectoryStore) {
    let _ = migration::acknowledge_notice(store);
}

/// How to keep data a SpaceTerm too old to update directly saved: install the Stepping Stone
/// release `floor` first, or an earlier release when this build names no floor.
pub(crate) fn keep_too_old(floor: Option<&str>) -> String {
    match floor {
        Some(floor) => {
            format!("To keep them, quit and install SpaceTerm {floor} first, then update from it.")
        }
        None => "To keep them, quit and install an earlier SpaceTerm release first, then update \
                 from it."
            .to_owned(),
    }
}

/// The Settings Recovery detail for settings a SpaceTerm too old to update directly saved.
pub(crate) fn outdated_settings(floor: Option<&str>) -> String {
    let release = floor.map_or_else(
        || "an earlier SpaceTerm release".to_owned(),
        |floor| format!("SpaceTerm {floor}"),
    );
    format!(
        "Your settings are from a SpaceTerm too old to update directly. To keep yours, install \
         {release} first, then update from it. Until then, SpaceTerm uses default settings. Reset \
         Settings replaces yours with defaults and keeps the earlier file."
    )
}

/// The one-time notice after rebuildable data reset on its own.
pub(crate) fn notice_detail(reset: &[DataKind]) -> String {
    format!(
        "SpaceTerm couldn\u{2019}t carry your {} forward from an earlier version, so it started \
         over. The earlier files are kept.",
        migration::nouns(reset)
    )
}

/// What the confirmation before a reset says is lost and why.
pub(crate) fn confirmation_detail(
    reason: ResetReason,
    kinds: &[DataKind],
    floor: Option<&str>,
) -> String {
    let nouns = migration::nouns(kinds);
    let mut detail = if reason == ResetReason::TooOld {
        format!(
            "Your {nouns} are from a SpaceTerm too old to update directly. {}",
            keep_too_old(floor)
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
    detail
}

pub(crate) fn notice_alert(reset: &[DataKind], can_show: bool) -> Alert<DataAction> {
    let mut actions = vec![
        ModalAction::new(
            DataAction::Dismiss,
            "OK",
            ModalActionRole::Affirmative,
            "data-reset-notice-ok",
        )
        .default_action(true),
    ];
    if can_show {
        actions.push(ModalAction::new(
            DataAction::ShowFiles,
            "Show Earlier Files",
            // Not Cancel: Escape must never open the file manager.
            ModalActionRole::Auxiliary,
            "data-reset-notice-show",
        ));
    }
    Alert::new(
        ModalId::new("data-reset-notice"),
        "Some saved data was reset",
        "Some Saved Data Was Reset",
        notice_detail(reset),
        actions,
    )
    .intent(AlertIntent::Informational)
}

/// Asks before resetting data the person may not expect to lose. Quit leaves it unchanged.
pub(crate) fn confirmation_alert(
    reason: ResetReason,
    kinds: &[DataKind],
    floor: Option<&str>,
) -> Alert<DataAction> {
    Alert::new(
        ModalId::new("data-reset-confirm"),
        "Can\u{2019}t read saved data",
        "SpaceTerm Can\u{2019}t Read Some Saved Data",
        confirmation_detail(reason, kinds, floor),
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
        "Install the newest SpaceTerm to open your Workspaces. SpaceTerm hasn\u{2019}t changed \
         this data.",
        vec![
            ModalAction::new(
                DataAction::Quit,
                "Quit",
                ModalActionRole::Affirmative,
                "data-newer-quit",
            )
            .default_action(true),
        ],
    )
    .intent(AlertIntent::Warning)
}

/// Storage that can't be read is never treated as empty. Try Again checks it again.
pub(crate) fn unavailable_alert() -> Alert<DataAction> {
    Alert::new(
        ModalId::new("data-unavailable"),
        "Saved data unavailable",
        "SpaceTerm Can\u{2019}t Reach Its Saved Data",
        "Make sure your account can read and change SpaceTerm\u{2019}s folders, then try again. \
         SpaceTerm hasn\u{2019}t changed this data.",
        vec![
            ModalAction::new(
                DataAction::TryAgain,
                "Try Again",
                ModalActionRole::Affirmative,
                "data-unavailable-try-again",
            )
            .default_action(true),
            ModalAction::new(
                DataAction::Quit,
                "Quit",
                ModalActionRole::Cancel,
                "data-unavailable-quit",
            ),
        ],
    )
    .intent(AlertIntent::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, IntoElement, Render, Styled as _, TestAppContext, Window, div};
    use spaceterm_ui::ModalLayer;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct LaunchWindow;

    impl Render for LaunchWindow {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    /// Presents `alert`, presses `keys`, and returns the action they chose, if any.
    fn answer(
        cx: &mut TestAppContext,
        alert: Alert<DataAction>,
        keys: &str,
    ) -> Option<DataAction> {
        cx.update(crate::ui::init).unwrap();
        let (root, cx) = cx.add_window_view(|window, _| {
            window.activate_window();
            LaunchWindow
        });
        let outcome = Rc::new(RefCell::new(None));
        let recorded = Rc::clone(&outcome);
        cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                alert
                    .present(window, cx, move |outcome, _| {
                        if let AlertOutcome::Activated { action_id, .. } = outcome {
                            *recorded.borrow_mut() = Some(action_id);
                        }
                    })
                    .unwrap();
            });
        });
        cx.run_until_parked();
        let keystroke = gpui::Keystroke::parse(keys).unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
        outcome.take()
    }

    #[gpui::test]
    fn return_tries_again_when_saved_data_is_unavailable(cx: &mut TestAppContext) {
        assert_eq!(
            answer(cx, unavailable_alert(), "enter"),
            Some(DataAction::TryAgain)
        );
    }

    #[gpui::test]
    fn escape_never_shows_the_earlier_files(cx: &mut TestAppContext) {
        assert_ne!(
            answer(cx, notice_alert(&[DataKind::View], true), "escape"),
            Some(DataAction::ShowFiles)
        );
    }

    #[gpui::test]
    fn return_acknowledges_the_reset_notice(cx: &mut TestAppContext) {
        assert_eq!(
            answer(cx, notice_alert(&[DataKind::View], true), "enter"),
            Some(DataAction::Dismiss)
        );
    }

    #[test]
    fn too_old_data_names_the_stepping_stone_to_install() {
        assert_eq!(
            confirmation_detail(
                ResetReason::TooOld,
                &[DataKind::SavedConnections, DataKind::ClientRecords],
                Some("1.1.0"),
            ),
            "Your saved machines and unfinished Workspace changes are from a SpaceTerm too old to \
             update directly. To keep them, quit and install SpaceTerm 1.1.0 first, then update from \
             it. Reset moves them aside and starts over. Machines you added must be added again."
        );
        assert!(outdated_settings(Some("1.1.0")).contains("install SpaceTerm 1.1.0 first"));
        assert!(outdated_settings(None).contains("To keep yours, install an earlier"));
        assert!(
            !confirmation_detail(
                ResetReason::Invalid,
                &[DataKind::ClientRecords],
                Some("1.1.0")
            )
            .contains("1.1.0"),
            "only data too old to update names a Stepping Stone"
        );
    }

    #[test]
    fn every_data_prompt_satisfies_the_desktop_alert_policy() {
        let policy = spaceterm_ui::ModalDesktopPolicy::mac_os();
        for alert in [
            notice_alert(&[DataKind::View], true),
            notice_alert(&[DataKind::View], false),
            confirmation_alert(ResetReason::TooOld, &[DataKind::SavedConnections], None),
            newer_alert(),
            unavailable_alert(),
        ] {
            assert_eq!(alert.validate(&policy), Ok(()));
        }
    }
}
