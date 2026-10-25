use super::*;

fn settings(template: &str, version: u64) -> Settings {
    Settings {
        template: template.to_owned(),
        version,
    }
}

/// A field showing a connected server's value.
fn connected(template: &str, version: u64) -> LocationModel {
    let mut model = LocationModel::default();
    model.received(settings(template, version));
    model
}

#[test]
fn each_typed_problem_appears_under_the_field_as_the_person_types() {
    let mut model = connected("~/.worktrees/{repository}/{branch}", 0);

    for (text, message) in [
        ("", "Enter a location."),
        (
            "~/wt/{repo}/{branch}",
            "Use only {repository} and {branch} placeholders.",
        ),
        (
            "~/wt/{repository}",
            "Include {branch} for separate Worktree directories.",
        ),
        ("wt/{branch}", "Start the location with ~/ or /."),
    ] {
        assert_eq!(
            model.edited(text),
            None,
            "{text:?} needs no server to find its problem"
        );
        assert_eq!(model.message(), Some((message, CaptionTone::Error)));
        assert_eq!(model.commit(), None, "{text:?} is never written");
    }
}

#[test]
fn an_unwritable_folder_appears_only_for_the_text_the_server_checked() {
    let mut model = connected("~/.worktrees/{repository}/{branch}", 0);
    let earlier = "/System/{branch}";
    let current = "/Volumes/Locked/{branch}";

    assert_eq!(model.edited(earlier).as_deref(), Some(earlier));
    assert_eq!(model.edited(current).as_deref(), Some(current));
    model.validated(
        earlier,
        Err(Error::Template(WorktreePathTemplateError::NotWritable)),
    );
    assert_eq!(
        model.message(),
        None,
        "an answer about earlier text lands nowhere"
    );

    model.validated(
        current,
        Err(Error::Template(WorktreePathTemplateError::NotWritable)),
    );
    assert_eq!(
        model.message(),
        Some(("The server can't write here.", CaptionTone::Error))
    );
    assert_eq!(model.commit(), None);
}

#[test]
fn a_draft_is_written_once_against_the_version_it_edits() {
    let mut model = connected("~/.worktrees/{repository}/{branch}", 3);
    model.edited("~/elsewhere/{repository}/{branch}");

    assert_eq!(
        model.commit(),
        Some(settings("~/elsewhere/{repository}/{branch}", 3))
    );
    assert_eq!(model.commit(), None, "one write is in flight");

    assert!(!model.committed(settings("~/elsewhere/{repository}/{branch}", 3), Ok(())));
    assert_eq!(
        model.commit(),
        None,
        "the saved text is the server's value now"
    );
    assert!(!model.has_draft());
}

#[test]
fn a_concurrent_change_reloads_the_other_devices_value_and_says_so() {
    let mut model = connected("~/first/{branch}", 0);
    model.edited("~/mine/{branch}");
    assert_eq!(
        model.received(settings("~/theirs/{branch}", 1)),
        None,
        "a read never replaces what the person is typing"
    );
    let write = model.commit().unwrap();
    assert_eq!(write.version, 0);

    assert!(model.committed(write, Err(WriteError::Server(Error::Changed))));
    assert_eq!(
        model
            .changed_elsewhere(Some(settings("~/theirs/{branch}", 1)))
            .as_deref(),
        Some("~/theirs/{branch}")
    );
    assert_eq!(
        model.message(),
        Some((
            "Another device changed this location.",
            CaptionTone::Warning
        ))
    );
    assert_eq!(
        model.commit(),
        None,
        "the person's draft is never written over the other device's change"
    );
}

#[test]
fn a_disconnected_server_keeps_its_last_value_and_cannot_change() {
    let mut model = connected("~/first/{branch}", 2);
    model.edited("~/draft/{branch}");

    assert_eq!(model.disconnected().as_deref(), Some("~/first/{branch}"));
    assert!(!model.editable());
    assert_eq!(model.commit(), None);

    assert_eq!(model.received(settings("~/first/{branch}", 2)), None);
    assert!(model.editable(), "reconnecting allows changes again");
}

#[test]
fn a_remembered_value_shows_until_the_server_answers_and_never_saves() {
    let mut model = LocationModel::default();

    assert_eq!(
        model
            .remembered(Some("~/kept/{branch}".to_owned()))
            .as_deref(),
        Some("~/kept/{branch}")
    );
    assert!(!model.editable());
    assert_eq!(model.commit(), None);
    assert_eq!(
        model.received(settings("~/now/{branch}", 7)).as_deref(),
        Some("~/now/{branch}"),
        "the server's own answer replaces the remembered value"
    );
    model.edited("~/typing/{branch}");
    assert_eq!(
        model.remembered(Some("~/late/{branch}".to_owned())),
        None,
        "facts arriving late never replace the server's value or a draft"
    );
    assert_eq!(model.draft, "~/typing/{branch}");
}

#[test]
fn a_newer_value_reaches_an_unedited_field_and_an_older_one_never_does() {
    let mut model = connected("~/first/{branch}", 4);

    assert_eq!(
        model.received(settings("~/newer/{branch}", 5)).as_deref(),
        Some("~/newer/{branch}")
    );
    assert_eq!(model.received(settings("~/older/{branch}", 3)), None);
    assert_eq!(model.draft, "~/newer/{branch}");
}

/// "Git on <server>" against a paired server whose reads and writes a test scripts.
mod presented {
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::{TestAppContext, VisualTestContext};
    use spaceterm_server::hierarchy::{Mutation, Outcome};
    use spaceterm_ui::a11y_testing::A11yTree;

    use super::super::super::server_list::ServerChoice;
    use super::super::super::server_list::tests::{
        Paired, Servers, click, connected_paired, disconnected, open_with, settle,
    };
    use super::*;
    use crate::repository_status::ToolVersion;
    use crate::server_context::ServerKey;
    use crate::ui::server_worktrees::testing::{ScriptedReads, present_direct};
    use crate::ui::shared_workspaces::testing::ScriptedEdits;

    const GIT_SECTION: &str = "settings-navigation-settings-section-git";
    const SAVED: &str = "~/penguin/{repository}/{branch}";

    fn penguin(paired: &Paired) -> ServerKey {
        ServerChoice::Direct(paired.1[0]).key()
    }

    /// Opens Settings with penguin saved and chosen, whatever the selection would fall back to.
    fn open_penguin(paired: Paired, cx: &mut TestAppContext) -> Servers<'_> {
        let key = penguin(&paired);
        cx.update(|cx| server_selection::select(key, cx));
        open_with(false, paired, cx)
    }

    /// Opens Git with penguin connected and its writer answering from `reads`.
    fn present_penguin(
        reads: Arc<ScriptedReads>,
        cx: &mut TestAppContext,
    ) -> (Servers<'_>, Rc<ScriptedEdits>) {
        let paired = connected_paired(&["penguin"], cx);
        let servers = open_penguin(paired, cx);
        let edits = Rc::new(ScriptedEdits::new(None));
        edits.set_worktree_writes(true);
        let writer = edits.clone();
        let route = servers.routes[0];
        servers.cx.update(|_, cx| {
            present_direct(route, writer, reads, cx);
        });
        click(GIT_SECTION, servers.cx);
        (servers, edits)
    }

    fn location_field(cx: &mut VisualTestContext) -> String {
        A11yTree::read(cx).node("New Worktree Location")["aria"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn says(text: &str, cx: &mut VisualTestContext) -> bool {
        A11yTree::read(cx).find_text(text).is_some()
    }

    /// Replaces the field's text as a person typing it does.
    fn type_location(text: &str, cx: &mut VisualTestContext) {
        click("settings-server-worktree-location", cx);
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input(text);
    }

    /// Lets the timers waiting on the field, the poll and a write fire.
    fn wait(duration: Duration, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    fn penguin_location(key: &ServerKey, cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| server_selection::facts(key, cx))
            .and_then(|facts| facts.worktree_location)
    }

    #[gpui::test]
    fn the_git_row_shows_the_selected_servers_git(cx: &mut TestAppContext) {
        let reads = ScriptedReads::new(SAVED);
        reads.set_git(GitToolStatus::Ready(ToolVersion {
            major: 2,
            minor: 47,
            patch: 0,
        }));

        let (servers, _) = present_penguin(reads.clone(), cx);
        settle(servers.cx, |cx| says("2.47.0", cx));

        assert!(says("Installed", servers.cx));
        assert!(
            reads.reads().iter().any(|read| matches!(read, Read::Git)),
            "the Git row asks the owning server, not this computer"
        );
    }

    #[gpui::test]
    fn a_folder_the_server_cannot_write_to_is_named_under_the_field_and_never_saved(
        cx: &mut TestAppContext,
    ) {
        let reads = ScriptedReads::new(SAVED);
        reads.refuse_writes_under("/Volumes/Locked");
        let (servers, edits) = present_penguin(reads, cx);
        settle(servers.cx, |cx| location_field(cx) == SAVED);

        type_location("/Volumes/Locked/{branch}", servers.cx);
        wait(Duration::from_millis(400), servers.cx);

        assert!(says("The server can't write here.", servers.cx));
        servers.cx.simulate_keystrokes("enter");
        servers.cx.run_until_parked();
        assert!(edits.submitted().is_empty());
    }

    #[gpui::test]
    fn a_concurrent_change_reloads_the_field_and_keeps_the_other_devices_value(
        cx: &mut TestAppContext,
    ) {
        let reads = ScriptedReads::new(SAVED);
        let (servers, edits) = present_penguin(reads.clone(), cx);
        settle(servers.cx, |cx| location_field(cx) == SAVED);

        type_location("~/mine/{branch}", servers.cx);
        reads.change("~/theirs/{branch}");
        wait(Duration::from_secs(1), servers.cx);
        assert_eq!(location_field(servers.cx), "~/mine/{branch}");
        servers.cx.simulate_keystrokes("enter");
        servers.cx.run_until_parked();
        assert_eq!(
            edits.submitted(),
            vec![Mutation::Worktree(Operation::SetLocation {
                version: 0,
                template: "~/mine/{branch}".into(),
            })]
        );
        edits.answer(Outcome::WorktreeFailed(Error::Changed));
        wait(Duration::from_millis(100), servers.cx);

        assert_eq!(location_field(servers.cx), "~/theirs/{branch}");
        assert!(says("Another device changed this location.", servers.cx));
        assert_eq!(
            edits.submitted().len(),
            1,
            "the draft is never written over the other device's change"
        );
    }

    #[gpui::test]
    fn a_server_that_is_not_connected_shows_its_last_known_git_settings(cx: &mut TestAppContext) {
        // As after a relaunch: the record keeps what penguin last answered, and nothing
        // presents penguin.
        let paired = disconnected(&["penguin"]);
        let key = penguin(&paired);
        cx.update(|cx| {
            server_selection::remember(
                key,
                LastKnownFacts {
                    observed_at: 10,
                    git: GitToolStatus::Ready(ToolVersion {
                        major: 2,
                        minor: 47,
                        patch: 0,
                    }),
                    worktree_location: Some("~/kept/{branch}".to_owned()),
                    ..LastKnownFacts::default()
                },
                cx,
            );
        });
        let servers = open_penguin(paired, cx);
        click(GIT_SECTION, servers.cx);

        assert_eq!(location_field(servers.cx), "~/kept/{branch}");
        assert!(says("2.47.0", servers.cx));
        type_location("~/elsewhere/{branch}", servers.cx);
        assert_eq!(
            location_field(servers.cx),
            "~/kept/{branch}",
            "a server that isn't connected can't be changed"
        );
    }

    #[gpui::test]
    fn last_known_facts_that_load_after_the_server_is_chosen_show_at_once(cx: &mut TestAppContext) {
        let paired = disconnected(&["penguin"]);
        let key = penguin(&paired);
        let servers = open_penguin(paired, cx);
        click(GIT_SECTION, servers.cx);
        assert_eq!(location_field(servers.cx), "");

        // The retained record finishes loading while Git already shows penguin.
        servers.cx.update(|_, cx| {
            server_selection::remember(
                key,
                LastKnownFacts {
                    observed_at: 10,
                    git: GitToolStatus::Ready(ToolVersion {
                        major: 2,
                        minor: 47,
                        patch: 0,
                    }),
                    worktree_location: Some("~/late/{branch}".to_owned()),
                    ..LastKnownFacts::default()
                },
                cx,
            );
        });
        servers.cx.run_until_parked();

        assert_eq!(location_field(servers.cx), "~/late/{branch}");
        assert!(says("2.47.0", servers.cx));
    }

    #[gpui::test]
    fn the_location_read_is_retained_with_the_servers_other_last_known_facts(
        cx: &mut TestAppContext,
    ) {
        // The connection's own observation stamps the time it looks. One newer than any this run
        // makes keeps the record as its owner wrote it, so only retaining the Git settings can
        // change it.
        let observed_at = 4_102_444_800;
        let paired = connected_paired(&["penguin"], cx);
        let key = penguin(&paired);
        cx.update(|cx| {
            server_selection::remember(
                key.clone(),
                LastKnownFacts {
                    observed_at,
                    machine: Some("Penguin".to_owned()),
                    ..LastKnownFacts::default()
                },
                cx,
            );
        });
        let servers = open_penguin(paired, cx);
        let route = servers.routes[0];
        servers.cx.update(|_, cx| {
            present_direct(
                route,
                Rc::new(ScriptedEdits::new(None)),
                ScriptedReads::new(SAVED),
                cx,
            );
        });
        click(GIT_SECTION, servers.cx);
        settle(servers.cx, |cx| {
            penguin_location(&key, cx).as_deref() == Some(SAVED)
        });

        let facts = servers
            .cx
            .update(|_, cx| server_selection::facts(&key, cx))
            .unwrap();
        assert_eq!(
            (facts.machine.as_deref(), facts.observed_at),
            (Some("Penguin"), observed_at)
        );
    }
}
