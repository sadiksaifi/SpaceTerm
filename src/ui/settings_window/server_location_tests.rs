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
            "Use only the {repository} and {branch} placeholders.",
        ),
        (
            "~/wt/{repository}",
            "Include {branch}, so each Worktree gets its own folder.",
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
        Some((
            "The server can\u{2019}t write to this folder.",
            CaptionTone::Error
        ))
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
fn a_newer_value_reaches_an_unedited_field_and_an_older_one_never_does() {
    let mut model = connected("~/first/{branch}", 4);

    assert_eq!(
        model.received(settings("~/newer/{branch}", 5)).as_deref(),
        Some("~/newer/{branch}")
    );
    assert_eq!(model.received(settings("~/older/{branch}", 3)), None);
    assert_eq!(model.draft, "~/newer/{branch}");
}
