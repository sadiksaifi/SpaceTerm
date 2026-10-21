//! Acquire Pane control and start only explicitly requested retained shells.
use super::*;

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
            explicit: state.selected.swap(false, Ordering::AcqRel),
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
        while matches!(reply, Reply::Restored { .. } | Reply::Takeover { .. }) {
            let mut dormant = false;
            while let Reply::Restored {
                server: boot,
                pane,
                ref recovery,
                directory_unavailable,
                session_ended,
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
                state.set(if unavailable {
                    DevelopmentPaneStatus::Failed(ServerError::DirectoryUnavailable)
                } else if session_ended {
                    DevelopmentPaneStatus::SessionEnded
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
                if repair.is_some()
                    || state.start_restored.swap(false, Ordering::AcqRel)
                    || state.selected.swap(false, Ordering::AcqRel)
                {
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
                                "That directory is no longer available. Choose another."
                            } else {
                                "The directory is still unavailable."
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
                if !alive.load(Ordering::Acquire)
                    || attachment.as_ref().is_some_and(|permit| !permit.active())
                {
                    return Ok(None);
                }
                if state.take_over.swap(false, Ordering::AcqRel) {
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
