use super::tests::{attach, command, host, size};
use super::*;
use crate::terminal::{metadata::*, server_metadata};
use std::time::Instant;

fn exchange(host: &mut DevelopmentPaneHost, lease: Lease, request: Request) -> Reply {
    // Exercise the serialized server/client boundary rather than injecting client metadata.
    let request = serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    let reply = host.request([1; 32], lease.connection, request);
    serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap()
}
fn await_metadata(
    host: &mut DevelopmentPaneHost,
    lease: Lease,
    title: &str,
) -> Arc<TerminalMetadataSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match exchange(
            host,
            lease,
            Request::Metadata {
                lease,
                revision: None,
            },
        ) {
            Reply::Metadata(wire) => {
                let metadata = server_metadata::decode(*wire, None, None).unwrap();
                if metadata.title.value.as_ref() == title {
                    return metadata;
                }
            }
            Reply::Unchanged => {}
            reply => panic!("metadata exchange refused: {reply:?}"),
        }
        assert!(Instant::now() < deadline, "metadata title did not arrive");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn metadata_only_changes_reach_hidden_panes_and_reconnect_without_a_screen() {
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033]2;cargo test · api\\007'",
    );
    let first = await_metadata(&mut host, lease, "cargo test · api");
    assert!(matches!(
        exchange(
            &mut host,
            lease,
            Request::Metadata {
                lease,
                revision: Some(first.revision)
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        exchange(
            &mut host,
            lease,
            Request::PresentationDemand {
                lease,
                visible: false
            }
        ),
        Reply::Unchanged
    ));
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]2;hidden-title\\007\\033]7;file://localhost/tmp/api\\007\\033]9;4;1;42\\007'",
    );
    let hidden = await_metadata(&mut host, lease, "hidden-title");
    assert!(hidden.revision > first.revision);
    assert_eq!(hidden.directory.path.as_ref(), "/tmp/api");
    assert_eq!(hidden.progress, ProgressMetadata::Normal(42));
    assert!(hidden.confirmed_directory.is_none());
    assert_eq!(
        hidden.context.local_file_capabilities(),
        TerminalLocalFileCapabilities::Disabled
    );
    assert!(matches!(
        exchange(&mut host, lease, Request::Poll { lease }),
        Reply::Unchanged
    ));
    host.disconnected(lease.connection);
    let Reply::Attached { lease: replacement } = host.request(
        [1; 32],
        [2; 32],
        Request::Select {
            create: false,
            size: size(),
            resume: Some(lease.into()),
            style: None,
        },
    ) else {
        panic!("reconnect refused")
    };
    // A new connection requests current metadata even though its revision did not change.
    let current = await_metadata(&mut host, replacement, "hidden-title");
    assert_eq!(current.directory, hidden.directory);
    assert_eq!(current.progress, hidden.progress);
    assert!(matches!(
        exchange(
            &mut host,
            lease,
            Request::Metadata {
                lease,
                revision: None
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}

#[test]
fn presentation_roundtrip_keeps_command_identity_glyph_and_activity_without_authority() {
    use crate::terminal::title::TitleGlyph;
    let epoch = Instant::now();
    let secret = "prompt-owner-private-fixture";
    let context = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/srv/api",
        LocalMachine::new(Some("alice"), Some("build.test"), Some("/home/alice")),
    )
    .with_prompt_owner(PromptOwner::fixture(secret));
    let mut tracker = MetadataTracker::new_with_context(context, "shell", epoch);
    tracker.apply_semantic_prompt("C;cmdline=cargo%20test", epoch);
    tracker.set_reported_title("✳ cargo test · api", epoch);
    tracker.set_reported_title("⠋ cargo test · api", epoch + Duration::from_millis(10));
    tracker.set_reported_title("⠙ cargo test · api", epoch + Duration::from_millis(250));
    tracker.set_reported_directory("file://localhost/srv/api");
    tracker.apply_semantic_prompt(&format!("A;spaceterm={secret}"), epoch);
    // Running state is independent of a reported terminal title.
    tracker.apply_semantic_prompt("C;cmdline=cargo%20test", epoch + Duration::from_millis(251));
    tracker.set_reported_title("✳ cargo test · api", epoch + Duration::from_millis(260));
    tracker.set_reported_title("⠋ cargo test · api", epoch + Duration::from_millis(270));
    tracker.set_reported_title("⠙ cargo test · api", epoch + Duration::from_millis(510));
    let snapshot = tracker.snapshot();
    assert!(snapshot.title_activity);
    let encoded = metadata::encode(&snapshot);
    let bytes = serde_json::to_vec(&Reply::Metadata(Box::new(encoded))).unwrap();
    let json = std::str::from_utf8(&bytes).unwrap();
    assert!(!json.contains(secret));
    assert!(!json.contains("confirmed_directory"));
    assert!(!json.contains("prompt_owner"));
    assert!(!json.contains("local_file"));
    let Reply::Metadata(wire) = serde_json::from_slice(&bytes).unwrap() else {
        unreachable!()
    };
    let decoded = server_metadata::decode(*wire, None, None).unwrap();
    assert_eq!(
        decoded.command.as_ref().unwrap().line.as_ref(),
        "cargo test"
    );
    assert_eq!(
        decoded.command.as_ref().unwrap().state,
        CommandState::Running
    );
    assert_eq!(
        decoded.context.origin(),
        TerminalOrigin::Remote {
            user: Some("alice"),
            host: "build.test"
        }
    );
    assert_eq!(decoded.context.home(), Some("/home/alice"));
    assert!(decoded.title_activity);
    assert_eq!(decoded.title_glyph, TitleGlyph::Retained(Some("✳".into())));
    assert!(decoded.confirmed_directory.is_none());
    assert!(decoded.context.local_paths().is_none());
    assert_eq!(
        decoded.context.local_file_capabilities(),
        TerminalLocalFileCapabilities::Disabled
    );
    tracker.apply_semantic_prompt("D;7", epoch + Duration::from_secs(1));
    let decoded =
        server_metadata::decode(metadata::encode(&tracker.snapshot()), None, None).unwrap();
    assert!(
        matches!(decoded.command.as_ref().unwrap().state, CommandState::Finished { exit_status: Some(7), duration } if duration == Duration::from_millis(749))
    );
}

#[test]
fn encoder_sanitizes_bounds_and_client_rejects_malformed_presentation() {
    use spaceterm_server::pane::metadata as wire;
    let mut tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/srv/api",
        "shell",
        LocalMachine::default(),
        Instant::now(),
    );
    tracker.apply_semantic_prompt("C;cmdline=test", Instant::now());
    let mut snapshot = (*tracker.snapshot()).clone();
    snapshot.title.value = format!(
        "\u{061c}\u{200e}\u{200f}\u{202e}\u{2067}\r\n{}",
        "界".repeat(300)
    )
    .into();
    snapshot.command.as_mut().unwrap().line = format!("\u{1b}{}", "x".repeat(5000)).into();
    snapshot.directory.path = format!("/{}", "d".repeat(5000)).into();
    let encoded = metadata::encode(&snapshot);
    assert_eq!(encoded.title, "界".repeat(wire::MAX_TITLE_CHARS));
    assert_eq!(
        encoded.command.as_ref().unwrap().line,
        "x".repeat(wire::MAX_COMMAND_CHARS)
    );
    assert_eq!(encoded.directory.chars().count(), wire::MAX_DIRECTORY_CHARS);
    encoded.validate().unwrap();
    let mut bad = encoded.clone();
    bad.title.push('\u{1b}');
    assert!(server_metadata::decode(bad, None, None).is_err());
    let mut bad = encoded.clone();
    bad.command.as_mut().unwrap().line.push('x');
    assert!(server_metadata::decode(bad, None, None).is_err());
    let mut bad = encoded;
    bad.progress = wire::Progress::Normal(101);
    assert!(server_metadata::decode(bad, None, None).is_err());
}

#[test]
fn inventory_observers_receive_unpainted_pane_metadata_without_claiming_control() {
    fn inventory(
        host: &mut SharedWorkspaceHost,
        connection: u8,
    ) -> spaceterm_server::hierarchy::Inventory {
        let request: Request =
            serde_json::from_slice(&serde_json::to_vec(&Request::Inventory).unwrap()).unwrap();
        let reply = host.request([2; 32], [connection; 32], request);
        let reply: Reply = serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
        let Reply::Inventory { inventory, .. } = reply else {
            panic!("observer inventory refused")
        };
        inventory
    }
    fn wait(
        host: &mut SharedWorkspaceHost,
        connection: u8,
        title: &str,
    ) -> spaceterm_server::hierarchy::Inventory {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let inventory = inventory(host, connection);
            if inventory.workspaces[0].tabs[0].panes[0]
                .presentation
                .as_ref()
                .is_some_and(|p| p.title == title)
            {
                return inventory;
            }
            assert!(
                Instant::now() < deadline,
                "unpainted Pane metadata not published to observer"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let dormant = inventory(&mut host, 20);
    let pane = dormant.workspaces[0].tabs[0].root_pane;
    assert!(
        dormant.workspaces[0].tabs[0].panes[0]
            .presentation
            .is_none()
    );
    assert!(
        dormant
            .close
            .unwrap()
            .sessions
            .iter()
            .all(|p| p.session.is_none())
    );
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("owner selection refused")
    };
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::PresentationDemand {
                lease,
                visible: false
            }
        ),
        Reply::Unchanged
    ));
    // Neither owner nor observer requests a Screen or a metadata lease exchange.
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033]2;unpainted-title\\007\\033]7;file://localhost/tmp/api\\007\\033]9;4;1;42\\007'",
    );
    let first = wait(&mut host, 20, "unpainted-title");
    let snapshot = first.workspaces[0].tabs[0].panes[0]
        .presentation
        .as_ref()
        .unwrap();
    let decoded = server_metadata::decode((**snapshot).clone(), None, None).unwrap();
    assert_eq!(decoded.directory.path.as_ref(), "/tmp/api");
    assert_eq!(decoded.progress, ProgressMetadata::Normal(42));
    assert!(decoded.confirmed_directory.is_none());
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]2;metadata-only-update\\007'",
    );
    let changed = wait(&mut host, 20, "metadata-only-update");
    assert!(changed.revision > first.revision);
    host.disconnected([20; 32]);
    let reconnected = wait(&mut host, 21, "metadata-only-update");
    assert_eq!(
        reconnected.workspaces[0].tabs[0].panes[0].presentation,
        changed.workspaces[0].tabs[0].panes[0].presentation
    );
    assert!(
        matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Key {
                    lease,
                    key: Key {
                        physical: PhysicalKey::Unidentified as u32,
                        action: 0,
                        logical: "text-input".into(),
                        text: Some("true".into()),
                        unshifted: None,
                        modifiers: 0,
                        consumed: 0,
                        option_as_alt: false,
                    }
                }
            ),
            Reply::Unchanged
        ),
        "observing metadata must not replace the controller"
    );
    assert!(matches!(
        host.request([1; 32], [1; 32], Request::Poll { lease }),
        Reply::Unchanged
    ));
    host.shutdown().unwrap();
}

#[test]
fn presentation_is_excluded_from_retained_organization_and_maximum_inventory_fits_a_frame() {
    use spaceterm_server::hierarchy::{MAX_PANES, State};
    let organization = Organization::first("/tmp".into()).unwrap();
    let mut state = State::from_legacy(&organization);
    let tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/tmp",
        "shell",
        LocalMachine::default(),
        Instant::now(),
    );
    let mut metadata = metadata::encode(&tracker.snapshot());
    state.present_presentation(organization.pane, Some(Box::new(metadata.clone())));
    let retained = state.envelope("/tmp".as_ref()).unwrap();
    assert!(
        !serde_json::to_string(&retained)
            .unwrap()
            .contains("presentation")
    );
    metadata.title = "界".repeat(pane::metadata::MAX_TITLE_CHARS);
    metadata.directory = "界".repeat(pane::metadata::MAX_DIRECTORY_CHARS);
    metadata.user = Some("界".repeat(pane::metadata::MAX_MACHINE_CHARS));
    metadata.host = metadata.user.clone();
    metadata.home = Some(metadata.directory.clone());
    metadata.command = Some(pane::metadata::Command {
        line: "界".repeat(pane::metadata::MAX_COMMAND_CHARS),
        state: pane::metadata::CommandState::Running,
    });
    metadata.title_glyph = pane::metadata::TitleGlyph::Retained(Some(metadata.title.clone()));
    metadata.validate().unwrap();
    let mut inventory = state.inventory;
    let mut exemplar = inventory.workspaces[0].tabs[0].panes[0].clone();
    exemplar.presentation = Some(Box::new(metadata));
    inventory.workspaces[0].tabs[0].panes = vec![exemplar; MAX_PANES];
    assert!(
        serde_json::to_vec(&Reply::Inventory {
            inventory,
            next_sequence: 1
        })
        .unwrap()
        .len()
            < MAX_TERMINAL_FRAME
    );
}

#[test]
fn ssh_presentation_preserves_destination_alias_and_spelled_account() {
    let tracker = MetadataTracker::new(
        LocalPathSemantics::Posix,
        "/srv/api",
        "shell",
        LocalMachine::new(
            Some("discovered"),
            Some("host.internal"),
            Some("/home/discovered"),
        ),
        Instant::now(),
    );
    for (destination, expected_user) in [
        ("build-alias", "discovered"),
        ("selected@build-alias", "selected"),
    ] {
        let context = RemoteTerminalMetadataContext::new(
            crate::domain::SshDestination::new(destination.into()).unwrap(),
            crate::domain::RemoteDirectory::home(),
        );
        let decoded =
            server_metadata::decode(metadata::encode(&tracker.snapshot()), None, Some(&context))
                .unwrap();
        assert_eq!(
            decoded.context.origin(),
            TerminalOrigin::Remote {
                user: Some(expected_user),
                host: "build-alias"
            }
        );
        assert_eq!(decoded.context.home(), Some("/home/discovered"));
        assert_eq!(
            decoded.context.local_file_capabilities(),
            TerminalLocalFileCapabilities::Disabled
        );
    }
}
