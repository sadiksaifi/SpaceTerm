use super::tests::{command, host, size};
use super::*;
use crate::terminal::{metadata::*, server_metadata};
use std::time::Instant;

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
    let mut prototype = host();
    prototype.factory = NativeTerminalSessionFactory::new(
        native_pty_factory(),
        ShellLaunchPlanner::new(
            "/bin/sh".into(),
            std::path::PathBuf::new(),
            ShellIntegrationMode::Disabled,
            ShellEnvironment::default(),
            ShellIntegrationPolicy {
                supported: true,
                path_list_separator: ':',
                fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
            },
            &[],
        ),
        local_filesystem(),
        LocalMachine::new(Some("alice"), Some("build.test"), Some("/home/alice")),
    );
    let mut host = SharedWorkspaceHost::new(prototype);
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
    // Publish the source directory before the command starts so completion changes only metadata.
    // Neither client requests a Screen. Only the owner sends input; the observer reads Inventory.
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033]7;file://localhost/tmp/api\\007\\033]133;C;cmdline=cargo%%20test\\007\\033]2;✳ cargo test · api\\007\\033]9;4;1;42\\007'",
    );
    wait(&mut host, 20, "✳ cargo test · api");
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]2;⠋ cargo test · api\\007'",
    );
    wait(&mut host, 20, "⠋ cargo test · api");
    // Observe each frame before advancing so a delayed PTY reader cannot merge their times.
    std::thread::sleep(Duration::from_millis(250));
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]2;⠙ cargo test · api\\007'",
    );
    let first = wait(&mut host, 20, "⠙ cargo test · api");
    let snapshot = first.workspaces[0].tabs[0].panes[0]
        .presentation
        .as_ref()
        .unwrap();
    let decoded = server_metadata::decode((**snapshot).clone(), None, None).unwrap();
    assert_eq!(decoded.directory.path.as_ref(), "/tmp/api");
    assert_eq!(decoded.progress, ProgressMetadata::Normal(42));
    assert_eq!(
        decoded.command.as_ref().unwrap().line.as_ref(),
        "cargo test"
    );
    assert_eq!(
        decoded.command.as_ref().unwrap().state,
        CommandState::Running
    );
    assert!(decoded.title_activity);
    assert_eq!(
        decoded.title_glyph,
        crate::terminal::title::TitleGlyph::Retained(Some("✳".into()))
    );
    assert_eq!(
        decoded.context.origin(),
        TerminalOrigin::Remote {
            user: Some("alice"),
            host: "build.test"
        }
    );
    assert_eq!(decoded.context.home(), Some("/home/alice"));
    assert!(decoded.context.local_paths().is_none());
    assert_eq!(
        decoded.context.local_file_capabilities(),
        TerminalLocalFileCapabilities::Disabled
    );
    assert!(decoded.confirmed_directory.is_none());
    command(
        &mut host,
        1,
        lease,
        "printf '\\033]133;D;7\\007\\033]2;metadata-only-update\\007'",
    );
    let changed = wait(&mut host, 20, "metadata-only-update");
    assert_eq!(
        changed.revision, first.revision,
        "presentation must not reconcile or save hierarchy"
    );
    assert_ne!(
        changed, first,
        "the inventory worker detects presentation by full content"
    );
    let finished = server_metadata::decode(
        (**changed.workspaces[0].tabs[0].panes[0]
            .presentation
            .as_ref()
            .unwrap())
        .clone(),
        None,
        None,
    )
    .unwrap();
    assert!(
        matches!(finished.command.as_ref().unwrap().state, CommandState::Finished { exit_status: Some(7), duration } if duration >= Duration::from_millis(250))
    );
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

#[test]
fn encoded_snapshot_excludes_owning_shell_secrets_and_authority() {
    let epoch = Instant::now();
    let secret = "prompt-owner-private-fixture";
    let context = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/srv/api",
        LocalMachine::default(),
    )
    .with_prompt_owner(PromptOwner::fixture(secret));
    let mut tracker = MetadataTracker::new_with_context(context, "shell", epoch);
    tracker.set_reported_directory("file://localhost/srv/api");
    tracker.apply_semantic_prompt(&format!("A;spaceterm={secret}"), epoch);
    assert_eq!(
        tracker.snapshot().confirmed_directory.as_deref(),
        Some("/srv/api")
    );
    let snapshot = metadata::encode(&tracker.snapshot());
    let json = serde_json::to_string(&snapshot).unwrap();
    for forbidden in [secret, "confirmed_directory", "prompt_owner", "local_file"] {
        assert!(!json.contains(forbidden));
    }
    let snapshot = serde_json::from_str(&json).unwrap();
    let decoded = server_metadata::decode(snapshot, None, None).unwrap();
    assert!(decoded.confirmed_directory.is_none());
    assert!(decoded.context.local_paths().is_none());
    assert_eq!(
        decoded.context.local_file_capabilities(),
        TerminalLocalFileCapabilities::Disabled
    );
}
