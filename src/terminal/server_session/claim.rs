//! Acquire Pane control and start only explicitly requested retained shells.
use super::*;

#[cfg(test)]
mod process_tests;

pub(super) struct ClaimWorker<'a> {
    pub state: &'a DevelopmentPaneState,
    pub pane_id: spaceterm_server::hierarchy::Id,
    pub alive: &'a AtomicBool,
    pub attachment: &'a Option<spaceterm_server::attachments::AttachmentPermit>,
    pub size: &'a Mutex<PaneSize>,
    pub appearance: &'a Mutex<TerminalAppearanceUpdate>,
    pub directory_recovery: bool,
    pub attached_session: &'a mut bool,
    pub takeover: &'a mut bool,
    pub paste_unconfirmed: &'a mut bool,
}

pub(super) struct Granted {
    pub lease: Lease,
    pub size: PaneSize,
    pub appearance: Option<crate::appearance::AppearanceGeneration>,
}

/// Runs in the presenter's send turn, so replacement cannot overtake recovery bookkeeping.
pub(super) fn exchange(
    state: &DevelopmentPaneState,
    pane_id: spaceterm_server::hierarchy::Id,
    request: &Request,
    exchange: impl FnOnce(&Request) -> Result<Reply, ServerError>,
) -> Result<Reply, ServerError> {
    let start = match request {
        Request::StartRestored { server, .. } => Some(Some(*server)),
        Request::RepairDirectory { target, .. } => Some(Some(target.boot)),
        Request::SelectPane { explicit: true, .. } => Some(None),
        Request::TakeOver { target, .. }
            if state.session_dormant() || target.session == pane_id =>
        {
            Some(Some(target.server))
        }
        _ => None,
    };
    if let Some(boot) = start {
        state
            .connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin_start(boot)?;
        state.directory_busy.store(true, Ordering::Release);
        state.admission(Admission::Pending);
    }
    let result = exchange(request).and_then(|reply| {
        if matches!(&reply, Reply::Restored { pane, .. } if *pane != pane_id) {
            Err(ServerError::Protocol)
        } else {
            Ok(reply)
        }
    });
    if start.is_some() {
        state
            .connection_recovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .started(&result);
        state.directory_busy.store(false, Ordering::Release);
        if result.is_err() && !state.start_unconfirmed() {
            state.admission(Admission::Waiting);
        }
    }
    let reply = result?;
    state
        .connection_recovery
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .observed(&reply)?;
    Ok(reply)
}

impl ClaimWorker<'_> {
    pub fn run(
        self,
        mut exchange: impl FnMut(&Request) -> Result<Reply, ServerError>,
        mut discard_presentation: impl FnMut(),
    ) -> Result<Option<Granted>, ServerError> {
        let Self {
            state,
            pane_id,
            alive,
            attachment,
            size,
            appearance,
            directory_recovery,
            attached_session,
            takeover,
            paste_unconfirmed,
        } = self;
        let mut grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
        let grant_update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut grant_appearance = Some(grant_update.generation);
        let style = Some(encode_style(&grant_update));
        let request = Request::SelectPane {
            pane: pane_id,
            // Observe the boot before first-use intent can start a restored shell. The queued
            // selection is consumed only after that observation, including through Takeover.
            explicit: false,
            size: grant_size,
            style,
        };
        let mut unavailable = false;
        let mut reply = match exchange(&request) {
            Err(ServerError::DirectoryUnavailable) => {
                unavailable = true;
                let passive = Request::SelectPane {
                    pane: pane_id,
                    explicit: false,
                    size: grant_size,
                    style: None,
                };
                exchange(&passive)?
            }
            other => other.map_err(|error| match error {
                ServerError::Controlled => ServerError::Unsupported,
                ServerError::StaleControl => ServerError::SessionUnavailable,
                other => other,
            })?,
        };
        // Creation grants no control acquisition. Only the first authenticated observation of a
        // fresh, creator-controlled Pane can consume this intent. Reconnect never rearms it.
        let created_start = state.created_start.swap(false, Ordering::AcqRel)
            && matches!(
                &reply,
                Reply::Restored {
                    fresh: true,
                    session_ended: false,
                    ..
                }
            );
        let mut created_start = created_start && !state.start_unconfirmed();
        while matches!(reply, Reply::Restored { .. } | Reply::Takeover { .. }) {
            let mut dormant = false;
            while let Reply::Restored {
                server: boot,
                pane,
                ref recovery,
                directory_unavailable,
                session_ended,
                fresh,
                ..
            } = reply
            {
                if !dormant {
                    dormant = true;
                    *attached_session = false;
                    *takeover = false;
                    *paste_unconfirmed = false;
                    state.invalidate();
                    if state.directory_busy.swap(false, Ordering::AcqRel) {
                        *state
                            .directory_note
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some("Starting the shell was not confirmed.");
                    }
                    state.start_restored.store(false, Ordering::Release);
                    discard_presentation();
                }
                *state.recovery.lock().unwrap_or_else(|e| e.into_inner()) =
                    recovery.clone().filter(|_| directory_recovery);
                unavailable |= directory_unavailable;
                state
                    .session_dormant
                    .store(session_ended, Ordering::Release);
                state.admission(Admission::Waiting);
                state.set(if unavailable {
                    DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                } else if session_ended {
                    DevelopmentPaneStatus::SessionEnded
                } else if fresh {
                    DevelopmentPaneStatus::Connecting
                } else {
                    DevelopmentPaneStatus::Restored
                });
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(None);
                }
                let repair = state
                    .repair
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                // These requests authorize the same first use. Consume them together even
                // when creation or repair already supplies that authorization.
                let created = std::mem::take(&mut created_start);
                let start_restored = state.start_restored.swap(false, Ordering::AcqRel);
                let selected = state.selected.swap(false, Ordering::AcqRel);
                if created || repair.is_some() || start_restored || selected {
                    grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    grant_appearance = Some(update.generation);
                    let action = match repair {
                        Some((target, replacement)) => Request::RepairDirectory {
                            target,
                            replacement: Some(replacement),
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                        None if unavailable && recovery.is_some() => Request::RepairDirectory {
                            target: recovery.clone().ok_or(ServerError::Protocol)?,
                            replacement: None,
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                        None => Request::StartRestored {
                            server: boot,
                            pane,
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                    };
                    let replacing = matches!(
                        &action,
                        Request::RepairDirectory {
                            replacement: Some(_),
                            ..
                        }
                    );
                    reply = match exchange(&action) {
                        Err(ServerError::DirectoryUnavailable) => {
                            unavailable = true;
                            state.directory_busy.store(false, Ordering::Release);
                            *state
                                .directory_note
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = Some(if replacing {
                                "That directory is unavailable. Choose another."
                            } else {
                                "The directory is unavailable."
                            });
                            state.invalidate();
                            reply
                        }
                        result => result?,
                    };
                } else {
                    // Take Over, Start and selection wake this check early.
                    state
                        .wake
                        .wait_until(std::time::Instant::now() + Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    grant_size = size;
                    grant_appearance = None;
                    reply = exchange(&Request::SelectPane {
                        pane: pane_id,
                        explicit: false,
                        size,
                        style: None,
                    })?;
                }
            }
            state.directory_busy.store(false, Ordering::Release);
            state
                .recovery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            while let Reply::Takeover {
                target,
                ref controller,
                session_ended,
            } = reply
            {
                state.paste_unconfirmed.store(false, Ordering::Release);
                if !*takeover {
                    *takeover = true;
                    *paste_unconfirmed = false;
                    state.invalidate();
                    discard_presentation();
                }
                state.offered_take_over(controller.clone(), session_ended);
                state.admission(Admission::Waiting);
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(None);
                }
                if state.take_over.swap(false, Ordering::AcqRel) {
                    state.admission(Admission::Pending);
                    state.set(DevelopmentPaneStatus::TakingOver);
                    grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    grant_appearance = Some(update.generation);
                    reply = exchange(&Request::TakeOver {
                        target,
                        size: grant_size,
                        style: Some(encode_style(&update)),
                    })?;
                } else {
                    // Take Over, Start and selection wake this check early.
                    state
                        .wake
                        .wait_until(std::time::Instant::now() + Duration::from_millis(100));
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    grant_size = size;
                    grant_appearance = None;
                    reply = exchange(&Request::SelectPane {
                        pane: pane_id,
                        explicit: state.selected.swap(false, Ordering::AcqRel),
                        size,
                        style: None,
                    })?;
                }
            }
        }
        let Reply::Attached { lease } = reply else {
            return Err(ServerError::Protocol);
        };
        // A selection made while the admitting request was in flight selected this running
        // shell. It cannot authorize another shell after a later server restart.
        {
            // Serialize with selection of a Connecting placeholder, including a selection
            // made after admission but before the first hidden screen arrives.
            let _status = state.status.lock().unwrap_or_else(|e| e.into_inner());
            state.admission(Admission::Granted);
            state.selected.store(false, Ordering::Release);
            state.start_restored.store(false, Ordering::Release);
        }
        Ok(Some(Granted {
            lease,
            size: grant_size,
            appearance: grant_appearance,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_claim(
        state: &DevelopmentPaneState,
        mut server: impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Option<Granted>, ServerError> {
        ClaimWorker {
            state,
            pane_id: [2; 32],
            alive: &AtomicBool::new(true),
            attachment: &None,
            size: &Mutex::new(PaneSize {
                columns: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            }),
            appearance: &Mutex::new(TerminalAppearanceUpdate::builtin().unwrap()),
            directory_recovery: true,
            attached_session: &mut false,
            takeover: &mut false,
            paste_unconfirmed: &mut false,
        }
        .run(
            |request| exchange(state, [2; 32], request, &mut server),
            || {},
        )
    }

    fn restored(boot: u8) -> Reply {
        Reply::Restored {
            fresh: false,
            server: [boot; 32],
            pane: [2; 32],
            workspace: [3; 32],
            tab: [4; 32],
            recovery: None,
            directory_unavailable: false,
            session_ended: false,
        }
    }

    #[test]
    fn fifteen_hidden_creation_grants_release_startup_without_a_screen() {
        use crate::runtime::HostCommands as _;
        use spaceterm_server::hierarchy::Mutation;

        let mut host = crate::runtime::directory_testing::terminal_host();
        host.prepare_initial_organization().unwrap();
        let Reply::Inventory { inventory, .. } = host.request([1; 32], [1; 32], Request::Inventory)
        else {
            panic!("fixture inventory unavailable");
        };
        let workspace = inventory.workspaces[0].id;
        let mut observations = Vec::new();
        for sequence in 1..=15 {
            assert!(matches!(
                host.request(
                    [1; 32],
                    [1; 32],
                    Request::Mutate {
                        sequence,
                        mutation: Mutation::CreateTab {
                            workspace,
                            source: None
                        },
                    }
                ),
                Reply::Outcome(_)
            ));
            let Reply::Inventory { inventory, .. } =
                host.request([1; 32], [1; 32], Request::Inventory)
            else {
                panic!("new Tab inventory unavailable");
            };
            let pane = inventory.workspaces[0].tabs.last().unwrap().root_pane;
            let state = DevelopmentPaneState::new();
            state.start_created();
            let grant = ClaimWorker {
                state: &state,
                pane_id: pane,
                alive: &AtomicBool::new(true),
                attachment: &None,
                size: &Mutex::new(PaneSize {
                    columns: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                }),
                appearance: &Mutex::new(TerminalAppearanceUpdate::builtin().unwrap()),
                directory_recovery: true,
                attached_session: &mut false,
                takeover: &mut false,
                paste_unconfirmed: &mut false,
            }
            .run(
                |request| {
                    exchange(&state, pane, request, |request| {
                        Ok(host.request([1; 32], [1; 32], request.clone()))
                    })
                },
                || {},
            )
            .unwrap()
            .unwrap();
            assert!(matches!(
                host.request(
                    [1; 32],
                    [1; 32],
                    Request::PresentationDemand {
                        lease: grant.lease,
                        visible: false
                    }
                ),
                Reply::Unchanged
            ));
            let poll = host.request(
                [1; 32],
                [1; 32],
                Request::PollChanges {
                    lease: grant.lease,
                    base: None,
                },
            );
            observations.push((
                state.startup_pending(),
                state.status(),
                state.interactive_input(),
                matches!(poll, Reply::Screen(_) | Reply::ScreenChanges(_)),
            ));
        }
        // Stop every fixture PTY before any assertion can fail.
        host.shutdown().unwrap();
        assert_eq!(observations.len(), 15);
        for (pending, status, input, screen) in observations {
            assert!(!screen, "hidden lifecycle polls supply no screen");
            assert_eq!(status, DevelopmentPaneStatus::Connecting);
            assert_eq!(input, crate::terminal::InteractiveInput::Pending);
            assert!(
                !pending,
                "an authenticated creation grant releases the startup slot before a screen"
            );
        }
    }

    #[test]
    fn never_visited_hidden_pane_prepares_current_rows_without_visible_cadence() {
        use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};
        use crate::runtime::HostCommands as _;

        let mut host = crate::runtime::directory_testing::terminal_host();
        host.prepare_initial_organization().unwrap();
        let Reply::Inventory { inventory, .. } = host.request([1; 32], [1; 32], Request::Inventory)
        else {
            panic!("fixture inventory unavailable")
        };
        let pane = inventory.workspaces[0].tabs[0].root_pane;
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(80, 24),
            LogicalCellSize::new(8.0, 16.0),
            BackingScale::ONE,
        );
        let state = Arc::new(DevelopmentPaneState::new());
        state.start_created();
        let (input, _receiver) = mpsc::sync_channel(1);
        let handle = PresentationHandle {
            input,
            state: Arc::clone(&state),
            alive: Arc::new(AtomicBool::new(true)),
            size: Arc::new(Mutex::new(pane_size(geometry))),
            appearance: Arc::new(Mutex::new(TerminalAppearanceUpdate::builtin().unwrap())),
            geometry: Mutex::new(geometry),
        };
        handle.set_presentable(false);
        let grant = ClaimWorker {
            state: &state,
            pane_id: pane,
            alive: &handle.alive,
            attachment: &None,
            size: &handle.size,
            appearance: &handle.appearance,
            directory_recovery: true,
            attached_session: &mut false,
            takeover: &mut false,
            paste_unconfirmed: &mut false,
        }
        .run(
            |request| {
                exchange(&state, pane, request, |request| {
                    Ok(host.request([1; 32], [1; 32], request.clone()))
                })
            },
            || {},
        )
        .unwrap()
        .unwrap();
        let lease = grant.lease;
        // Fixture output is produced through the server's real PTY. The client remains hidden
        // and never receives an input-enabling screen or a forced Connected status.
        for key in [
            KeyInput::text_input("printf '\\033[2J\\033[HHIDDEN-PREPARED\\r\\n'"),
            KeyInput {
                physical_key: PhysicalKey::Enter,
                logical_key: "enter".into(),
                text: None,
                ..KeyInput::text_input("")
            },
        ] {
            assert!(matches!(
                host.request(
                    [1; 32],
                    [1; 32],
                    Request::Key {
                        lease,
                        key: encode_key(key),
                    }
                ),
                Reply::Unchanged
            ));
        }
        handle.set_preparation_demand(true);
        let start = std::time::Instant::now();
        let mut cadence = schedule::Cadence::new(start, false);
        // Read the same demand inputs that the production presentation worker uses.
        cadence.set_preparation(state.preparation.load(Ordering::Acquire));
        let mut prepared = false;
        let mut decoder = pane::presentation::ScreenDecoder::default();
        while start.elapsed() < Duration::from_secs(3) {
            if let Some(visible) = cadence.take_demand() {
                assert!(matches!(
                    host.request(
                        [1; 32],
                        [1; 32],
                        Request::PresentationDemand { lease, visible }
                    ),
                    Reply::Unchanged
                ));
            }
            let now = std::time::Instant::now();
            if cadence.poll_due(now) {
                let reply = host.request(
                    [1; 32],
                    [1; 32],
                    Request::PollChanges {
                        lease,
                        base: decoder.baseline(),
                    },
                );
                if let Reply::Screen(screen) = decoder.apply(reply).unwrap() {
                    prepared = screen.rows.iter().any(|row| {
                        row.iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            .trim_end()
                            == "HIDDEN-PREPARED"
                    });
                    if prepared {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.set_preparation_demand(false);
        cadence.set_preparation(state.preparation.load(Ordering::Acquire));
        let withdrawn = cadence.take_demand();
        host.shutdown().unwrap();
        assert!(
            prepared,
            "bounded preparation demand must supply real hidden output rows"
        );
        assert!(!state.presentable.load(Ordering::Acquire));
        assert!(
            !cadence.visible(),
            "preparation must not enable graphics fetching or visible cadence"
        );
        assert_eq!(withdrawn, Some(false));
        assert!(cadence.deadline() >= start + Duration::from_millis(250));
        assert_eq!(
            state.interactive_input(),
            crate::terminal::InteractiveInput::Pending
        );
    }

    #[test]
    fn fresh_panes_never_present_server_restarted_before_first_use() {
        use crate::ui::server_pane::ServerPaneView;
        use crate::runtime::HostCommands as _;
        use spaceterm_server::hierarchy::Mutation;

        for delayed in [false, true] {
            for revoked in [false, true] {
                let mut host = crate::runtime::directory_testing::terminal_host();
                host.prepare_initial_organization().unwrap();
                let Reply::Inventory { inventory, .. } =
                    host.request([1; 32], [1; 32], Request::Inventory)
                else {
                    panic!("fixture inventory unavailable");
                };
                let workspace = inventory.workspaces[0].id;
                assert!(matches!(
                    host.request(
                        [1; 32],
                        [1; 32],
                        Request::Mutate {
                            sequence: 1,
                            mutation: Mutation::CreateTab {
                                workspace,
                                source: None
                            },
                        }
                    ),
                    Reply::Outcome(_)
                ));
                let Reply::Inventory { inventory, .. } =
                    host.request([1; 32], [1; 32], Request::Inventory)
                else {
                    panic!("new Tab inventory unavailable");
                };
                let pane = inventory.workspaces[0].tabs.last().unwrap().root_pane;
                if revoked {
                    host.revoked([1; 32]);
                }
                let state = Arc::new(DevelopmentPaneState::new());
                let mut view = ServerPaneView::new(state.clone());
                if !delayed {
                    state.select();
                }
                let mut observations = 0;
                let granted = ClaimWorker {
                    state: &state,
                    pane_id: pane,
                    alive: &AtomicBool::new(true),
                    attachment: &None,
                    size: &Mutex::new(PaneSize {
                        columns: 80, rows: 24, cell_width: 8, cell_height: 16,
                    }),
                    appearance: &Mutex::new(TerminalAppearanceUpdate::builtin().unwrap()),
                    directory_recovery: true,
                    attached_session: &mut false,
                    takeover: &mut false,
                    paste_unconfirmed: &mut false,
                }.run(|request| {
                    view.sync();
                    assert_eq!(view.screen(), None,
                        "a fresh Pane must not present restart recovery, delayed={delayed}, revoked={revoked}");
                    if matches!(request, Request::SelectPane { .. }) {
                        observations += 1;
                        if delayed && observations == 2 {
                            state.select();
                        }
                        assert!(observations <= 3, "first selection must start the shell");
                    }
                    exchange(&state, pane, request, |request| {
                        Ok(host.request([1; 32], [1; 32], request.clone()))
                    })
                }, || {}).unwrap().unwrap();
                assert_eq!(granted.lease.connection, [1; 32]);
                host.shutdown().unwrap();
            }
        }
    }

    #[test]
    fn creation_starts_only_a_fresh_controlled_pane_and_never_rearms_on_reconnect() {
        let state = DevelopmentPaneState::new();
        state.start_created();
        let mut starts = 0;
        let result = run_claim(&state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                let mut reply = restored(1);
                if let Reply::Restored { fresh, .. } = &mut reply {
                    *fresh = true;
                }
                Ok(reply)
            }
            Request::StartRestored { .. } => {
                starts += 1;
                Err(ServerError::Protocol)
            }
            other => panic!("creation must not acquire control: {other:?}"),
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert_eq!(starts, 1);
        assert!(state.start_unconfirmed());
        assert!(
            state.startup_pending(),
            "an unconfirmed original still occupies its slot"
        );
        state.connection_failed(ServerError::Protocol, false);
        let mut observations = 0;
        let _ = run_claim(&state, |request| {
            observations += 1;
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            if observations == 1 {
                Ok(restored(1))
            } else {
                Err(ServerError::Stopped)
            }
        });
        assert_eq!(starts, 1);
        assert!(!state.created_start.load(Ordering::Acquire));
    }

    #[test]
    fn creation_and_selection_are_consumed_together_before_a_later_server_restart() {
        let state = DevelopmentPaneState::new();
        state.start_created();
        state.select();
        let lease = Lease {
            server: [1; 32],
            session: [5; 32],
            control: [6; 32],
            connection: [7; 32],
        };
        let granted = run_claim(&state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                let mut reply = restored(1);
                if let Reply::Restored { fresh, .. } = &mut reply {
                    *fresh = true;
                }
                Ok(reply)
            }
            Request::StartRestored { .. } => {
                state.select();
                Ok(Reply::Attached { lease })
            }
            other => panic!("unexpected request {other:?}"),
        })
        .unwrap();
        assert!(granted.is_some());
        // A hidden grant has not received a screen yet. Reselecting its Connecting placeholder
        // selects the admitted shell and cannot authorize first use after a later restart.
        assert_eq!(state.status(), DevelopmentPaneStatus::Connecting);
        state.select();
        let mut observations = 0;
        let _ = run_claim(&state, |request| {
            assert!(
                matches!(
                    request,
                    Request::SelectPane {
                        explicit: false,
                        ..
                    }
                ),
                "the selection coalesced with creation must not start work after restart: {request:?}"
            );
            observations += 1;
            if observations == 1 {
                Ok(restored(2))
            } else {
                Err(ServerError::Stopped)
            }
        });
        assert!(!state.selected.load(Ordering::Acquire));
    }

    #[test]
    fn creation_intent_does_not_start_retained_or_taken_over_work() {
        for taken_over in [false, true] {
            let state = DevelopmentPaneState::new();
            state.start_created();
            let mut observations = 0;
            let _ = run_claim(&state, |request| {
                assert!(
                    matches!(
                        request,
                        Request::SelectPane {
                            explicit: false,
                            ..
                        }
                    ),
                    "creation must neither start retained work nor acquire control"
                );
                observations += 1;
                if observations > 1 {
                    assert!(
                        !state.startup_pending(),
                        "waiting for first use or Take Over must release the startup slot"
                    );
                    return Err(ServerError::Stopped);
                }
                Ok(if taken_over {
                    Reply::Takeover {
                        target: pane::PaneTarget {
                            server: [1; 32],
                            session: [2; 32],
                        },
                        controller: pane::control::Controller::Nobody,
                        session_ended: false,
                    }
                } else {
                    restored(1)
                })
            });
            assert!(!state.created_start.load(Ordering::Acquire));
        }
    }

    fn lose_start(state: &DevelopmentPaneState) {
        let mut starts = 0;
        let result = run_claim(state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                // Select once to enter Restored, then request a start on its passive poll.
                state.start_restored();
                Ok(restored(1))
            }
            Request::StartRestored { .. } => {
                starts += 1;
                Err(ServerError::Protocol)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert_eq!(starts, 1);
        state.connection_failed(ServerError::Protocol, false);
    }

    #[test]
    fn a_lost_takeover_of_another_clients_restored_pane_blocks_another_start() {
        let state = DevelopmentPaneState::new();
        let offered = || Reply::Takeover {
            target: pane::PaneTarget {
                server: [1; 32],
                // A retained Pane without a live shell uses its stable Pane identity here.
                session: [2; 32],
            },
            controller: pane::control::Controller::Client {
                name: "Other client".into(),
                this_client: false,
            },
            // Restored organization has no prior ended Session, unlike End All Sessions.
            session_ended: false,
        };
        let mut starts = 0;
        let result = run_claim(&state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                state.take_over();
                Ok(offered())
            }
            Request::TakeOver { target, .. } => {
                assert_eq!(target.session, [2; 32]);
                starts += 1;
                // The old request may still be queued on its original transport.
                Err(ServerError::Protocol)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert_eq!(starts, 1);
        assert!(
            state.start_unconfirmed(),
            "Take Over can start this restored shell"
        );
        state.connection_failed(ServerError::Protocol, true);
        let mut observations = 0;
        let result = run_claim(&state, |request| {
            observations += 1;
            assert_eq!(observations, 1);
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            state.take_over();
            state.select();
            Ok(offered())
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(!state.take_over.load(Ordering::Acquire));
        assert!(!state.selected.load(Ordering::Acquire));
        assert!(state.start_unconfirmed());
    }

    #[test]
    fn a_dormant_observation_cannot_replace_a_delayed_start_on_the_old_connection() {
        let state = DevelopmentPaneState::new();
        lose_start(&state);
        // The old request is still queued at the host. The fresh connection overtakes it.
        let mut observations = 0;
        let result = run_claim(&state, |request| {
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            observations += 1;
            assert_eq!(
                observations, 1,
                "a dormant observation must retain uncertainty"
            );
            state.start_restored();
            state.select();
            Ok(restored(1))
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(state.start_unconfirmed());
        // Even an explicit action reaching the backend cannot produce another shell start.
        state.set(DevelopmentPaneStatus::SessionEnded);
        state.start_restored();
        state.select();
        assert!(!state.start_restored.load(Ordering::Acquire));
        assert!(!state.selected.load(Ordering::Acquire));
        state.offered_take_over(pane::control::Controller::Nobody, true);
        state.take_over();
        assert!(!state.take_over.load(Ordering::Acquire));
        let target = pane::directories::Target {
            boot: [1; 32],
            pane: [2; 32],
            original: "/original".into(),
        };
        *state.recovery.lock().unwrap() = Some(target.clone());
        state.set(DevelopmentPaneStatus::Failed(
            ServerError::DirectoryUnavailable,
        ));
        state.repair_directory(target, "/replacement".into());
        assert!(state.repair.lock().unwrap().is_none());
        // The delayed original finally runs. Observing its live grant settles it without replay.
        let lease = Lease {
            server: [1; 32],
            session: [5; 32],
            control: [6; 32],
            connection: [7; 32],
        };
        let grant = run_claim(&state, |request| {
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            Ok(Reply::Attached { lease })
        })
        .unwrap()
        .unwrap();
        assert_eq!(grant.lease, lease);
        assert!(!state.start_unconfirmed());
    }

    #[test]
    fn a_new_server_boot_settles_the_old_start_before_a_fresh_start() {
        let state = DevelopmentPaneState::new();
        state.select();
        lose_start(&state);
        let mut starts = 0;
        let result = run_claim(&state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                state.start_restored();
                Ok(restored(8))
            }
            Request::StartRestored { server, .. } => {
                starts += 1;
                assert_eq!(*server, [8; 32]);
                Err(ServerError::Unsupported)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(ServerError::Unsupported)));
        assert_eq!(starts, 1);
        assert!(
            !state.start_unconfirmed(),
            "a typed refusal settles the fresh request"
        );
        assert!(
            !state.startup_pending(),
            "definite refusal releases startup admission"
        );
    }

    #[test]
    fn a_reopened_presentation_retains_its_unconfirmed_start_without_blocking_other_panes() {
        let controls = PaneControls::default();
        let server = ServerConnection::for_identity("spaceterm-development").unwrap();
        let open = |controls: &PaneControls, pane| {
            DevelopmentServerSessionFactory::for_origin(server.clone(), controls, None, None, pane)
        };
        let (old, state) = open(&controls, [2; 32]);
        lose_start(&state);
        drop(old);
        drop(state);
        let (_, reopened) = open(&controls, [2; 32]);
        assert!(
            reopened.start_unconfirmed(),
            "closing a window cannot settle its start"
        );
        reopened.select();
        let result = run_claim(&reopened, |request| {
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            Ok(restored(1))
        });
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(reopened.start_unconfirmed());
        assert!(!open(&controls, [9; 32]).1.start_unconfirmed());
        assert!(
            !open(&PaneControls::default(), [2; 32])
                .1
                .start_unconfirmed()
        );
    }

    fn disconnected_direct_handle(route: u8, client: u8, server: u8) -> ServerConnection {
        use spaceterm_server::direct::{
            DirectConnection, DirectConnector, DirectError, PairedDevice, ServerPin,
        };
        struct Offline;
        impl DirectConnector for Offline {
            fn connect(&self) -> Result<DirectConnection, DirectError> {
                // Refuse before any account storage, network, or credential access.
                Err(DirectError::Transport)
            }
        }
        ServerConnection::direct(
            "spaceterm-development",
            [route; 32],
            PairedDevice {
                pin: ServerPin {
                    build: "spaceterm-development".into(),
                    server: [server; 32],
                    public_key: [7; 32],
                    certificate: [8; 32],
                },
                pairing: [9; 32],
                client: [client; 32],
            },
            Arc::new(Offline),
        )
        .unwrap()
    }

    #[gpui::test]
    fn disconnect_and_connect_cannot_unlock_a_delayed_start_on_a_paired_route(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::ui::server_worktrees::Client;
        {
            // A credential-bound handle with an offline connector keeps this writer fixture
            // entirely in memory while retaining the validated pairing fences.
            let server = disconnected_direct_handle(31, 32, 6);
            let client = cx.update(|cx| Client::for_direct([31; 32], server.clone(), cx));
            let (factory, state) = DevelopmentServerSessionFactory::for_origin(
                server.clone(),
                &client.controls,
                None,
                None,
                [2; 32],
            );
            lose_start(&state);
            cx.update(|cx| {
                Client::forget_direct(&[31; 32], cx);
                assert!(Client::direct(&[31; 32], cx).is_none());
            });
            drop(factory);
            drop(state);
            drop(client);
            let client = cx.update(|cx| Client::for_direct([31; 32], server.clone(), cx));
            let (_, state) = DevelopmentServerSessionFactory::for_origin(
                server.clone(),
                &client.controls,
                None,
                None,
                [2; 32],
            );
            assert!(
                state.start_unconfirmed(),
                "Disconnect cannot settle the original start"
            );
            state.select();
            let result = run_claim(&state, |request| {
                assert!(matches!(
                    request,
                    Request::SelectPane {
                        explicit: false,
                        ..
                    }
                ));
                state.start_restored();
                Ok(restored(1))
            });
            assert!(matches!(result, Err(ServerError::Protocol)));
            assert!(state.start_unconfirmed());
            assert!(!state.selected.load(Ordering::Acquire));
            assert!(!state.start_restored.load(Ordering::Acquire));

            // Transport replacement with the same bound identities also retains the original.
            let same_identity = disconnected_direct_handle(31, 32, 6);
            let replacement =
                cx.update(|cx| Client::for_direct([31; 32], same_identity.clone(), cx));
            let (_, replacement_state) = DevelopmentServerSessionFactory::for_origin(
                same_identity,
                &replacement.controls,
                None,
                None,
                [2; 32],
            );
            assert!(replacement_state.start_unconfirmed());

            // A different client or server on this presentation route cannot inherit its gate.
            for other in [
                disconnected_direct_handle(31, 33, 6),
                disconnected_direct_handle(31, 32, 7),
            ] {
                let client = cx.update(|cx| Client::for_direct([31; 32], other.clone(), cx));
                let (_, other_state) = DevelopmentServerSessionFactory::for_origin(
                    other,
                    &client.controls,
                    None,
                    None,
                    [2; 32],
                );
                assert!(!other_state.start_unconfirmed());
            }
            let independent = cx.update(|cx| Client::for_direct([34; 32], server.clone(), cx));
            let (_, independent_state) = DevelopmentServerSessionFactory::for_origin(
                server,
                &independent.controls,
                None,
                None,
                [2; 32],
            );
            assert!(
                !independent_state.start_unconfirmed(),
                "presentation routes remain independent"
            );
        }
    }

    #[gpui::test]
    fn an_unvalidated_route_cannot_dispatch_pane_requests(cx: &mut gpui::TestAppContext) {
        use crate::ui::server_worktrees::Client;
        let server = ServerConnection::for_identity("spaceterm-development").unwrap();
        let client = cx.update(|cx| Client::for_direct([31; 32], server, cx));
        let control = client.controls.pane([2; 32]);
        let presenter = control.reserve().activate(&AtomicBool::new(true));
        let mut sent = false;
        let result = presenter.send(
            &Request::StartRestored {
                server: [1; 32],
                pane: [2; 32],
                size: PaneSize {
                    columns: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                style: None,
            },
            |_| {
                sent = true;
                Err(ServerError::Protocol)
            },
        );
        assert_eq!(result.err(), Some(ServerError::AuthenticationRefused));
        assert!(!sent, "identity refusal must happen before wire dispatch");
    }

    #[test]
    fn takeover_of_an_ended_pane_retires_earlier_selection_and_preserves_later_selection() {
        let state = DevelopmentPaneState::new();
        let alive = AtomicBool::new(true);
        let size = Mutex::new(PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        });
        let appearance = Mutex::new(TerminalAppearanceUpdate::builtin().unwrap());
        let mut attached_session = false;
        let mut takeover = false;
        let mut paste_unconfirmed = false;
        let target = pane::PaneTarget {
            server: [1; 32],
            session: [2; 32],
        };
        let lease = Lease {
            server: target.server,
            session: [3; 32],
            control: [4; 32],
            connection: [5; 32],
        };
        let mut phase = 0;
        let granted = ClaimWorker {
            state: &state,
            pane_id: target.session,
            alive: &alive,
            attachment: &None,
            size: &size,
            appearance: &appearance,
            directory_recovery: true,
            attached_session: &mut attached_session,
            takeover: &mut takeover,
            paste_unconfirmed: &mut paste_unconfirmed,
        }.run(|request| {
            let restored = || Reply::Restored {
                fresh: false,
                recovery: None,
                directory_unavailable: false,
                session_ended: true,
                server: target.server,
                workspace: [6; 32],
                tab: [7; 32],
                pane: target.session,
            };
            let offered = || Reply::Takeover {
                target,
                controller: pane::control::Controller::Client {
                    name: "Studio".into(),
                    this_client: false,
                },
                session_ended: true,
            };
            let reply = match phase {
                0 => {
                    assert!(matches!(request, Request::SelectPane { explicit: false, .. }));
                    offered()
                }
                1 => {
                    assert!(matches!(request, Request::SelectPane { explicit: false, .. }));
                    // Selection and Take Over arrive while this passive observation is in flight.
                    state.select();
                    state.take_over();
                    offered()
                }
                2 => {
                    assert!(matches!(request, Request::TakeOver { .. }));
                    restored()
                }
                3 => {
                    assert!(matches!(request, Request::SelectPane { explicit: false, .. }),
                        "Take Over must leave the ended Pane dormant until a later selection; observed {request:?}");
                    assert_eq!(state.status(), DevelopmentPaneStatus::SessionEnded);
                    state.select();
                    restored()
                }
                4 => {
                    assert!(matches!(request, Request::StartRestored { .. }),
                        "a later explicit selection must still start the shell");
                    Reply::Attached { lease }
                }
                _ => panic!("claim continued after receiving its lease"),
            };
            phase += 1;
            Ok(reply)
        }, || {}).unwrap().unwrap();
        assert_eq!(granted.lease, lease);
        assert_eq!(phase, 5);
    }
}
