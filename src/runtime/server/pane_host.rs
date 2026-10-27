//! Pane selection, claim, input and lifecycle for the Development host.
use super::*;

impl DevelopmentPaneHost {
    /// Offers Take Over to `asking`, naming who controls the Pane now.
    pub(super) fn takeover(
        &self,
        target: PaneTarget,
        controlling: Option<[u8; 32]>,
        asking: [u8; 32],
    ) -> Reply {
        let names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        Reply::Takeover {
            session_ended: self.dormant.is_some(),
            target,
            controller: names.controller(controlling, asking),
        }
    }
    pub(super) fn retain(&mut self, organization: Option<Organization>) -> Result<(), Refusal> {
        if self.organization == organization && !self.uncertain {
            return Ok(());
        }
        if let Some(store) = &self.store
            && store.write(organization.as_ref()).is_err()
        {
            self.uncertain = true;
            return Err(Refusal::StorageUnavailable);
        }
        self.organization = organization;
        Ok(())
    }
    pub(super) fn recover(&mut self) -> Result<(), Refusal> {
        if self.uncertain {
            let recovered = self
                .store
                .as_ref()
                .ok_or(Refusal::StorageUnavailable)?
                .read()
                .map_err(|_| Refusal::StorageUnavailable)?;
            if recovered.is_none() && self.pane.is_some() {
                if let Some(pane) = &mut self.pane {
                    pane.runtime
                        .shutdown_and_wait(Duration::from_secs(5))
                        .map_err(|_| Refusal::CleanupUnresolved)?;
                }
                self.pane = None;
            }
            self.organization = recovered;
            self.uncertain = false;
        }
        Ok(())
    }
    fn restored(&self) -> Result<Reply, Refusal> {
        let organization = self.organization.as_ref().ok_or(Refusal::Missing)?;
        Ok(Reply::Restored {
            fresh: false,
            session_ended: self.dormant.is_some(),
            recovery: Some(pane::directories::Target {
                boot: self.boot,
                pane: organization.pane,
                original: organization.startup_directory().to_owned(),
            }),
            directory_unavailable: self.directory_unavailable,
            server: self.boot,
            workspace: organization.workspace,
            tab: organization.tab,
            pane: organization.pane,
        })
    }
    pub(super) fn retain_directory(&mut self) -> Result<(), Refusal> {
        let confirmed = self
            .pane
            .as_ref()
            .and_then(|pane| pane.runtime.metadata_snapshot())
            .and_then(|metadata| {
                metadata
                    .confirmed_directory
                    .as_ref()
                    .map(|path| std::path::PathBuf::from(path.as_ref()))
            });
        if let (Some(directory), Some(organization)) = (confirmed, &self.organization)
            && organization.confirmed_directory.as_ref() != Some(&directory)
        {
            // Only owning-shell markers supply this candidate. Validate on the owning host
            // before retaining it, and again when a later runtime starts.
            let Ok(directory) = local_filesystem().validate_directory(&directory) else {
                return Ok(());
            };
            let mut updated = organization.clone();
            updated.confirmed_directory = Some(directory.path().to_owned());
            self.retain(Some(updated))?;
        }
        Ok(())
    }
    pub(super) fn start(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<Lease, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        if self.organization.is_none() {
            self.retain(Some(
                Organization::first(self.directory.clone()).map_err(|_| Refusal::Failed)?,
            ))?;
        }
        let directory = local_filesystem()
            .validate_directory(
                self.organization
                    .as_ref()
                    .ok_or(Refusal::Missing)?
                    .startup_directory(),
            )
            .map_err(|_| {
                self.directory_unavailable = true;
                Refusal::DirectoryUnavailable
            })?;
        self.start_directory(device, connection, size, style, directory)
    }
    pub(super) fn start_directory(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        size: PaneSize,
        style: Option<PresentationStyle>,
        directory: ValidatedLocalDirectory,
    ) -> Result<Lease, Refusal> {
        self.directory_unavailable = false;
        self.failed_directory_session = None;
        let lease = Lease {
            server: self.boot,
            session: nonce().map_err(|_| Refusal::Failed)?,
            control: nonce().map_err(|_| Refusal::Failed)?,
            connection,
        };
        let runtime = self
            .factory
            .start_runtime(
                geometry(size),
                LocalTerminalLaunchPlan::new(directory).without_client_effects(),
                appearance(style, 1).map_err(|_| Refusal::Failed)?,
            )
            .map_err(|_| Refusal::Failed)?;
        let subscription = runtime.subscribe();
        runtime.route_client_effects();
        self.pane = Some(OwnedPane {
            device: Some(device),
            attachment_generation: None,
            control_epoch: 1,
            lease,
            connected: true,
            runtime,
            subscription,
            screen: None,
            graphics_transfer: None,
            sent: None,
            screen_encoder: screen_encoder::ScreenEncoder::default(),
            frame: 0,
            history: screen_encoder::HistoryTracker::default(),
            presentable: true,
            ended: false,
            failed: false,
            directory_failed: false,
            ended_restart: self.dormant.is_some(),
            appearance_generation: 1,
            focus_generation: 0,
            focus: FocusTarget::Unfocused,
            pending_paste: None,
            screen_generation: 0,
            size,
            effects: NativeEffects::default(),
            reading: reading::Reading::default(),
            accessibility: accessibility::Accessibility::default(),
        });
        self.dormant = None;
        Ok(lease)
    }
    pub(super) fn select(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        create: bool,
        size: PaneSize,
        resume: Option<PaneTarget>,
        style: Option<PresentationStyle>,
    ) -> Result<Reply, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        self.reconcile_selection()?;
        if let Some(dormant) = self.dormant
            && dormant.device.is_some_and(|owner| owner != device)
        {
            return Ok(self.takeover(
                PaneTarget {
                    server: self.boot,
                    session: self.organization.as_ref().ok_or(Refusal::Missing)?.pane,
                },
                dormant.device,
                device,
            ));
        }
        if let Some(pane) = &self.pane {
            let target = PaneTarget::from(pane.lease);
            if resume.is_some_and(|old| old != target) {
                return Err(Refusal::Stale);
            }
            if pane.device.is_some_and(|owner| owner != device)
                || (pane.device.is_none() && resume.is_some())
            {
                return Ok(self.takeover(target, pane.device, device));
            }
        }
        if let Some(pane) = &mut self.pane {
            return pane.claim(device, connection, size, style, pane.device.is_none());
        }
        if self.organization.is_some() {
            // Observing a restarted server never grants first-use intent.
            if !create || resume.is_some() {
                return self.restored();
            }
            return self
                .start(device, connection, size, style)
                .map(|lease| Reply::Attached { lease });
        }
        // A reconnect must never create a replacement Session after a lost launch reply.
        if !create || resume.is_some() {
            return Err(Refusal::Missing);
        }
        self.start(device, connection, size, style)
            .map(|lease| Reply::Attached { lease })
    }
    pub(super) fn repair_directory(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        target: pane::directories::Target,
        replacement: Option<std::path::PathBuf>,
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<Reply, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        let org = self.organization.as_ref().ok_or(Refusal::Missing)?;
        if target.boot != self.boot || target.pane != org.pane {
            return Err(Refusal::Stale);
        }
        // A concurrent first use already won. Repair can observe it, never replace it.
        if self.pane.is_some() {
            return self.select(device, connection, false, size, None, style);
        }
        if target.original != org.startup_directory() {
            return Err(Refusal::Stale);
        }
        let directory = local_filesystem()
            .validate_directory(replacement.as_deref().unwrap_or(&target.original))
            .map_err(|_| Refusal::DirectoryUnavailable)?;
        if replacement.is_some() {
            let mut updated = org.clone();
            updated.confirmed_directory = Some(directory.path().to_owned());
            self.retain(Some(updated))?;
        }
        self.start_directory(device, connection, size, style, directory)
            .map(|lease| Reply::Attached { lease })
    }

    pub(super) fn retire_exited(&mut self) -> Result<(), Refusal> {
        let Some(pane) = &mut self.pane else {
            return Ok(());
        };
        pane.drain();
        if pane.directory_failed {
            pane.runtime
                .shutdown_and_wait(Duration::from_secs(5))
                .map_err(|_| Refusal::CleanupUnresolved)?;
            self.failed_directory_session = Some(pane.lease.into());
            if pane.ended_restart {
                self.dormant = Some(DormantPane {
                    device: pane.device,
                    attachment_generation: pane.attachment_generation,
                    control_epoch: pane.control_epoch,
                });
            }
            self.pane = None;
            self.directory_unavailable = true;
            return Ok(());
        }
        if pane.ended {
            pane.runtime
                .shutdown_and_wait(Duration::from_secs(5))
                .map_err(|_| Refusal::CleanupUnresolved)?;
            // Ordinary exit closes the final Pane even without a subscribed client. Keep its
            // final screen until the controller polls, but never retain it across a restart.
            self.retain(None)?;
        }
        Ok(())
    }
    pub(super) fn end_session(&mut self, session: [u8; 32]) -> Result<(), Refusal> {
        let Some(pane) = self.pane.as_ref().filter(|p| p.lease.session == session) else {
            return Ok(());
        };
        self.dormant = Some(DormantPane {
            device: pane.device,
            attachment_generation: pane.attachment_generation,
            control_epoch: pane.control_epoch,
        });
        // Use ordinary shutdown to invalidate input/effects and wait for native cleanup, but
        // retain organization and its control grant rather than retiring an ordinary exit.
        self.shutdown().map_err(|_| Refusal::CleanupUnresolved)
    }
    pub(super) fn take_over_ended(
        &mut self,
        device: [u8; 32],
        attachment_generation: Option<u64>,
    ) -> Result<Reply, Refusal> {
        if self.pane.is_some() || self.dormant.is_none() {
            return Err(Refusal::Stale);
        }
        let reply = self.restored()?;
        let control_epoch = self
            .dormant
            .as_ref()
            .ok_or(Refusal::Stale)?
            .control_epoch
            .checked_add(1)
            .ok_or(Refusal::Capacity)?;
        self.dormant = Some(DormantPane {
            device: Some(device),
            attachment_generation,
            control_epoch,
        });
        Ok(reply)
    }
    pub(super) fn reconcile_selection(&mut self) -> Result<(), Refusal> {
        self.retire_exited()?;
        let Some(pane) = &self.pane else {
            return Ok(());
        };
        if pane.ended {
            self.pane = None;
        } else if pane.failed {
            return Err(Refusal::Failed);
        }
        Ok(())
    }
}
impl OwnedPane {
    pub(super) fn invalidate_input(&mut self) {
        self.presentable = false;
        self.runtime.set_presentable(false);
        self.screen_encoder = screen_encoder::ScreenEncoder::default();
        self.graphics_transfer = None;
        self.accessibility.disconnect();
        self.runtime.end_server_accessibility_demand();
        self.reading.invalidate();
        self.runtime.client_effect_focus(None);
        self.effects.clear();
        self.focus = FocusTarget::Unfocused;
        self.pending_paste = None;
        self.runtime.focus(false);
    }

    pub(super) fn focus(&mut self, generation: u64, target: FocusTarget) -> Result<Reply, Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if generation == 0 || generation < self.focus_generation {
            return Err(Refusal::Stale);
        }
        if generation == self.focus_generation && target == self.focus {
            return Ok(Reply::Unchanged);
        }
        if let FocusTarget::PasteConfirmation { id } = target {
            if self.pending_paste != Some(id)
                || generation != self.focus_generation
                || self.focus != FocusTarget::Terminal
            {
                return Err(Refusal::Stale);
            }
            self.runtime
                .focus_paste_confirmation(PasteConfirmationId::new(id));
        } else {
            if generation <= self.focus_generation {
                return Err(Refusal::Stale);
            }
            self.runtime.cancel_application_pointer();
            // Even a new Terminal generation cancels approvals from a preceding local focus.
            if let Some(id) = self.pending_paste.take() {
                self.runtime
                    .resolve_paste(PasteConfirmationId::new(id), PasteDecision::Cancel);
            }
            self.runtime.focus(target == FocusTarget::Terminal);
        }
        self.effects.clear();
        self.runtime
            .client_effect_focus(Some(target == FocusTarget::Terminal));
        self.focus_generation = generation;
        self.focus = target;
        Ok(Reply::Unchanged)
    }

    fn require_key_focus(&self, key: &KeyInput) -> Result<(), Refusal> {
        if self.focus == FocusTarget::Terminal
            || self.focus == FocusTarget::FocusedPane && key.is_input_method_commit()
        {
            Ok(())
        } else {
            Err(Refusal::Stale)
        }
    }

    pub(super) fn keys(&mut self, generation: u64, keys: Vec<Key>) -> Result<Reply, Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if generation == 0 || generation != self.focus_generation {
            return Err(Refusal::Stale);
        }
        // Collect only after validating the entire batch. No malformed or ineligible
        // later event may leave an accepted prefix, including a focused-Pane IME commit.
        let keys = keys
            .into_iter()
            .map(|key| {
                let key = decode_key(key).ok_or(Refusal::Unsupported)?;
                self.require_key_focus(&key)?;
                Ok(key)
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        for key in keys {
            self.runtime.key(key);
        }
        Ok(Reply::Unchanged)
    }

    pub(super) fn input(
        &mut self,
        generation: u64,
        input: InteractiveInput,
    ) -> Result<Reply, Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if generation == 0 || generation != self.focus_generation {
            return Err(Refusal::Stale);
        }
        match input {
            InteractiveInput::CopyOrForward { modifiers } => {
                self.require_terminal_focus()?;
                if modifiers > 1023 {
                    return Err(Refusal::Unsupported);
                }
                self.runtime
                    .forward_copy_shortcut(decode_modifiers(modifiers));
                Ok(Reply::Unchanged)
            }
            InteractiveInput::Key(key) => {
                let key = decode_key(key).ok_or(Refusal::Unsupported)?;
                self.require_key_focus(&key)?;
                self.runtime.key(key);
                Ok(Reply::Unchanged)
            }
            InteractiveInput::Pointer(input) => {
                self.require_terminal_focus()?;
                let generation = self.pointer_generation(
                    self.screen_generation,
                    input.x,
                    input.y,
                    input.modifiers,
                );
                let generation = match generation {
                    Ok(generation) => generation,
                    Err(reason) => {
                        if !matches!(input.phase, pane::PointerPhase::Press) {
                            self.runtime.cancel_application_pointer();
                        }
                        return Err(reason);
                    }
                };
                if !matches!(input.phase, pane::PointerPhase::Motion) && input.button.is_none() {
                    if matches!(input.phase, pane::PointerPhase::Release) {
                        self.runtime.cancel_application_pointer();
                    }
                    return Err(Refusal::Unsupported);
                }
                self.runtime.application_pointer(
                    PointerInput {
                        generation,
                        phase: match input.phase {
                            pane::PointerPhase::Press => super::super::PointerPhase::Press,
                            pane::PointerPhase::Motion => super::super::PointerPhase::Motion,
                            pane::PointerPhase::Release => super::super::PointerPhase::Release,
                        },
                        button: input.button.map(|button| match button {
                            pane::PointerButton::Left => super::super::PointerButton::Left,
                            pane::PointerButton::Middle => super::super::PointerButton::Middle,
                            pane::PointerButton::Right => super::super::PointerButton::Right,
                        }),
                        position: SurfacePosition {
                            x: input.x,
                            y: input.y,
                        },
                        modifiers: decode_modifiers(input.modifiers),
                        shift_selection: ShiftSelectionPolicy::from_selection_override(
                            input.shift_selection,
                        ),
                    },
                    input.screen_generation,
                    self.screen_generation,
                );
                Ok(Reply::Unchanged)
            }
            InteractiveInput::Wheel(input) => {
                self.require_terminal_focus()?;
                if input.screen_generation != self.screen_generation {
                    return Ok(Reply::Unchanged);
                }
                let generation = self.pointer_generation(
                    input.screen_generation,
                    input.x,
                    input.y,
                    input.modifiers,
                )?;
                if input.horizontal_steps.unsigned_abs() > 100
                    || input.vertical_steps.unsigned_abs() > 100
                {
                    return Err(Refusal::Unsupported);
                }
                self.runtime.application_wheel(WheelInput {
                    generation,
                    horizontal_steps: input.horizontal_steps,
                    vertical_steps: input.vertical_steps,
                    phase: match input.phase {
                        pane::WheelPhase::GestureStarted => {
                            super::super::WheelPhase::GestureStarted
                        }
                        pane::WheelPhase::GestureChanged => {
                            super::super::WheelPhase::GestureChanged
                        }
                        pane::WheelPhase::GestureEnded => super::super::WheelPhase::GestureEnded,
                        pane::WheelPhase::GestureCancelled => {
                            super::super::WheelPhase::GestureCancelled
                        }
                        pane::WheelPhase::MomentumStarted => {
                            super::super::WheelPhase::MomentumStarted
                        }
                        pane::WheelPhase::MomentumChanged => {
                            super::super::WheelPhase::MomentumChanged
                        }
                        pane::WheelPhase::MomentumEnded => super::super::WheelPhase::MomentumEnded,
                        pane::WheelPhase::MomentumCancelled => {
                            super::super::WheelPhase::MomentumCancelled
                        }
                    },
                    position: SurfacePosition {
                        x: input.x,
                        y: input.y,
                    },
                    modifiers: decode_modifiers(input.modifiers),
                    shift_selection: ShiftSelectionPolicy::from_selection_override(
                        input.shift_selection,
                    ),
                });
                Ok(Reply::Unchanged)
            }
            InteractiveInput::Paste { text } => {
                self.require_terminal_focus()?;
                let result = self.runtime.request_paste(PastePayload::from(text));
                let result = runtime_reply(result)?;
                let outcome = match result {
                    PasteRequestOutcome::Unconfirmed => return Err(Refusal::Failed),
                    PasteRequestOutcome::Written => PasteOutcome::Written,
                    PasteRequestOutcome::ConfirmationRequired(confirmation) => {
                        let id = confirmation.id.value();
                        self.pending_paste = Some(id);
                        PasteOutcome::ConfirmationRequired(pane::PasteConfirmation {
                            id,
                            byte_len: confirmation.byte_len as u32,
                            line_count: confirmation.line_count as u32,
                            multiline: confirmation.risk.multiline,
                            control_bytes: confirmation.risk.control_bytes,
                            closing_fence: confirmation.risk.closing_fence,
                        })
                    }
                    PasteRequestOutcome::Rejected(reason) => PasteOutcome::Rejected(match reason {
                        super::super::PasteRejection::Empty => pane::PasteRejection::Empty,
                        super::super::PasteRejection::TooLarge { limit } => {
                            pane::PasteRejection::TooLarge {
                                limit: limit as u32,
                            }
                        }
                        super::super::PasteRejection::ConfirmationPending => {
                            pane::PasteRejection::ConfirmationPending
                        }
                        super::super::PasteRejection::TerminalUnfocused => {
                            pane::PasteRejection::TerminalUnfocused
                        }
                    }),
                };
                Ok(Reply::Paste(outcome))
            }
            InteractiveInput::ResolvePaste { id, confirm } => {
                if self.pending_paste != Some(id)
                    || !(self.focus == FocusTarget::Terminal
                        || self.focus == (FocusTarget::PasteConfirmation { id }))
                {
                    return Ok(Reply::Paste(PasteOutcome::Stale));
                }
                self.pending_paste = None;
                let result = runtime_reply(self.runtime.resolve_paste(
                    PasteConfirmationId::new(id),
                    if confirm {
                        PasteDecision::Confirm
                    } else {
                        PasteDecision::Cancel
                    },
                ))?;
                Ok(Reply::Paste(match result {
                    PasteResolution::Unconfirmed => return Err(Refusal::Failed),
                    PasteResolution::Written => PasteOutcome::Written,
                    PasteResolution::Cancelled => PasteOutcome::Cancelled,
                    PasteResolution::Stale => PasteOutcome::Stale,
                }))
            }
        }
    }
    pub(super) fn require_terminal_focus(&self) -> Result<(), Refusal> {
        if self.focus == FocusTarget::Terminal {
            Ok(())
        } else {
            Err(Refusal::Stale)
        }
    }
    pub(super) fn pointer_generation(
        &self,
        generation: u64,
        x: f32,
        y: f32,
        modifiers: u16,
    ) -> Result<PresentationGeneration, Refusal> {
        if modifiers > 1023
            || !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f32::from(self.size.columns) * f32::from(self.size.cell_width)
            || y >= f32::from(self.size.rows) * f32::from(self.size.cell_height)
        {
            return Err(Refusal::Unsupported);
        }
        if generation == 0 || generation != self.screen_generation {
            return Err(Refusal::Stale);
        }
        self.screen
            .as_ref()
            .map(|screen| screen.generation)
            .ok_or(Refusal::Stale)
    }
    pub(super) fn claim(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        size: PaneSize,
        style: Option<PresentationStyle>,
        transfer: bool,
    ) -> Result<Reply, Refusal> {
        size.validate().map_err(|_| Refusal::Unsupported)?;
        let control = if transfer {
            nonce().map_err(|_| Refusal::Failed)?
        } else {
            self.lease.control
        };
        let control_epoch = if transfer {
            self.control_epoch.checked_add(1).ok_or(Refusal::Capacity)?
        } else {
            self.control_epoch
        };
        let appearance = if style.is_some() {
            Some(appearance(style, self.appearance_generation + 1).map_err(|_| Refusal::Failed)?)
        } else {
            None
        };
        if transfer {
            self.attachment_generation = None;
            self.accessibility.reset();
            self.reading.reset(&self.runtime);
            self.screen = None;
            self.reading.invalidate();
        }
        if transfer || connection != self.lease.connection || !self.connected {
            self.invalidate_input();
            self.focus_generation = 0;
        }
        self.device = Some(device);
        self.control_epoch = control_epoch;
        self.lease.control = control;
        self.lease.connection = connection;
        self.connected = true;
        self.presentable = true;
        self.runtime.set_presentable(true);
        self.sent = None;
        self.screen_encoder = screen_encoder::ScreenEncoder::default();
        self.runtime.resize(geometry(size));
        self.size = size;
        if let Some(appearance) = appearance {
            self.appearance_generation += 1;
            self.runtime.update_appearance(appearance);
        }
        Ok(Reply::Attached { lease: self.lease })
    }
    pub(super) fn drain(&mut self) {
        if self.reading.reset_pending() {
            return;
        }
        self.effects.drain_clipboard(&self.subscription.clipboard);
        for request in self.subscription.take_client_effects() {
            self.effects.push(request);
        }
        while let Ok(event) = self.subscription.events.try_recv() {
            match event {
                TerminalSessionEvent::Screen(screen) => {
                    if self
                        .graphics_transfer
                        .as_ref()
                        .is_some_and(|(active, image)| {
                            *active != screen.active_screen
                                || !screen.graphics.images.iter().any(|current| {
                                    current.key.image_id == image.key.image_id
                                        && current.width == image.width
                                        && current.height == image.height
                                })
                        })
                    {
                        self.graphics_transfer = None;
                    }
                    self.advance_screen_generation(&screen);
                    self.reading.observe(self.screen_generation, &screen);
                    self.screen = Some(screen);
                }
                TerminalSessionEvent::ClientEffect(_) => {}
                TerminalSessionEvent::Exited(_) => {
                    self.invalidate_input();
                    self.ended = true;
                }
                TerminalSessionEvent::Failed(ref failure) => {
                    self.directory_failed = matches!(
                        failure,
                        TerminalSessionFailure::Startup {
                            stage: TerminalSessionStartupStage::Directory,
                            ..
                        }
                    );
                    self.invalidate_input();
                    self.failed = true;
                }
                _ => {}
            }
        }
        if let Some(screen) = &self.screen {
            self.accessibility.bind_reset_screen(screen.generation);
        }
        while let Ok(model) = self.subscription.accessibility.try_recv() {
            self.accessibility.update(model);
        }
        if let Some(screen) = &self.screen {
            self.reading.update(&self.runtime, screen);
        }
    }
}
fn runtime_reply<T>(reply: async_channel::Receiver<Result<T, String>>) -> Result<T, Refusal> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match reply.try_recv() {
            Ok(result) => return result.map_err(|_| Refusal::Failed),
            Err(async_channel::TryRecvError::Closed) => return Err(Refusal::Failed),
            Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(Refusal::Failed),
        }
    }
}
pub(crate) fn geometry(size: PaneSize) -> TerminalGeometry {
    TerminalGeometry::from_viewport(
        LogicalSize::new(
            f32::from(size.columns) * f32::from(size.cell_width),
            f32::from(size.rows) * f32::from(size.cell_height),
        ),
        LogicalCellSize::new(f32::from(size.cell_width), f32::from(size.cell_height)),
        BackingScale::ONE,
        CellGridSize::new(2, 2),
    )
}
pub(super) fn decode_modifiers(bits: u16) -> InputModifiers {
    InputModifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        control: bits & 4 != 0,
        platform: bits & 8 != 0,
        caps_lock: bits & 16 != 0,
        num_lock: bits & 32 != 0,
        shift_right: bits & 64 != 0,
        alt_right: bits & 128 != 0,
        control_right: bits & 256 != 0,
        platform_right: bits & 512 != 0,
    }
}
pub(crate) fn decode_key(key: Key) -> Option<KeyInput> {
    if key.logical.len() > 128
        || key
            .text
            .as_ref()
            .is_some_and(|t| t.len() > pane::MAX_KEY_TEXT_BYTES)
        || key.modifiers > 1023
        || key.consumed > 1023
    {
        return None;
    }
    let input = KeyInput {
        action: match key.action {
            0 => KeyAction::Press,
            1 => KeyAction::Repeat,
            2 => KeyAction::Release,
            _ => return None,
        },
        physical_key: PhysicalKey::try_from(i32::try_from(key.physical).ok()?).ok()?,
        native_key_code: None,
        logical_key: key.logical,
        text: key.text,
        unshifted_codepoint: key.unshifted,
        modifiers: decode_modifiers(key.modifiers),
        consumed_modifiers: decode_modifiers(key.consumed),
        option_as_alt: if key.option_as_alt {
            OptionAsAltPolicy::Both
        } else {
            OptionAsAltPolicy::None
        },
    };
    input.validate().ok()?;
    // Text input made of control characters reaches the PTY as the keys that type it. Text with
    // a control character no key produces is refused here, before the Terminal Session sees it.
    if input.is_text_input() || input.is_input_method_commit() {
        input.typed_keys().ok()?;
    }
    Some(input)
}
