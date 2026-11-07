//! The What's new sheet: this build's release notes, embedded at build time, ending with a link
//! to the release on GitHub. The notes count as read once the sheet opens, not while it waits
//! behind another modal.
//!
//! The title-bar update control and the menu-bar menu offer the sheet while
//! [`release_notes_unread`] holds, and open it with [`open_whats_new`]. Both read the
//! [`crate::updates::UpdateService`] entity, so observing it reports changes to the flag.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{App, Context, Global, SharedString, Window, accesskit, actions, div};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, Dialog, DialogActionRequest, DialogCloseDecision,
    DialogInitialFocus, DialogOutcome, DialogPendingCompletion, DialogSize, ModalAction,
    ModalActionEmphasis, ModalActionRole, ModalId, ModalLifecycleEvent,
};

use super::{front_workspace_window, service};
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::updates::release_notes::{self, NotesBlock, ReleaseNotes};

actions!(spaceterm, [OpenWhatsNew]);

/// The Help menu command, present only in a build that carries release notes.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the native application menu names it")
)]
pub(crate) const WHATS_NEW_TITLE: &str = "What's New in SpaceTerm";
const MODAL_ID: &str = "whats-new";
/// The link below the notes that opens this version's release on GitHub.
const RELEASE_LINK: &str = "View release on GitHub";
const RELEASE_LINK_SELECTOR: &str = "whats-new-view-release-on-github";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SheetAction {
    Done,
}

/// The sheet is open in some window, so asking again shows no second one.
struct OpenSheet;
impl Global for OpenSheet {}

pub(super) fn init(cx: &mut App) {
    cx.on_action(|_: &OpenWhatsNew, cx| open_whats_new(cx));
}

/// The menu command that opens the sheet, such as "What's New in SpaceTerm 0.9", or nothing in a
/// build without release notes.
#[expect(
    dead_code,
    reason = "the menu-bar menu names its What's new command with this"
)]
pub(crate) fn whats_new_menu_title() -> Option<String> {
    release_notes::embedded().map(ReleaseNotes::menu_title)
}

/// Opens the What's new sheet in the front Workspace window. The notes count as read once it
/// opens. A build without release notes, or without a Workspace window, shows nothing.
pub(crate) fn open_whats_new(cx: &mut App) {
    let Some(notes) = release_notes::embedded() else {
        return;
    };
    if cx.has_global::<OpenSheet>() {
        return;
    }
    // Menu actions bubble while their window is borrowed. Open after dispatch releases it.
    cx.defer(move |cx| {
        if cx.has_global::<OpenSheet>() {
            return;
        }
        let Some(handle) = front_workspace_window(cx) else {
            return;
        };
        let this_year = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let this_year = release_notes::year_at(this_year);
        let _ = handle.update(cx, |_, window, cx| present(notes, this_year, window, cx));
    });
}

fn sheet(
    notes: &ReleaseNotes,
    this_year: i32,
    body: gpui::Entity<NotesBody>,
) -> Dialog<SheetAction> {
    let title = notes.title();
    let dialog = Dialog::new(
        ModalId::new(MODAL_ID),
        title.clone(),
        title,
        vec![
            ModalAction::new(
                SheetAction::Done,
                "Done",
                ModalActionRole::Cancel,
                "whats-new-done",
            )
            .with_emphasis(ModalActionEmphasis::Prominent),
        ],
        DialogInitialFocus::Action(SheetAction::Done),
    )
    .size(DialogSize::Regular)
    .body(body);
    match notes.released(this_year) {
        Some(released) => dialog.description(released),
        None => dialog,
    }
}

/// Presents the sheet for `notes` in this window, or queues it behind the modal already there.
/// The notes count as read once the sheet opens. Returns whether the window took the sheet.
fn present<T: 'static>(
    notes: &ReleaseNotes,
    this_year: i32,
    window: &mut Window,
    cx: &mut Context<T>,
) -> bool {
    let body = cx.new(|_| NotesBody {
        blocks: notes.blocks().to_vec(),
        release_url: notes.github_url(),
    });
    let on_action = |request: DialogActionRequest<SheetAction>,
                     _: DialogPendingCompletion,
                     _: &mut App| match request.action_id() {
        SheetAction::Done => DialogCloseDecision::Allow,
    };
    let on_result = |_: DialogOutcome<SheetAction>, cx: &mut App| {
        cx.remove_global::<OpenSheet>();
    };
    // A queued sheet can be discarded unseen, so only an open one marks the notes read.
    let on_lifecycle = |event: &ModalLifecycleEvent, cx: &mut App| {
        if matches!(event, ModalLifecycleEvent::Opened(_))
            && let Some(updates) = service(cx)
        {
            updates.update(cx, |updates, cx| updates.read_release_notes(cx));
        }
    };
    let presented = sheet(notes, this_year, body)
        .present_with_lifecycle(window, cx, on_action, on_result, on_lifecycle)
        .is_ok();
    if presented {
        cx.set_global(OpenSheet);
    } else {
        eprintln!("failed to present the What's new sheet");
    }
    presented
}

/// The notes as groups of bulleted changes, then the link to the release on GitHub.
struct NotesBody {
    blocks: Vec<NotesBlock>,
    release_url: String,
}

impl Render for NotesBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::appearance::shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let body = appearance.typography.style(TextRole::Body);
        let mut column = div()
            .chrome_text(body)
            .text_color(gpui_color(colors.text))
            .flex()
            .flex_col()
            .gap(appearance.spacing(4.0));
        for (index, block) in self.blocks.iter().enumerate() {
            let id = ("whats-new-block", index);
            column = column.child(match block {
                NotesBlock::Heading(text) => div()
                    .id(id)
                    .role(accesskit::Role::Heading)
                    .aria_label(SharedString::from(text.clone()))
                    .when(index > 0, |heading| heading.pt(appearance.spacing(8.0)))
                    .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                    .child(SharedString::from(text.clone())),
                NotesBlock::Item(text) => div()
                    .id(id)
                    .role(accesskit::Role::Label)
                    .aria_value(SharedString::from(text.clone()))
                    .flex()
                    .gap(appearance.spacing(6.0))
                    .child(div().flex_none().child("•"))
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_normal()
                            .child(SharedString::from(text.clone())),
                    ),
                NotesBlock::Paragraph(text) => div()
                    .id(id)
                    .role(accesskit::Role::Label)
                    .aria_value(SharedString::from(text.clone()))
                    .whitespace_normal()
                    .child(SharedString::from(text.clone())),
            });
        }
        // Opening the release leaves the sheet open, so the person can come back to it.
        let url = self.release_url.clone();
        let link = Button::new(RELEASE_LINK_SELECTOR, RELEASE_LINK)
            .variant(ButtonVariant::Link)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .debug_selector(RELEASE_LINK_SELECTOR)
            .on_activate(move |_, _, cx| cx.open_url(&url));
        column.child(div().flex().pt(appearance.spacing(8.0)).child(link))
    }
}

#[cfg(test)]
mod tests {
    use spaceterm_ui::a11y_testing::{self, A11yTree};

    use super::*;

    const NOTES: &str = "### Features

- **panes:** Let panes swap places ([087ed1d](https://github.com/sadiksaifi/SpaceTerm/commit/087ed1d2))

### Fixes

- SpaceTerm uses less energy while idle ([86af166](https://github.com/sadiksaifi/SpaceTerm/commit/86af1669))
";

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            spaceterm_ui::ModalLayer::new(div().size_full())
        }
    }

    #[cfg(spaceterm_release_notes)]
    #[gpui::test]
    fn the_help_menu_action_opens_the_embedded_notes_in_the_workspace(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        cx.update(super::init);
        let (workspace, visual) = cx.add_window_view(|window, cx| {
            crate::ui::WorkspaceManager::new_with_adapters(
                std::env::temp_dir(),
                crate::ui::workspace_manager::tests::workspace_adapters(),
                crate::settings::sidebar::SidebarState::Expanded,
                window,
                cx,
            )
        });
        visual.update(|window, cx| {
            workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
        });
        visual.run_until_parked();

        let action = crate::platform::application_menu_model::help_menu()
            .items
            .into_iter()
            .find_map(|item| match item {
                gpui::MenuItem::Action { name, action, .. } if name == WHATS_NEW_TITLE => {
                    Some(action)
                }
                _ => None,
            })
            .expect("a build with notes offers them in Help");
        visual.update(|window, cx| window.dispatch_action(action, cx));
        visual.run_until_parked();

        let tree = A11yTree::read(visual);
        tree.node(&release_notes::embedded().unwrap().title());
        tree.node("View release on GitHub");
        tree.node("Done");
    }

    #[gpui::test]
    fn the_sheet_satisfies_the_desktop_dialog_policy(cx: &mut gpui::TestAppContext) {
        let notes = ReleaseNotes::parse("0.9.0", "2026-11-03", NOTES).unwrap();
        let body = cx.new(|_| NotesBody {
            blocks: Vec::new(),
            release_url: notes.github_url(),
        });
        assert_eq!(
            sheet(&notes, 2026, body).validate(&spaceterm_ui::ModalDesktopPolicy::mac_os()),
            Ok(())
        );
    }

    #[gpui::test]
    fn the_sheet_shows_the_notes_with_a_github_link_until_done(cx: &mut gpui::TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let notes = ReleaseNotes::parse("0.9.0", "2026-11-03", NOTES).unwrap();
        let (host, cx) = cx.add_window_view(|_, _| Host);
        let opened =
            cx.update(|window, cx| host.update(cx, |_, cx| present(&notes, 2026, window, cx)));
        assert!(opened);
        cx.run_until_parked();

        let tree = A11yTree::read(cx);
        for text in [
            "What's new in SpaceTerm 0.9.0",
            "Released 3 November",
            "panes: Let panes swap places",
            "SpaceTerm uses less energy while idle",
        ] {
            tree.text(text);
        }
        let headings = tree
            .with_role("Heading")
            .into_iter()
            .map(|heading| heading["aria"]["label"].clone())
            .collect::<Vec<_>>();
        assert_eq!(headings, ["Features", "Fixes"]);
        let link = tree.node(RELEASE_LINK);
        assert!(cx.update(|_, cx| cx.has_global::<OpenSheet>()));

        a11y_testing::perform(cx, link, accesskit::Action::Click);
        cx.run_until_parked();
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/sadiksaifi/SpaceTerm/releases/tag/v0.9.0")
        );
        let tree = A11yTree::read(cx);
        tree.text("Released 3 November");
        assert!(
            cx.update(|_, cx| cx.has_global::<OpenSheet>()),
            "the sheet stays open after the link opens the browser"
        );

        a11y_testing::perform(cx, tree.node("Done"), accesskit::Action::Click);
        cx.run_until_parked();
        assert!(
            A11yTree::read(cx)
                .find_text("Released 3 November")
                .is_none()
        );
        assert!(
            !cx.update(|_, cx| cx.has_global::<OpenSheet>()),
            "a closed sheet can open again"
        );
    }

    #[gpui::test]
    fn notes_count_as_read_once_the_sheet_opens_not_while_it_waits(cx: &mut gpui::TestAppContext) {
        use crate::updates::policy::UpdateHistory;
        use crate::updates::testing::RecordingAdapter;

        let mut adapter = RecordingAdapter::available();
        let mut history = UpdateHistory::default();
        history.launched("0.0.1", 0);
        adapter.history = history;
        let adapter = std::rc::Rc::new(adapter);
        cx.update(|cx| crate::updates::ApplicationUpdates::install(adapter.clone(), cx));
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let updates = cx.update(|cx| service(cx).expect("the update service is installed"));
        let unread = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| updates.read(cx).release_notes_unread())
        };
        let notes = ReleaseNotes::parse("0.9.0", "2026-11-03", NOTES).unwrap();
        let (host, cx) = cx.add_window_view(|_, _| Host);
        assert!(unread(cx), "this launch came by an update");

        cx.update(|window, cx| {
            host.update(cx, |_, cx| {
                super::super::present_up_to_date(window, cx);
                assert!(present(&notes, 2026, window, cx));
            })
        });
        cx.run_until_parked();
        let tree = A11yTree::read(cx);
        assert!(tree.find_text("Released 3 November").is_none());
        assert!(unread(cx), "a sheet waiting behind another stays unread");

        a11y_testing::perform(cx, tree.node("OK"), accesskit::Action::Click);
        cx.run_until_parked();
        A11yTree::read(cx).text("Released 3 November");
        assert!(!unread(cx), "an open sheet is read");
    }
}
