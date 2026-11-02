use super::publish::Observed;
use super::*;
use spaceterm_server::protocol::PaneEvent;
use spaceterm_server::protocol::screen::ScreenUpdate;
use std::time::{Duration, Instant};

#[test]
fn a_shell_that_never_started_allows_pane_close_and_server_shutdown() {
    use crate::platform::native_pty::NativePtyAdapterConstructionFailure as Failure;
    struct Unstarted(Failure);
    impl NativePtyAdapterFactory for Unstarted {
        fn create(
            &self,
            _: crate::platform::shell_launch::PreparedShellLaunch,
            _: crate::platform::native_pty::NativePtySize,
        ) -> Result<crate::platform::native_pty::NativePtyAdapterParts, Failure> {
            Err(self.0)
        }
    }
    // A spawn fails this way when the Pane's directory disappears after validation.
    for failure in [
        Failure::EnvironmentUnavailable,
        Failure::ResourceCreationFailed,
        Failure::ResourceConfigurationFailed,
        Failure::ProcessStartFailed,
    ] {
        for close_pane in [true, false] {
            let mut host = ServerPaneHost::local_with_pty_factory(
                Arc::new(Unstarted(failure)),
                Arc::default(),
            )
            .unwrap();
            let lease = attach(&mut host, 1, true);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let pane = host.pane.as_mut().unwrap();
                pane.drain();
                if pane.failed {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "startup failure was not reported"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            if close_pane {
                let reply = host.request([1; 32], [1; 32], Request::Close { lease });
                assert!(matches!(reply, Reply::Closed), "{failure:?}: {reply:?}");
            }
            assert_eq!(host.shutdown(), Ok(()), "{failure:?}");
        }
    }
}

/// A host the helpers below drive: one Pane's host, or a host of Workspaces.
pub(super) trait Driven {
    fn send(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply;
    fn watch(&mut self, device: [u8; 32], lease: &Lease) -> Observed;
    fn page(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        lease: &Lease,
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal>;
}

impl<H: HostCommands> Driven for H {
    fn send(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.request(device, connection, request)
    }
    fn watch(&mut self, device: [u8; 32], lease: &Lease) -> Observed {
        self.observe(device, lease)
    }
    fn page(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        lease: &Lease,
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal> {
        self.scrollback_page(device, connection, lease, read, reply)
    }
}

impl Driven for ServerPaneHost {
    fn send(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.request(device, connection, request)
    }
    fn watch(&mut self, device: [u8; 32], lease: &Lease) -> Observed {
        self.observe(device, lease)
    }
    fn page(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        lease: &Lease,
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal> {
        self.scrollback_page(device, connection, lease, read, reply)
    }
}

pub(super) fn native_command(host: &mut impl Driven, lease: Lease, generation: u64, text: &str) {
    assert!(matches!(
        input(host, lease, generation, text_key(text, false)),
        Reply::Unchanged
    ));
    let mut enter = match text_key("", false) {
        InteractiveInput::Key(key) => key,
        _ => unreachable!(),
    };
    enter.physical = PhysicalKey::Enter as u32;
    enter.logical = "enter".into();
    enter.text = None;
    assert!(matches!(
        input(host, lease, generation, InteractiveInput::Key(enter)),
        Reply::Unchanged
    ));
}

pub(super) fn focus(host: &mut impl Driven, lease: Lease, generation: u64, target: FocusTarget) {
    assert!(matches!(
        host.send(
            [1; 32],
            lease.connection,
            Request::Focus {
                lease,
                generation,
                target
            }
        ),
        Reply::Unchanged
    ));
}

pub(super) fn input(
    host: &mut impl Driven,
    lease: Lease,
    generation: u64,
    input: InteractiveInput,
) -> Reply {
    host.send(
        [1; 32],
        lease.connection,
        Request::Input {
            lease,
            generation,
            input,
        },
    )
}

pub(super) fn text_key(text: &str, ime: bool) -> InteractiveInput {
    InteractiveInput::Key(Key {
        physical: PhysicalKey::Unidentified as u32,
        action: 0,
        logical: if ime {
            "input-method-commit"
        } else {
            "text-input"
        }
        .into(),
        text: Some(text.into()),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    })
}

pub(super) fn workspace_inventory(
    host: &mut WorkspaceHost,
) -> spaceterm_server::hierarchy::Inventory {
    let Reply::Inventory { inventory, .. } = host.request([2; 32], [2; 32], Request::Inventory)
    else {
        panic!("inventory unavailable")
    };
    inventory
}

pub(super) fn create_workspace(host: &mut WorkspaceHost, sequence: u64) -> [u8; 32] {
    use spaceterm_server::hierarchy::{Mutation, Outcome};
    let Reply::Outcome(result) = host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence,
            mutation: Mutation::CreateWorkspace {
                name: "Build".into(),
                directory: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    ) else {
        panic!("creation unavailable")
    };
    let Outcome::Accepted { target } = result.outcome else {
        panic!("creation refused")
    };
    target
}

#[test]
fn kill_ends_another_controllers_work_and_replays_without_touching_survivors() {
    use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
    let mut host = WorkspaceHost::new(host());
    let workspace = create_workspace(&mut host, 1);
    let other = create_workspace(&mut host, 2);
    let inventory = workspace_inventory(&mut host);
    let pane = inventory.workspaces[0].tabs[0].root_pane;
    let other_pane = inventory.workspaces[1].tabs[0].root_pane;
    let start = |pane| Request::SelectPane {
        pane,
        explicit: true,
        size: size(),
        style: None,
    };
    let Reply::Attached { lease } = host.request([1; 32], [1; 32], start(pane)) else {
        panic!()
    };
    let Reply::Attached { lease: survivor } = host.request([1; 32], [1; 32], start(other_pane))
    else {
        panic!()
    };
    let target = CloseTarget::KillWorkspace { workspace };
    let confirmation = workspace_inventory(&mut host)
        .close_confirmation(target)
        .expect("server supplies close authority");
    let close = Mutation::Close {
        target,
        confirmation,
    };
    for _ in 0..2 {
        let reply = host.request(
            [2; 32],
            [2; 32],
            Request::Mutate {
                sequence: 1,
                mutation: close.clone(),
            },
        );
        assert!(
            matches!(reply, Reply::Outcome(result) if result.outcome == Outcome::Accepted { target: workspace })
        );
    }
    // The tick after the retained deletion ends its work.
    host.tick(Instant::now());
    assert!(matches!(
        screen(&mut host, [1; 32], lease),
        Err(PaneEvent::Ended)
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Resize {
                lease: survivor,
                size: size()
            }
        ),
        Reply::Unchanged
    ));
    let inventory = workspace_inventory(&mut host);
    assert_eq!(inventory.workspaces.len(), 1);
    assert_eq!(inventory.workspaces[0].id, other);
    host.shutdown().unwrap();
}

#[test]
fn close_rejects_a_session_started_after_confirmation_but_allows_takeover() {
    use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
    let mut host = WorkspaceHost::new(host());
    let workspace = create_workspace(&mut host, 1);
    let initial = workspace_inventory(&mut host);
    let tab = initial.workspaces[0].root_tab;
    let pane = initial.workspaces[0].tabs[0].root_pane;
    let target = CloseTarget::Tab { workspace, tab };
    let stale = initial.close_confirmation(target).unwrap();
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
        panic!()
    };
    assert!(
        matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close { target, confirmation: stale } }), Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation)
    );
    let confirmation = workspace_inventory(&mut host)
        .close_confirmation(target)
        .unwrap();
    assert!(matches!(
        host.request(
            [3; 32],
            [3; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None
            }
        ),
        Reply::Attached { .. }
    ));
    assert!(
        matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 2, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(o) if o.outcome == Outcome::Accepted { target: tab })
    );
    assert!(workspace_inventory(&mut host).workspaces.is_empty());
}

#[test]
fn close_rejects_membership_changes_and_confirmations_from_another_boot() {
    use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
    let mut host = WorkspaceHost::new(host());
    let workspace = create_workspace(&mut host, 1);
    let target = CloseTarget::KillWorkspace { workspace };
    let confirmation = workspace_inventory(&mut host)
        .close_confirmation(target)
        .unwrap();
    host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 2,
            mutation: Mutation::CreateTab {
                workspace,
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    );
    assert!(
        matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation)
    );
    let mut confirmation = workspace_inventory(&mut host)
        .close_confirmation(target)
        .unwrap();
    confirmation.boot = [0; 32];
    assert!(
        matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 2, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation)
    );
    assert_eq!(workspace_inventory(&mut host).workspaces[0].tabs.len(), 2);
}

#[test]
fn close_rejects_new_ancestor_deletion_after_a_sibling_closes() {
    use spaceterm_server::hierarchy::{Axis, CloseTarget, Mutation, Outcome};
    for close_pane in [true, false] {
        let mut host = WorkspaceHost::new(host());
        let workspace = create_workspace(&mut host, 1);
        let initial = workspace_inventory(&mut host);
        let tab = initial.workspaces[0].root_tab;
        let pane = initial.workspaces[0].tabs[0].root_pane;
        let mutation = if close_pane {
            Mutation::SplitPane {
                workspace,
                tab,
                source: pane,
                axis: Axis::Down,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            }
        } else {
            Mutation::CreateTab {
                workspace,
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            }
        };
        let Reply::Outcome(result) = host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation,
            },
        ) else {
            panic!()
        };
        let Outcome::Accepted { target: sibling } = result.outcome else {
            panic!()
        };
        let target = if close_pane {
            CloseTarget::Pane {
                workspace,
                tab,
                pane,
            }
        } else {
            CloseTarget::Tab { workspace, tab }
        };
        let sibling = if close_pane {
            CloseTarget::Pane {
                workspace,
                tab,
                pane: sibling,
            }
        } else {
            CloseTarget::Tab {
                workspace,
                tab: sibling,
            }
        };
        let inventory = workspace_inventory(&mut host);
        let approval = inventory.close_confirmation(target).unwrap();
        let sibling_confirmation = inventory.close_confirmation(sibling).unwrap();
        assert!(matches!(host.request(
            [2; 32], [2; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close {
                target: sibling, confirmation: sibling_confirmation,
            } },
        ), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
        assert!(matches!(host.request(
            [3; 32], [3; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close {
                target, confirmation: approval,
            } },
        ), Reply::Outcome(result) if result.outcome == Outcome::StaleConfirmation));
        let remaining = workspace_inventory(&mut host);
        assert_eq!(remaining.workspaces[0].id, workspace);
        let confirmation = remaining.close_confirmation(target).unwrap();
        assert!(matches!(host.request(
            [3; 32], [3; 32], Request::Mutate { sequence: 2, mutation: Mutation::Close {
                target, confirmation,
            } },
        ), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
        assert!(workspace_inventory(&mut host).workspaces.is_empty());
    }
}

#[test]
fn session_reset_retains_membership_but_ordinary_final_shell_exit_removes_it() {
    let mut host = WorkspaceHost::new(host());
    create_workspace(&mut host, 1);
    let pane = workspace_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let select = |explicit| Request::SelectPane {
        pane,
        explicit,
        size: size(),
        style: None,
    };
    let Reply::Attached { lease: old } = host.request([1; 32], [1; 32], select(true)) else {
        panic!()
    };
    host.shutdown().unwrap();
    assert_eq!(
        workspace_inventory(&mut host).workspaces[0].tabs[0].root_pane,
        pane
    );
    assert!(matches!(
        host.request([1; 32], [1; 32], select(false)),
        Reply::Restored { .. }
    ));
    let Reply::Attached { lease } = host.request([1; 32], [1; 32], select(true)) else {
        panic!()
    };
    assert_ne!(lease.session, old.session);
    let reply = command(&mut host, 1, lease, "exit 0\n");
    assert!(matches!(reply, Reply::Unchanged));
    let until = Instant::now() + Duration::from_secs(3);
    while !workspace_inventory(&mut host).workspaces.is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(workspace_inventory(&mut host).workspaces.is_empty());
    host.shutdown().unwrap();
}

#[test]
fn initial_organization_is_prepared_once_without_starting_a_shell() {
    let mut host = WorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let initial = workspace_inventory(&mut host);
    assert_eq!(initial.workspaces.len(), 1);
    assert!(
        initial
            .close
            .as_ref()
            .unwrap()
            .sessions
            .iter()
            .all(|p| p.session.is_none())
    );
    host.prepare_initial_organization().unwrap();
    assert_eq!(
        workspace_inventory(&mut host).workspaces,
        initial.workspaces
    );
    host.shutdown().unwrap();
}

#[test]
fn preparation_does_not_refill_deliberately_empty_organization() {
    use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
    let mut host = WorkspaceHost::new(host());
    let workspace = create_workspace(&mut host, 1);
    let confirmation = workspace_inventory(&mut host)
        .close_confirmation(CloseTarget::KillWorkspace { workspace })
        .unwrap();
    assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
        sequence: 2,
        mutation: Mutation::Close { target: CloseTarget::KillWorkspace { workspace }, confirmation },
    }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
    host.prepare_initial_organization().unwrap();
    assert!(workspace_inventory(&mut host).workspaces.is_empty());
    host.shutdown().unwrap();
}

#[test]
fn membership_preserves_independent_pane_grants_and_lazy_startup() {
    use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};
    let mut host = WorkspaceHost::new(host());
    let created = host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::CreateWorkspace {
                name: "Build".into(),
                directory: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    );
    assert!(
        matches!(created, Reply::Outcome(ref o) if matches!(o.outcome, Outcome::Accepted { .. }))
    );
    let Reply::Inventory { inventory, .. } = host.request([2; 32], [2; 32], Request::Inventory)
    else {
        panic!()
    };
    let w = &inventory.workspaces[0];
    let t = &w.tabs[0];
    let first = t.root_pane;
    let request = |pane, explicit| Request::SelectPane {
        pane,
        explicit,
        size: size(),
        style: None,
    };
    assert!(matches!(
        host.request([1; 32], [1; 32], request(first, false)),
        Reply::Restored { .. }
    ));
    let Reply::Attached { lease: first_lease } =
        host.request([1; 32], [1; 32], request(first, true))
    else {
        panic!()
    };
    let Reply::Outcome(second) = host.request(
        [2; 32],
        [2; 32],
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::SplitPane {
                workspace: w.id,
                tab: t.id,
                source: first,
                axis: Axis::Down,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    ) else {
        panic!()
    };
    let Outcome::Accepted { target: second } = second.outcome else {
        panic!()
    };
    let Reply::Attached {
        lease: second_lease,
    } = host.request([2; 32], [2; 32], request(second, true))
    else {
        panic!()
    };
    // A third client's passive subscription cannot acquire either Pane.
    assert!(matches!(
        host.request([3; 32], [3; 32], request(second, false)),
        Reply::Takeover { .. }
    ));
    // Opening another Tab acquires only its explicitly selected Focused Pane.
    let Reply::Outcome(tab) = host.request(
        [1; 32],
        [4; 32],
        Request::Mutate {
            sequence: 2,
            mutation: Mutation::CreateTab {
                workspace: w.id,
                source: None,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    ) else {
        panic!()
    };
    let Outcome::Accepted { target: tab } = tab.outcome else {
        panic!()
    };
    let Reply::Inventory { inventory, .. } = host.request([2; 32], [2; 32], Request::Inventory)
    else {
        panic!()
    };
    let third = inventory.workspaces[0]
        .tabs
        .iter()
        .find(|t| t.id == tab)
        .unwrap()
        .root_pane;
    assert!(matches!(
        host.request([2; 32], [2; 32], request(third, false)),
        Reply::Takeover { .. }
    ));
    assert!(matches!(
        host.request([1; 32], [4; 32], request(third, true)),
        Reply::Attached { .. }
    ));
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Resize {
                lease: first_lease,
                size: size()
            }
        ),
        Reply::Unchanged
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::Resize {
                lease: second_lease,
                size: size()
            }
        ),
        Reply::Unchanged
    ));
    host.shutdown().unwrap();
}

#[cfg(feature = "native-tests")]
#[test]
fn repeated_real_zsh_shutdown_confirms_owned_process_cleanup() {
    /// Starts zsh with an empty ZDOTDIR, so it reads only the system's startup files. The
    /// account's own files run programs of their own, and how long they take depends on the
    /// account and the host, not on the cleanup this test checks.
    struct SystemStartupFiles {
        pty: Arc<dyn NativePtyAdapterFactory>,
        zdotdir: String,
    }
    impl NativePtyAdapterFactory for SystemStartupFiles {
        fn create(
            &self,
            launch: crate::platform::shell_launch::PreparedShellLaunch,
            size: crate::platform::native_pty::NativePtySize,
        ) -> Result<
            crate::platform::native_pty::NativePtyAdapterParts,
            crate::platform::native_pty::NativePtyAdapterConstructionFailure,
        > {
            self.pty.create(
                launch.with_environment_value("ZDOTDIR", &self.zdotdir),
                size,
            )
        }
    }
    let zdotdir = std::env::temp_dir().join(format!(
        "spaceterm-zdotdir-{}-{:x}",
        std::process::id(),
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    std::fs::create_dir(&zdotdir).unwrap();
    let pty: Arc<dyn NativePtyAdapterFactory> = Arc::new(SystemStartupFiles {
        pty: native_pty_factory(Arc::default()),
        zdotdir: zdotdir.to_str().unwrap().into(),
    });
    for attempt in 0..64 {
        let mut host = host_with_shell("/bin/zsh", pty.clone());
        let lease = attach(&mut host, 1, true);
        command(
            &mut host,
            1,
            lease,
            "stty -echo; printf '\\033[2J\\033[HCLEANUP-READY\\r\\n'",
        );
        await_text(&mut host, lease, "CLEANUP-READY");
        assert_eq!(
            host.shutdown(),
            Ok(()),
            "native zsh cleanup attempt {attempt}"
        );
    }
    std::fs::remove_dir_all(zdotdir).unwrap();
}

pub(super) fn host() -> ServerPaneHost {
    host_with_shell("/bin/sh", native_pty_factory(Arc::default()))
}

fn host_with_shell(shell: &str, pty: Arc<dyn NativePtyAdapterFactory>) -> ServerPaneHost {
    let filesystem = local_filesystem();
    let directory = filesystem
        .validate_directory(&std::env::temp_dir())
        .unwrap();
    let factory = NativeTerminalSessionFactory::new(
        pty,
        ShellLaunchPlanner::new(
            shell.into(),
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
        filesystem,
        LocalMachine::default(),
        Arc::default(),
    );
    ServerPaneHost::new(factory, directory).unwrap()
}

pub(super) fn size() -> PaneSize {
    PaneSize {
        columns: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 18,
    }
}

/// A Pane identity for a host the tests run on its own, whose Sessions start in `directory`.
pub(super) fn identity(directory: std::path::PathBuf) -> PaneIdentity {
    PaneIdentity {
        workspace: [11; 32],
        tab: [12; 32],
        pane: [13; 32],
        starting_directory: directory,
        confirmed_directory: None,
    }
}

/// Selects `host`'s Pane for the client `[1; 32]` over `connection`. With `create`, the selection
/// is explicit, so it starts the Pane's first Session; without it, it claims the running one.
pub(super) fn attach(host: &mut ServerPaneHost, connection: u8, create: bool) -> Lease {
    attach_sized(host, connection, create, size())
}

/// [`attach`] at `size`.
pub(super) fn attach_sized(
    host: &mut ServerPaneHost,
    connection: u8,
    create: bool,
    size: PaneSize,
) -> Lease {
    host.identity
        .get_or_insert_with(|| identity(host.directory.clone()));
    let Ok(Reply::Attached { lease }) = host.select([1; 32], [connection; 32], create, size, None)
    else {
        panic!("expected attachment")
    };
    lease
}

/// The focus generation [`command`] types at. A test that sets its own focus starts at this
/// generation with the terminal focused, or above it.
pub(super) const COMMAND_FOCUS: u64 = 1;

/// Types `text` and Enter at the shell of `lease`'s Pane, as the client `[1; 32]` with the
/// terminal focused at [`COMMAND_FOCUS`].
pub(super) fn command(host: &mut impl Driven, connection: u8, lease: Lease, text: &str) -> Reply {
    let connection = [connection; 32];
    let focused = host.send(
        [1; 32],
        connection,
        Request::Focus {
            lease,
            generation: COMMAND_FOCUS,
            target: FocusTarget::Terminal,
        },
    );
    if !matches!(focused, Reply::Unchanged) {
        return focused;
    }
    let key = |physical: PhysicalKey, logical: &str, text: Option<String>| Key {
        physical: physical as u32,
        action: 0,
        logical: logical.into(),
        text,
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    };
    host.send(
        [1; 32],
        connection,
        Request::Keys {
            lease,
            generation: COMMAND_FOCUS,
            keys: vec![
                key(
                    PhysicalKey::Unidentified,
                    "text-input",
                    Some(text.trim_end_matches('\n').into()),
                ),
                key(PhysicalKey::Enter, "enter", None),
            ],
        },
    )
}

/// What the channel bound to `lease`, claimed by `device`, receives now: the Pane's current screen
/// as a keyframe, or the channel's last event once the lease no longer presents the Pane.
pub(super) fn screen(
    host: &mut impl Driven,
    device: [u8; 32],
    lease: Lease,
) -> Result<Option<ScreenUpdate>, PaneEvent> {
    match host.watch(device, &lease) {
        Observed::Live(observation) => Ok(observation.screen.map(|capture| {
            screen_encoder::ScreenEncoder::default()
                .encode(&capture, 1, None)
                .expect("a keyframe always encodes")
        })),
        Observed::Gone { event, .. } => Err(event),
    }
}

/// Starts `read` as request `id` of `connection` and waits for its reply.
pub(super) fn read(
    host: &mut impl HostCommands,
    connection: [u8; 32],
    id: u32,
    read: HostRead,
) -> Result<spaceterm_server::protocol::ReplyBody, Refusal> {
    let id = spaceterm_server::protocol::RequestId(id);
    if let Some(reply) = host.read([1; 32], connection, id, read) {
        return reply;
    }
    Ok(await_read(host, connection, id))
}

/// Waits for read `id` of `connection`, which runs on the host, to settle.
pub(super) fn await_read(
    host: &mut impl HostCommands,
    connection: [u8; 32],
    id: spaceterm_server::protocol::RequestId,
) -> spaceterm_server::protocol::ReplyBody {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(read) = host
            .settled_reads()
            .into_iter()
            .find(|read| read.connection == connection && read.id == id)
        {
            return read.reply;
        }
        assert!(Instant::now() < deadline, "the read did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until a row of the Pane's screen reads exactly `text`, for the client `[1; 32]`.
pub(super) fn await_text(host: &mut impl Driven, lease: Lease, text: &str) {
    await_screen(host, [1; 32], lease, |update| {
        lines(update).iter().any(|line| line.trim_end() == text)
    });
}

/// Waits until the Pane's screen satisfies `ready` and returns it.
pub(super) fn await_screen(
    host: &mut impl Driven,
    device: [u8; 32],
    lease: Lease,
    ready: impl Fn(&ScreenUpdate) -> bool,
) -> ScreenUpdate {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut diagnostic = Vec::new();
    loop {
        match screen(host, device, lease) {
            Ok(Some(update)) => {
                diagnostic = lines(&update);
                if ready(&update) {
                    return update;
                }
            }
            Ok(None) => {}
            Err(event) => panic!("the channel ended with {event:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "expected terminal output; fixture screen: {:?}",
            diagnostic.join("\n")
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Observes the Pane until it has released `count` client effects to the client `[1; 32]`,
/// oldest first.
pub(super) fn effects(
    host: &mut impl Driven,
    lease: Lease,
    count: usize,
) -> Vec<pane::effects::Request> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut effects = Vec::new();
    loop {
        match host.watch([1; 32], &lease) {
            Observed::Live(observation) => effects.extend(observation.effects),
            Observed::Gone { event, .. } => panic!("the channel ended with {event:?}"),
        }
        if effects.len() >= count {
            break;
        }
        assert!(Instant::now() < deadline, "native request missing");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(effects.len(), count, "unexpected native requests");
    effects
}

/// The text of every Scrollback row the Pane retains, read through `ScrollbackPage` as a client
/// reads it, oldest first.
pub(super) fn history_text(
    host: &mut impl Driven,
    device: [u8; 32],
    lease: Lease,
    history: spaceterm_server::protocol::screen::History,
) -> Vec<String> {
    let mut text = Vec::new();
    let mut start = history.oldest;
    while start < history.total {
        let (sent, received) = std::sync::mpsc::channel();
        host.page(
            device,
            lease.connection,
            &lease,
            ScrollbackRead {
                epoch: history.epoch,
                start,
                rows: spaceterm_server::protocol::limits::MAX_PAGE_ROWS,
            },
            Box::new(move |page| {
                let _ = sent.send(page);
            }),
        )
        .expect("the controller reads its Scrollback");
        let page = received
            .recv_timeout(Duration::from_secs(10))
            .expect("the terminal worker answers")
            .expect("the epoch is current");
        page.validate().expect("pages are within bounds");
        if page.rows.is_empty() {
            break;
        }
        start = page.start + page.rows.len() as u64;
        text.extend(
            page.rows
                .iter()
                .map(|row| row.runs.iter().map(|run| run.text.as_str()).collect()),
        );
    }
    text
}

/// The text of each row of a keyframe. Rows the keyframe omits are blank, and a wide character's
/// spacer adds nothing.
pub(super) fn lines(update: &ScreenUpdate) -> Vec<String> {
    let rows = update
        .geometry
        .map_or(0, |geometry| usize::from(geometry.rows));
    let mut lines = vec![String::new(); rows];
    for row in &update.rows {
        if let Some(line) = lines.get_mut(usize::from(row.index)) {
            *line = row.runs.iter().map(|run| run.text.as_str()).collect();
        }
    }
    lines
}

/// The bytes `update` takes on the wire as a pushed keyframe, or `None` when it exceeds the
/// protocol's bounds.
pub(super) fn frame_bytes(update: ScreenUpdate) -> Option<usize> {
    let message = spaceterm_server::protocol::ServerMessage::Frame(Box::new(
        spaceterm_server::protocol::Frame {
            channel: spaceterm_server::protocol::Channel(1),
            frame: 1,
            base: None,
            input: 0,
            update,
        },
    ));
    spaceterm_server::protocol::frame::encode_server(&message)
        .ok()
        .map(|wire| wire.bytes())
}

/// `screen` encoded as a keyframe, as a new channel receives it.
pub(super) fn keyframe(
    screen: &ScreenSnapshot,
) -> spaceterm_server::protocol::screen::ScreenUpdate {
    let capture = screen_encoder::ScreenCapture {
        screen: Arc::new(screen.clone()),
        generation: 1,
        accessibility_revision: 0,
    };
    screen_encoder::ScreenEncoder::default()
        .encode(&capture, 1, None)
        .unwrap()
}
