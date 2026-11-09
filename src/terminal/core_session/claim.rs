//! Acquire Pane control and start only explicitly requested retained shells.
use super::*;
use spaceterm_core::connection::link::{LinkEvent, LinkSink};

#[cfg(test)]
mod process_tests;

pub(super) struct ClaimWorker<'a> {
    pub state: &'a CorePaneState,
    pub pane_id: spaceterm_core::hierarchy::Id,
    pub alive: &'a AtomicBool,
    pub attachment: &'a Option<spaceterm_core::attachments::AttachmentPermit>,
    pub size: &'a Mutex<PaneSize>,
    pub appearance: &'a Mutex<TerminalAppearanceUpdate>,
    pub attached_session: &'a mut bool,
    pub takeover: &'a mut bool,
    pub paste_unconfirmed: &'a mut bool,
}

pub(super) struct Granted {
    pub lease: Lease,
    pub size: PaneSize,
    pub appearance: Option<crate::appearance::AppearanceGeneration>,
}

/// Wakes a claim waiting at the Restored or Takeover screen when the server's answer can change
/// without this client acting: a pushed Workspace Inventory that changes this Pane's live control
/// facts, a detached Workspace, or the end of the link. A person's Start, selection, repair or
/// Take Over wakes the claim directly, so the claim never asks the server again on a timer.
pub(super) struct ControlWatch {
    state: Arc<CorePaneState>,
    pane: spaceterm_core::hierarchy::Id,
    /// This Pane's control facts in the last inventory delivered.
    seen: Mutex<Option<ControlFacts>>,
    /// Held while the watch lives; the attachments keep a weak reference.
    detached: Arc<dyn Fn() + Send + Sync>,
}

/// What a Workspace Inventory says about who may claim one Pane: the server's boot, its live
/// Terminal Session and its control grant. A Pane the inventory no longer lists has neither.
#[derive(Clone, PartialEq, Eq)]
struct ControlFacts {
    boot: spaceterm_core::hierarchy::Id,
    pane: Option<spaceterm_core::hierarchy::ConfirmedPane>,
    control: Option<spaceterm_core::hierarchy::ConfirmedControl>,
}

impl ControlFacts {
    fn of(
        inventory: &spaceterm_core::hierarchy::Inventory,
        pane: spaceterm_core::hierarchy::Id,
    ) -> Option<Self> {
        let close = inventory.close.as_ref()?;
        Some(Self {
            boot: close.boot,
            pane: close.sessions.iter().find(|p| p.id == pane).cloned(),
            control: close
                .controls
                .iter()
                .find(|control| control.pane == pane)
                .cloned(),
        })
    }
}

impl ControlWatch {
    pub(super) fn new(state: Arc<CorePaneState>, pane: spaceterm_core::hierarchy::Id) -> Arc<Self> {
        let detached: Arc<dyn Fn() + Send + Sync> = {
            let state = Arc::clone(&state);
            Arc::new(move || state.wake.notify())
        };
        Arc::new(Self {
            state,
            pane,
            seen: Mutex::new(None),
            detached,
        })
    }

    /// Follows the shared link of `server` and the Workspace attachment of `permit`, as long as
    /// the returned watch lives.
    pub(super) fn start(
        core: &MachineConnection,
        state: Arc<CorePaneState>,
        pane: spaceterm_core::hierarchy::Id,
        permit: Option<&spaceterm_core::attachments::AttachmentPermit>,
    ) -> Arc<Self> {
        let watch = Self::new(state, pane);
        core.subscribe(Arc::downgrade(&watch) as std::sync::Weak<dyn LinkSink>);
        if let Some(permit) = permit {
            permit.watch(Arc::downgrade(&watch.detached));
        }
        watch
    }
}

impl LinkSink for ControlWatch {
    fn deliver(&self, event: LinkEvent) {
        let changed = match event {
            LinkEvent::Inventory(revision) => {
                self.state.core_reachable();
                let facts = ControlFacts::of(&revision.inventory, self.pane);
                let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
                (*seen != facts) && {
                    *seen = facts;
                    true
                }
            }
            // The next request learns how the link ended.
            LinkEvent::Health(_) => true,
            LinkEvent::UpdateFacts(_) => {
                self.state.changed();
                true
            }
        };
        if changed {
            self.state.wake.notify();
        }
    }
}

/// Runs in the presenter's send turn, so replacement cannot overtake recovery bookkeeping.
pub(super) fn exchange(
    state: &CorePaneState,
    pane_id: spaceterm_core::hierarchy::Id,
    request: &Request,
    exchange: impl FnOnce(&Request) -> Result<Reply, CoreError>,
) -> Result<Reply, CoreError> {
    let result = bookkept(state, pane_id, request, exchange);
    // A start's confirmation and directory progress may have moved.
    state.changed();
    result
}

fn bookkept(
    state: &CorePaneState,
    pane_id: spaceterm_core::hierarchy::Id,
    request: &Request,
    exchange: impl FnOnce(&Request) -> Result<Reply, CoreError>,
) -> Result<Reply, CoreError> {
    let start = match request {
        Request::StartRestored { core, .. } => Some(Some(*core)),
        Request::RepairDirectory { target, .. } => Some(Some(target.boot)),
        Request::SelectPane { explicit: true, .. } => Some(None),
        Request::TakeOver { target, .. }
            if state.session_end().is_some() || target.session == pane_id =>
        {
            Some(Some(target.core))
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
            Err(CoreError::Protocol)
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
    if !state.start_unconfirmed() {
        state
            .directory_note
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
    Ok(reply)
}

impl ClaimWorker<'_> {
    pub fn run(
        self,
        mut exchange: impl FnMut(&Request) -> Result<Reply, CoreError>,
        mut discard_presentation: impl FnMut(),
    ) -> Result<Option<Granted>, CoreError> {
        let Self {
            state,
            pane_id,
            alive,
            attachment,
            size,
            appearance,
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
            Err(CoreError::DirectoryUnavailable) => {
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
                CoreError::Controlled => CoreError::Unsupported,
                CoreError::StaleControl => CoreError::SessionUnavailable,
                other => other,
            })?,
        };
        // The creation's first-use intent survives a restart before its claim. A passive
        // observation still precedes the start, so only the Core can grant control.
        let mut created_start = state.created_start.load(Ordering::Acquire)
            && !state.start_unconfirmed()
            && matches!(
                reply,
                Reply::Restored {
                    session_ended: None,
                    ..
                } | Reply::Takeover {
                    session_ended: None,
                    ..
                }
            );
        if !created_start {
            state.created_start.store(false, Ordering::Release);
        }
        while matches!(reply, Reply::Restored { .. } | Reply::Takeover { .. }) {
            let mut dormant = false;
            while let Reply::Restored {
                core: boot,
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
                    if state.directory_busy.swap(false, Ordering::AcqRel)
                        && state.start_unconfirmed()
                    {
                        *state
                            .directory_note
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) =
                            Some("Starting the shell was not confirmed.");
                    }
                    discard_presentation();
                }
                *state.recovery.lock().unwrap_or_else(|e| e.into_inner()) = recovery.clone();
                unavailable |= directory_unavailable;
                state.set_session_end(session_ended);
                state.admission(Admission::Waiting);
                state.set(if unavailable {
                    CorePaneStatus::Failed(CoreError::DirectoryUnavailable)
                } else if let Some(end) = session_ended {
                    CorePaneStatus::SessionEnded(end)
                } else if fresh {
                    CorePaneStatus::Connecting
                } else {
                    CorePaneStatus::Restored
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
                if created {
                    state.created_start.store(false, Ordering::Release);
                }
                let start_restored = state.start_restored.swap(false, Ordering::AcqRel);
                // Selection never starts a Session that ended in this server run.
                let selected =
                    state.selected.swap(false, Ordering::AcqRel) && session_ended.is_none();
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
                            target: recovery.clone().ok_or(CoreError::Protocol)?,
                            replacement: None,
                            size: grant_size,
                            style: Some(encode_style(&update)),
                        },
                        None => Request::StartRestored {
                            core: boot,
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
                        Err(CoreError::DirectoryUnavailable) => {
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
                        Err(error) => {
                            if state.retain_start_after(error) {
                                state.start_restored.store(true, Ordering::Release);
                            }
                            return Err(error);
                        }
                        Ok(reply) => reply,
                    };
                } else {
                    // Start, selection, repair and dismissal wake this wait, and so does
                    // anything `ControlWatch` sees that can change the server's answer.
                    state.wake.wait(None);
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
                let dormant_target = session_ended.is_some() || target.session == pane_id;
                if !dormant_target {
                    // Another client already started a shell. No retained intent may replace it.
                    state.start_restored.store(false, Ordering::Release);
                    state.created_start.store(false, Ordering::Release);
                    created_start = false;
                }
                let queued_start = dormant_target
                    && !state.start_unconfirmed()
                    && state.start_restored.swap(false, Ordering::AcqRel);
                if state.take_over.swap(false, Ordering::AcqRel) || queued_start {
                    state.admission(Admission::Pending);
                    state.set(CorePaneStatus::TakingOver);
                    grant_size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    let update = appearance.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    grant_appearance = Some(update.generation);
                    reply = exchange(&Request::TakeOver {
                        target,
                        size: grant_size,
                        style: Some(encode_style(&update)),
                    })
                    .inspect_err(|error| {
                        if dormant_target && state.retain_start_after(*error) {
                            state.start_restored.store(true, Ordering::Release);
                        }
                    })?;
                } else {
                    // Take Over, selection and dismissal wake this wait, and so does anything
                    // `ControlWatch` sees that can change the server's answer.
                    state.wake.wait(None);
                    let size = *size.lock().unwrap_or_else(|e| e.into_inner());
                    grant_size = size;
                    grant_appearance = None;
                    reply = exchange(&Request::SelectPane {
                        pane: pane_id,
                        explicit: state.selected.swap(false, Ordering::AcqRel)
                            && session_ended.is_none(),
                        size,
                        style: None,
                    })?;
                }
            }
        }
        let Reply::Attached { lease } = reply else {
            return Err(CoreError::Protocol);
        };
        // A selection made while the admitting request was in flight selected this running
        // shell. It cannot authorize another shell after a later server restart.
        {
            // Serialize with selection of a Connecting placeholder, including a selection
            // made after admission but before the first hidden screen arrives.
            let _status = state.status.lock().unwrap_or_else(|e| e.into_inner());
            state.admission(Admission::Granted);
            state.created_start.store(false, Ordering::Release);
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
        state: &CorePaneState,
        mut core: impl FnMut(&Request) -> Result<Reply, CoreError>,
    ) -> Result<Option<Granted>, CoreError> {
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
            attached_session: &mut false,
            takeover: &mut false,
            paste_unconfirmed: &mut false,
        }
        .run(
            |request| exchange(state, [2; 32], request, &mut core),
            || {},
        )
    }

    fn restored(boot: u8) -> Reply {
        Reply::Restored {
            fresh: false,
            core: [boot; 32],
            pane: [2; 32],
            workspace: [3; 32],
            tab: [4; 32],
            recovery: None,
            directory_unavailable: false,
            session_ended: None,
        }
    }

    fn reachable(watch: &ControlWatch) {
        use spaceterm_core::{hierarchy::Inventory, protocol::InventoryRevision};
        watch.deliver(LinkEvent::Inventory(Arc::new(InventoryRevision {
            inventory: Inventory {
                revision: 1,
                workspaces: Vec::new(),
                close: None,
                repositories: None,
            },
            next_sequence: 1,
        })));
    }

    #[test]
    fn start_session_in_another_clients_ended_pane_waits_for_reconnect() {
        let state = CorePaneState::new();
        let end = pane::SessionEnd::Exited { code: 1 };
        state.offered_take_over(pane::control::Controller::Nobody, Some(end));
        assert!(state.connection_failed(CoreError::ServiceBusy, true));
        state.start_restored();
        assert!(state.start_waiting());
        let mut requests = 0;
        let granted = run_claim(&state, |request| {
            requests += 1;
            match requests {
                1 => Ok(Reply::Takeover {
                    target: pane::PaneTarget {
                        core: [2; 32],
                        session: [2; 32],
                    },
                    controller: pane::control::Controller::Nobody,
                    session_ended: Some(end),
                }),
                2 => {
                    assert!(matches!(request, Request::TakeOver { .. }));
                    Ok(Reply::Attached {
                        lease: Lease {
                            core: [2; 32],
                            session: [3; 32],
                            control: [4; 32],
                            connection: [5; 32],
                        },
                    })
                }
                _ => panic!("unexpected claim {request:?}"),
            }
        })
        .unwrap();
        assert!(granted.is_some());
        assert!(!state.start_waiting());
    }

    #[test]
    fn a_start_session_click_survives_replacement_of_its_presentation() {
        let core = MachineConnection::for_identity("spaceterm-development").unwrap();
        let controls = PaneControls::default();
        let open = || {
            CoreSessionFactory::for_remote_pane(
                core.clone(),
                &controls,
                "penguin".into(),
                [2; 32],
                Arc::default(),
            )
        };
        let (_first, state) = open();
        state.set(CorePaneStatus::Restored);
        state.start_restored();
        assert!(state.connection_failed(CoreError::ServiceBusy, false));
        let (_replacement, state) = open();
        let mut requests = 0;
        let granted = run_claim(&state, |request| {
            requests += 1;
            match requests {
                1 => Ok(restored(2)),
                2 => {
                    assert!(
                        matches!(request, Request::StartRestored { .. }),
                        "queued start did not reach the replacement: {request:?}"
                    );
                    Ok(Reply::Attached {
                        lease: Lease {
                            core: [2; 32],
                            session: [3; 32],
                            control: [4; 32],
                            connection: [5; 32],
                        },
                    })
                }
                _ => panic!("unexpected claim {request:?}"),
            }
        })
        .unwrap();
        assert!(granted.is_some());
        let (_, reopened) = open();
        assert!(
            !reopened.start_waiting(),
            "the completed intent must not start a later shell"
        );
    }

    #[test]
    fn a_busy_created_start_completes_after_reconnecting_to_a_new_boot() {
        for busy_on_observation in [false, true] {
            let state = CorePaneState::new();
            state.start_created();
            let mut requests = 0;
            assert!(matches!(
                run_claim(&state, |request| {
                    requests += 1;
                    match requests {
                        1 => {
                            assert!(matches!(
                                request,
                                Request::SelectPane {
                                    explicit: false,
                                    ..
                                }
                            ));
                            if busy_on_observation {
                                return Err(CoreError::ServiceBusy);
                            }
                            let mut reply = restored(1);
                            if let Reply::Restored { fresh, .. } = &mut reply {
                                *fresh = true;
                            }
                            Ok(reply)
                        }
                        2 => {
                            assert!(matches!(request, Request::StartRestored { .. }));
                            Err(CoreError::ServiceBusy)
                        }
                        _ => panic!("unexpected claim {request:?}"),
                    }
                }),
                Err(CoreError::ServiceBusy)
            ));
            assert!(state.connection_failed(CoreError::ServiceBusy, false));
            assert!(!state.start_unconfirmed());
            requests = 0;
            let granted = run_claim(&state, |request| {
                requests += 1;
                match requests {
                    1 => Ok(restored(2)),
                    2 => {
                        assert!(
                            matches!(request, Request::StartRestored { core: [2, ..], .. }),
                            "queued start was lost: {request:?}"
                        );
                        Ok(Reply::Attached {
                            lease: Lease {
                                core: [2; 32],
                                session: [3; 32],
                                control: [4; 32],
                                connection: [5; 32],
                            },
                        })
                    }
                    _ => panic!("unexpected claim {request:?}"),
                }
            })
            .unwrap();
            assert!(granted.is_some());
            assert_eq!(state.directory_progress(), (false, None));
        }
    }

    #[test]
    fn a_start_session_click_waits_through_a_connection_loss_before_dispatch() {
        let state = CorePaneState::new();
        state.set(CorePaneStatus::Restored);
        state.start_restored();
        assert!(state.connection_failed(CoreError::ServiceBusy, false));
        let mut requests = 0;
        state.wake.notify();
        assert!(
            run_claim(&state, |request| {
                requests += 1;
                match requests {
                    1 => Ok(restored(2)),
                    2 => {
                        assert!(
                            matches!(request, Request::StartRestored { .. }),
                            "queued click was lost: {request:?}"
                        );
                        Ok(Reply::Attached {
                            lease: Lease {
                                core: [2; 32],
                                session: [3; 32],
                                control: [4; 32],
                                connection: [5; 32],
                            },
                        })
                    }
                    _ => panic!("unexpected claim {request:?}"),
                }
            })
            .unwrap()
            .is_some()
        );
    }

    /// The exchange double changes reachability without opening a network connection. A long
    /// outage exhausts the Pane's retries, then the route's confirmed inventory wakes it again.
    #[test]
    fn a_paired_pane_reconnects_when_its_machine_becomes_reachable() {
        let state = Arc::new(CorePaneState::new());
        let watch = ControlWatch::new(Arc::clone(&state), [2; 32]);
        // An inventory may confirm reachability without changing this Pane's control facts.
        reachable(&watch);
        for attempt in 0..4 {
            let failure = run_claim(&state, |_| {
                Err(CoreError::Direct(
                    spaceterm_core::direct::DirectError::Transport,
                ))
            })
            .err()
            .unwrap();
            assert_eq!(state.connection_failed(failure, false), attempt < 3);
        }
        assert!(!state.retry.load(Ordering::Acquire));
        reachable(&watch);
        assert!(state.retry.swap(false, Ordering::AcqRel));
        let grant = run_claim(&state, |request| {
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            Ok(Reply::Attached {
                lease: Lease {
                    core: [1; 32],
                    session: [3; 32],
                    control: [4; 32],
                    connection: [5; 32],
                },
            })
        })
        .unwrap()
        .expect("the existing Terminal Session reconnects without Try Again");
        assert_eq!(grant.lease.session, [3; 32]);
    }

    #[test]
    fn a_paired_pane_recovers_a_restarted_machine_without_starting_a_shell() {
        let state = Arc::new(CorePaneState::new());
        let watch = ControlWatch::new(Arc::clone(&state), [2; 32]);
        let failure = run_claim(&state, |_| {
            Err(CoreError::Direct(
                spaceterm_core::direct::DirectError::Transport,
            ))
        })
        .err()
        .unwrap();
        assert!(state.connection_failed(failure, false));
        reachable(&watch);
        assert!(state.retry.swap(false, Ordering::AcqRel));

        use crate::ui::core_pane::{CorePaneScreen, CorePaneView, DormantCopy};
        let alive = AtomicBool::new(true);
        let mut view = CorePaneView::new(state.clone());
        let mut starts = 0;
        let mut passive_after_decision = 0;
        // The inventory wake lets the claim pass its no-start decision and observe once more.
        state.wake.notify();
        let grant = ClaimWorker {
            state: &state,
            pane_id: [2; 32],
            alive: &alive,
            attachment: &None,
            size: &Mutex::new(PaneSize {
                columns: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            }),
            appearance: &Mutex::new(TerminalAppearanceUpdate::builtin().unwrap()),
            attached_session: &mut true,
            takeover: &mut false,
            paste_unconfirmed: &mut false,
        }
        .run(
            |request| {
                match request {
                    Request::SelectPane {
                        explicit: false,
                        style,
                        ..
                    } => {
                        if style.is_none() {
                            passive_after_decision += 1;
                            view.sync();
                            assert_eq!(
                                view.screen(),
                                Some(CorePaneScreen::Restored {
                                    starting: false,
                                    unconfirmed: false,
                                    offered: true,
                                    ended: None,
                                })
                            );
                            let Some(CorePaneScreen::Restored { ended, .. }) = view.screen() else {
                                unreachable!()
                            };
                            let copy = DormantCopy::of(ended);
                            assert_eq!(
                                (copy.title.as_ref(), copy.label),
                                ("SpaceTerm Core Restarted", "Start Session")
                            );
                            // Teardown only after the live claim chose observation over Start.
                            alive.store(false, Ordering::Release);
                        }
                    }
                    Request::StartRestored { .. } | Request::SelectPane { explicit: true, .. } => {
                        starts += 1;
                        alive.store(false, Ordering::Release);
                    }
                    _ => panic!("reconnection must only observe the Pane"),
                }
                exchange(&state, [2; 32], request, |_| Ok(restored(9)))
            },
            || {},
        )
        .unwrap();
        assert!(grant.is_none());
        assert_eq!(starts, 0, "reachability authorized a replacement shell");
        assert_eq!(passive_after_decision, 1);
        assert_eq!(state.status(), CorePaneStatus::Restored);
    }

    #[test]
    fn a_paired_transport_loss_keeps_a_start_unconfirmed_until_passive_recovery() {
        let state = Arc::new(CorePaneState::new());
        let size = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        exchange(
            &state,
            [2; 32],
            &Request::SelectPane {
                pane: [2; 32],
                explicit: false,
                size,
                style: None,
            },
            |_| Ok(restored(1)),
        )
        .unwrap();
        let lost = CoreError::Direct(DirectError::Transport);
        let result = exchange(
            &state,
            [2; 32],
            &Request::StartRestored {
                core: [1; 32],
                pane: [2; 32],
                size,
                style: None,
            },
            |_| Err(lost),
        );
        assert_eq!(result.err(), Some(lost));
        assert!(
            state.start_unconfirmed(),
            "a transport loss forgot the submitted shell start"
        );
        assert!(state.connection_failed(lost, false));
        reachable(&ControlWatch::new(Arc::clone(&state), [2; 32]));
        assert!(state.retry.swap(false, Ordering::AcqRel));
        let grant = run_claim(&state, |request| {
            assert!(matches!(
                request,
                Request::SelectPane {
                    explicit: false,
                    ..
                }
            ));
            Ok(Reply::Attached {
                lease: Lease {
                    core: [1; 32],
                    session: [3; 32],
                    control: [4; 32],
                    connection: [5; 32],
                },
            })
        })
        .unwrap()
        .expect("passive observation resolves the original successful start");
        assert_eq!(grant.lease.session, [3; 32]);
        assert!(!state.start_unconfirmed());
    }

    #[test]
    fn fifteen_hidden_creation_grants_release_startup_without_a_screen() {
        use crate::runtime::HostCommands as _;
        use spaceterm_core::hierarchy::Mutation;

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
                            source: None,
                            control: spaceterm_core::hierarchy::CreateControl::Creator,
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
            let state = CorePaneState::new();
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
            crate::runtime::HostCommands::view(&mut host, &grant.lease, false, 60);
            observations.push((
                state.startup_pending(),
                state.status(),
                state.interactive_input(),
            ));
        }
        // Stop every fixture PTY before any assertion can fail.
        host.shutdown().unwrap();
        assert_eq!(observations.len(), 15);
        for (pending, status, input) in observations {
            assert_eq!(status, CorePaneStatus::Connecting);
            assert_eq!(input, crate::terminal::InteractiveInput::Pending);
            assert!(
                !pending,
                "an authenticated creation grant releases the startup slot before a screen"
            );
        }
    }

    #[test]
    fn never_visited_hidden_pane_prepares_current_rows_without_visible_cadence() {
        use crate::runtime::HostCommands as _;
        use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize};

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
        let state = Arc::new(CorePaneState::new());
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
                crate::runtime::directory_testing::type_keys(
                    &mut host,
                    lease,
                    vec![encode_key(key)]
                ),
                Reply::Unchanged
            ));
        }
        handle.set_preparation_demand(true);
        let start = std::time::Instant::now();
        let mut demand = super::inbox::Demand::new(false, 60);
        // Read the same demand inputs that the production presentation worker uses.
        demand.set_preparation(state.preparation.load(Ordering::Acquire));
        let mut prepared = false;
        while start.elapsed() < Duration::from_secs(3) {
            if let Some((visible, max_hz)) = demand.take_view() {
                crate::runtime::HostCommands::view(&mut host, &lease, visible, max_hz);
            }
            prepared =
                crate::terminal::core_session::screen_decoder::observed_lines(&mut host, lease)
                    .iter()
                    .any(|row| row.trim_end() == "HIDDEN-PREPARED");
            if prepared {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.set_preparation_demand(false);
        demand.set_preparation(state.preparation.load(Ordering::Acquire));
        let withdrawn = demand.take_view();
        host.shutdown().unwrap();
        assert!(
            prepared,
            "bounded preparation demand must supply real hidden output rows"
        );
        assert!(!state.presentable.load(Ordering::Acquire));
        assert!(
            !demand.visible(),
            "preparation must not enable graphics fetching"
        );
        assert_eq!(withdrawn, Some((false, 60)));
        assert_eq!(
            state.interactive_input(),
            crate::terminal::InteractiveInput::Pending
        );
    }

    #[test]
    fn fresh_panes_never_present_core_restarted_before_first_use() {
        use crate::runtime::HostCommands as _;
        use crate::ui::core_pane::CorePaneView;
        use spaceterm_core::hierarchy::Mutation;

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
                                source: None,
                                control: spaceterm_core::hierarchy::CreateControl::Creator,
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
                let state = Arc::new(CorePaneState::new());
                let mut view = CorePaneView::new(state.clone());
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
        let state = CorePaneState::new();
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
                Err(CoreError::Protocol)
            }
            other => panic!("creation must not acquire control: {other:?}"),
        });
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert_eq!(starts, 1);
        assert!(state.start_unconfirmed());
        assert!(
            state.startup_pending(),
            "an unconfirmed original still occupies its slot"
        );
        state.connection_failed(CoreError::Protocol, false);
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
                Err(CoreError::Stopped)
            }
        });
        assert_eq!(starts, 1);
        assert!(!state.created_start.load(Ordering::Acquire));
    }

    #[test]
    fn creation_and_selection_are_consumed_together_before_a_later_machine_restart() {
        let state = CorePaneState::new();
        state.start_created();
        state.select();
        let lease = Lease {
            core: [1; 32],
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
        assert_eq!(state.status(), CorePaneStatus::Connecting);
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
                Err(CoreError::Stopped)
            }
        });
        assert!(!state.selected.load(Ordering::Acquire));
    }

    #[test]
    fn creation_intent_does_not_restart_ended_or_take_over_running_work() {
        for taken_over in [false, true] {
            let state = CorePaneState::new();
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
                    "creation must not restart an ended shell or take over another client's running shell"
                );
                observations += 1;
                if observations > 1 {
                    assert!(
                        !state.startup_pending(),
                        "waiting for first use or Take Over must release the startup slot"
                    );
                    return Err(CoreError::Stopped);
                }
                Ok(if taken_over {
                    Reply::Takeover {
                        target: pane::PaneTarget {
                            core: [1; 32],
                            session: [5; 32],
                        },
                        controller: pane::control::Controller::Nobody,
                        session_ended: None,
                    }
                } else {
                    let mut reply = restored(1);
                    if let Reply::Restored { session_ended, .. } = &mut reply {
                        *session_ended = Some(pane::SessionEnd::Exited { code: 1 });
                    }
                    reply
                })
            });
            assert!(!state.created_start.load(Ordering::Acquire));
        }
    }

    fn lose_start(state: &CorePaneState) {
        let mut starts = 0;
        let result = run_claim(state, |request| match request {
            Request::SelectPane {
                explicit: false, ..
            } => {
                // Select once to enter Restored, then request a start on its next selection check.
                state.start_restored();
                Ok(restored(1))
            }
            Request::StartRestored { .. } => {
                starts += 1;
                Err(CoreError::Protocol)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert_eq!(starts, 1);
        state.connection_failed(CoreError::Protocol, false);
    }

    #[test]
    fn a_lost_takeover_of_another_clients_restored_pane_blocks_another_start() {
        let state = CorePaneState::new();
        let offered = || Reply::Takeover {
            target: pane::PaneTarget {
                core: [1; 32],
                // A retained Pane without a live shell uses its stable Pane identity here.
                session: [2; 32],
            },
            controller: pane::control::Controller::Client {
                name: "Other client".into(),
                this_client: false,
            },
            // Restored organization has no prior ended Session, unlike End All Sessions.
            session_ended: None,
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
                Err(CoreError::Protocol)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert_eq!(starts, 1);
        assert!(
            state.start_unconfirmed(),
            "Take Over can start this restored shell"
        );
        state.connection_failed(CoreError::Protocol, true);
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
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert!(!state.take_over.load(Ordering::Acquire));
        assert!(!state.selected.load(Ordering::Acquire));
        assert!(state.start_unconfirmed());
    }

    #[test]
    fn a_dormant_observation_cannot_replace_a_delayed_start_on_the_old_connection() {
        let state = CorePaneState::new();
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
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert!(state.start_unconfirmed());
        // Even an explicit action reaching the backend cannot produce another shell start.
        state.set(CorePaneStatus::SessionEnded(
            pane::SessionEnd::EndAllSessions,
        ));
        state.start_restored();
        state.select();
        assert!(!state.start_restored.load(Ordering::Acquire));
        assert!(!state.selected.load(Ordering::Acquire));
        state.offered_take_over(
            pane::control::Controller::Nobody,
            Some(pane::SessionEnd::EndAllSessions),
        );
        state.take_over();
        assert!(!state.take_over.load(Ordering::Acquire));
        let target = pane::directories::Target {
            boot: [1; 32],
            pane: [2; 32],
            original: "/original".into(),
        };
        *state.recovery.lock().unwrap() = Some(target.clone());
        state.set(CorePaneStatus::Failed(CoreError::DirectoryUnavailable));
        state.repair_directory(target, "/replacement".into());
        assert!(state.repair.lock().unwrap().is_none());
        // The delayed original finally runs. Observing its live grant settles it without replay.
        let lease = Lease {
            core: [1; 32],
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
    fn a_new_core_boot_settles_the_old_start_before_a_fresh_start() {
        let state = CorePaneState::new();
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
            Request::StartRestored { core, .. } => {
                starts += 1;
                assert_eq!(*core, [8; 32]);
                Err(CoreError::Unsupported)
            }
            other => panic!("unexpected request {other:?}"),
        });
        assert!(matches!(result, Err(CoreError::Unsupported)));
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
        let core = MachineConnection::for_identity("spaceterm-development").unwrap();
        let open = |controls: &PaneControls, pane| {
            CoreSessionFactory::for_origin(core.clone(), controls, None, None, pane, Arc::default())
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
        assert!(matches!(result, Err(CoreError::Protocol)));
        assert!(reopened.start_unconfirmed());
        assert!(!open(&controls, [9; 32]).1.start_unconfirmed());
        assert!(
            !open(&PaneControls::default(), [2; 32])
                .1
                .start_unconfirmed()
        );
    }

    fn disconnected_direct_handle(route: u8, client: u8, core: u8) -> MachineConnection {
        use spaceterm_core::direct::{
            DirectConnection, DirectConnector, DirectError, MachinePin, PairedDevice,
        };
        struct Offline;
        impl DirectConnector for Offline {
            fn connect(&self) -> Result<DirectConnection, DirectError> {
                // Refuse before any account storage, network, or credential access.
                Err(DirectError::Transport)
            }
        }
        MachineConnection::direct(
            "spaceterm-development",
            [route; 32],
            PairedDevice {
                pin: MachinePin {
                    build: "spaceterm-development".into(),
                    core: [core; 32],
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
        use crate::ui::core_worktrees::Client;
        {
            // A credential-bound handle with an offline connector keeps this writer fixture
            // entirely in memory while retaining the validated pairing fences.
            let core = disconnected_direct_handle(31, 32, 6);
            let client = cx.update(|cx| Client::for_direct([31; 32], core.clone(), cx));
            let (factory, state) = CoreSessionFactory::for_origin(
                core.clone(),
                &client.controls,
                None,
                None,
                [2; 32],
                Arc::default(),
            );
            lose_start(&state);
            cx.update(|cx| {
                Client::forget_direct(&[31; 32], cx);
                assert!(Client::direct(&[31; 32], cx).is_none());
            });
            drop(factory);
            drop(state);
            drop(client);
            let client = cx.update(|cx| Client::for_direct([31; 32], core.clone(), cx));
            let (_, state) = CoreSessionFactory::for_origin(
                core.clone(),
                &client.controls,
                None,
                None,
                [2; 32],
                Arc::default(),
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
            assert!(matches!(result, Err(CoreError::Protocol)));
            assert!(state.start_unconfirmed());
            assert!(!state.selected.load(Ordering::Acquire));
            assert!(!state.start_restored.load(Ordering::Acquire));

            // Transport replacement with the same bound identities also retains the original.
            let same_identity = disconnected_direct_handle(31, 32, 6);
            let replacement =
                cx.update(|cx| Client::for_direct([31; 32], same_identity.clone(), cx));
            let (_, replacement_state) = CoreSessionFactory::for_origin(
                same_identity,
                &replacement.controls,
                None,
                None,
                [2; 32],
                Arc::default(),
            );
            assert!(replacement_state.start_unconfirmed());

            // A different client or server on this presentation route cannot inherit its gate.
            for other in [
                disconnected_direct_handle(31, 33, 6),
                disconnected_direct_handle(31, 32, 7),
            ] {
                let client = cx.update(|cx| Client::for_direct([31; 32], other.clone(), cx));
                let (_, other_state) = CoreSessionFactory::for_origin(
                    other,
                    &client.controls,
                    None,
                    None,
                    [2; 32],
                    Arc::default(),
                );
                assert!(!other_state.start_unconfirmed());
            }
            let independent = cx.update(|cx| Client::for_direct([34; 32], core.clone(), cx));
            let (_, independent_state) = CoreSessionFactory::for_origin(
                core,
                &independent.controls,
                None,
                None,
                [2; 32],
                Arc::default(),
            );
            assert!(
                !independent_state.start_unconfirmed(),
                "presentation routes remain independent"
            );
        }
    }

    #[gpui::test]
    fn an_unvalidated_route_cannot_dispatch_pane_requests(cx: &mut gpui::TestAppContext) {
        use crate::ui::core_worktrees::Client;
        let core = MachineConnection::for_identity("spaceterm-development").unwrap();
        let client = cx.update(|cx| Client::for_direct([31; 32], core, cx));
        let control = client.controls.pane([2; 32]);
        let presenter = control.reserve().activate(&AtomicBool::new(true));
        let mut sent = false;
        let result = presenter.send(
            &Request::StartRestored {
                core: [1; 32],
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
                Err(CoreError::Protocol)
            },
        );
        assert_eq!(result.err(), Some(CoreError::AuthenticationRefused));
        assert!(!sent, "identity refusal must happen before wire dispatch");
    }

    #[test]
    fn a_waiting_claim_asks_again_only_when_its_pane_control_facts_change() {
        use spaceterm_core::connection::link::Health;
        use spaceterm_core::hierarchy::{
            CloseInventory, ConfirmedControl, ConfirmedPane, Inventory,
        };
        use spaceterm_core::protocol::InventoryRevision;
        use std::sync::atomic::AtomicUsize;
        use std::time::{Duration, Instant};

        let pane = [2; 32];
        let session = [3; 32];
        // Organization revisions change without moving this Pane's control.
        let revision = |owner: u8, epoch: u64, revision: u64| {
            let inventory = Inventory {
                repositories: None,
                revision,
                workspaces: Vec::new(),
                close: Some(CloseInventory {
                    boot: [1; 32],
                    sessions: vec![ConfirmedPane {
                        id: pane,
                        session: Some(session),
                    }],
                    controls: vec![ConfirmedControl {
                        pane,
                        session: Some(session),
                        owner: [owner; 32],
                        epoch,
                    }],
                }),
            };
            LinkEvent::Inventory(Arc::new(InventoryRevision {
                inventory,
                next_sequence: 1,
            }))
        };
        let state = Arc::new(CorePaneState::new());
        let watch = ControlWatch::new(Arc::clone(&state), pane);
        // The first revision the watch sees is new to it.
        watch.deliver(revision(9, 1, 1));
        let selections = AtomicUsize::new(0);
        let released = AtomicBool::new(false);
        let settled = |count: usize| {
            let until = Instant::now() + Duration::from_secs(5);
            while selections.load(Ordering::SeqCst) < count {
                assert!(Instant::now() < until, "the claim did not ask again");
                std::thread::sleep(Duration::from_millis(5));
            }
            // Long enough for several of the old 100 ms checks.
            std::thread::sleep(Duration::from_millis(400));
            selections.load(Ordering::SeqCst)
        };
        std::thread::scope(|scope| {
            let claim = scope.spawn(|| {
                run_claim(&state, |request| {
                    assert!(matches!(
                        request,
                        Request::SelectPane {
                            explicit: false,
                            ..
                        }
                    ));
                    selections.fetch_add(1, Ordering::SeqCst);
                    Ok(if released.load(Ordering::SeqCst) {
                        Reply::Attached {
                            lease: Lease {
                                core: [1; 32],
                                session,
                                control: [4; 32],
                                connection: [5; 32],
                            },
                        }
                    } else {
                        Reply::Takeover {
                            target: pane::PaneTarget {
                                core: [1; 32],
                                session,
                            },
                            controller: pane::control::Controller::Client {
                                name: "Studio".into(),
                                this_client: false,
                            },
                            session_ended: None,
                        }
                    })
                })
            });
            // Entering the Takeover Screen may look once more; then nothing asks while idle.
            let idle = settled(1);
            assert!(idle <= 2, "{idle} selections while nothing changed");
            assert_eq!(settled(idle), idle);
            watch.deliver(revision(9, 1, 2));
            assert_eq!(
                settled(idle),
                idle,
                "an inventory that leaves this Pane's control unchanged asks nothing"
            );
            watch.deliver(LinkEvent::Health(Health::Live));
            let after_health = settled(idle + 1);
            assert_eq!(after_health, idle + 1, "a link event asks once");
            released.store(true, Ordering::SeqCst);
            watch.deliver(revision(1, 2, 3));
            let granted = claim.join().unwrap().unwrap().unwrap();
            assert_eq!(granted.lease.session, session);
            assert_eq!(selections.load(Ordering::SeqCst), after_health + 1);
        });
    }

    /// A selection queued before the claim learns that the Session ended starts nothing. The
    /// Pane waits on Session Ended until Start Session.
    #[test]
    fn a_selection_never_starts_an_ended_pane() {
        let state = CorePaneState::new();
        let alive = AtomicBool::new(true);
        let size = Mutex::new(PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        });
        let appearance = Mutex::new(TerminalAppearanceUpdate::builtin().unwrap());
        let (mut attached_session, mut takeover, mut paste_unconfirmed) = (false, false, false);
        let lease = Lease {
            core: [1; 32],
            session: [9; 32],
            control: [4; 32],
            connection: [5; 32],
        };
        let ended = || Reply::Restored {
            fresh: false,
            recovery: None,
            directory_unavailable: false,
            session_ended: Some(pane::SessionEnd::Exited { code: 1 }),
            core: [1; 32],
            workspace: [6; 32],
            tab: [7; 32],
            pane: [2; 32],
        };
        let mut phase = 0;
        let granted = ClaimWorker {
            state: &state,
            pane_id: [2; 32],
            alive: &alive,
            attachment: &None,
            size: &size,
            appearance: &appearance,
            attached_session: &mut attached_session,
            takeover: &mut takeover,
            paste_unconfirmed: &mut paste_unconfirmed,
        }
        .run(
            |request| {
                let reply = match phase {
                    0 => {
                        // A focus or Tab opening selected the Pane before the answer arrived.
                        state.selected.store(true, Ordering::Release);
                        ended()
                    }
                    1 => {
                        assert!(
                            matches!(
                                request,
                                Request::SelectPane {
                                    explicit: false,
                                    ..
                                }
                            ),
                            "a selection must not start an ended Pane; observed {request:?}"
                        );
                        assert_eq!(
                            state.status(),
                            CorePaneStatus::SessionEnded(pane::SessionEnd::Exited { code: 1 })
                        );
                        state.start_restored();
                        ended()
                    }
                    2 => {
                        assert!(matches!(request, Request::StartRestored { .. }));
                        Reply::Attached { lease }
                    }
                    _ => panic!("claim continued after receiving its lease"),
                };
                phase += 1;
                if phase == 1 {
                    state.wake.notify();
                }
                Ok(reply)
            },
            || {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(granted.lease, lease);
        assert_eq!(phase, 3);
    }

    #[test]
    fn takeover_of_an_ended_pane_starts_it_in_one_request() {
        let state = CorePaneState::new();
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
            core: [1; 32],
            session: [2; 32],
        };
        let lease = Lease {
            core: target.core,
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
            attached_session: &mut attached_session,
            takeover: &mut takeover,
            paste_unconfirmed: &mut paste_unconfirmed,
        }
        .run(
            |request| {
                let offered = || Reply::Takeover {
                    target,
                    controller: pane::control::Controller::Client {
                        name: "Studio".into(),
                        this_client: false,
                    },
                    session_ended: Some(pane::SessionEnd::Exited { code: 1 }),
                };
                let reply = match phase {
                    0 => {
                        assert!(matches!(
                            request,
                            Request::SelectPane {
                                explicit: false,
                                ..
                            }
                        ));
                        offered()
                    }
                    1 => {
                        assert!(matches!(
                            request,
                            Request::SelectPane {
                                explicit: false,
                                ..
                            }
                        ));
                        // Selection and Take Over arrive while this passive observation is in flight.
                        state.select();
                        state.take_over();
                        offered()
                    }
                    2 => {
                        assert!(
                            matches!(request, Request::TakeOver { .. }),
                            "Take Over supersedes the earlier selection; observed {request:?}"
                        );
                        Reply::Attached { lease }
                    }
                    _ => panic!("claim continued after receiving its lease"),
                };
                phase += 1;
                Ok(reply)
            },
            || {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(granted.lease, lease);
        assert_eq!(phase, 3);
    }
}
