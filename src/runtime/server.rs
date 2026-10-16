//! The first retained Development Workspace and its independently owned live Terminal Session.
use super::*;
use spaceterm_server::organization::{Organization, OrganizationStore};
use spaceterm_server::{
    ServerError,
    pane::{self, *},
};
use std::{sync::Arc, time::Duration};

#[cfg(test)]
#[path = "server/effect_tests.rs"]
mod effect_tests;
mod effects;
use self::effects::NativeEffects;
mod reading;
#[cfg(test)]
mod reading_tests;
mod repositories;
#[cfg(test)]
#[path = "server/repository_tests.rs"]
mod repository_tests;
mod shared;
pub use shared::SharedWorkspaceHost;

pub struct DevelopmentPaneHost {
    boot: [u8; 32],
    pane: Option<OwnedPane>,
    factory: NativeTerminalSessionFactory,
    directory: std::path::PathBuf,
    organization: Option<Organization>,
    store: Option<OrganizationStore>,
    uncertain: bool,
}
struct OwnedPane {
    device: Option<[u8; 32]>,
    attachment_generation: Option<u64>,
    lease: Lease,
    connected: bool,
    runtime: TerminalRuntime,
    subscription: TerminalSubscription,
    screen: Option<Arc<ScreenSnapshot>>,
    sent: Option<PresentationGeneration>,
    ended: bool,
    failed: bool,
    appearance_generation: u64,
    focus_generation: u64,
    focus: FocusTarget,
    pending_paste: Option<u64>,
    screen_generation: u64,
    size: PaneSize,
    effects: NativeEffects,
    reading: reading::Reading,
}
fn nonce() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ServerError::StartupFailed)?;
    Ok(bytes)
}
impl DevelopmentPaneHost {
    pub fn new(
        factory: NativeTerminalSessionFactory,
        directory: ValidatedLocalDirectory,
    ) -> Result<Self, ServerError> {
        Ok(Self {
            boot: nonce()?,
            pane: None,
            factory,
            directory: directory.path().to_owned(),
            organization: None,
            store: None,
            uncertain: false,
        })
    }
    /// Native composition for the explicitly launched local Development host.
    pub fn local() -> Result<Self, ServerError> {
        let filesystem = local_filesystem();
        let directory = crate::platform::app_directories::AppDirectoryEnvironment::capture()
            .home
            .map(std::path::PathBuf::from)
            .ok_or(ServerError::StartupFailed)?;
        let shell = crate::platform::launch_host::user_shell();
        let factory = NativeTerminalSessionFactory::new(
            native_pty_factory(),
            ShellLaunchPlanner::new(
                shell,
                crate::platform::launch_host::resource_root(),
                ShellIntegrationMode::Automatic,
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
        );
        Ok(Self {
            boot: nonce()?,
            pane: None,
            factory,
            directory,
            organization: None,
            store: None,
            uncertain: false,
        })
    }
    fn retain(&mut self, organization: Option<Organization>) -> Result<(), Refusal> {
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
    fn recover(&mut self) -> Result<(), Refusal> {
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
            server: self.boot,
            workspace: organization.workspace,
            tab: organization.tab,
            pane: organization.pane,
        })
    }
    fn retain_directory(&mut self) -> Result<(), Refusal> {
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
    fn start(
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
            .map_err(|_| Refusal::DirectoryUnavailable)?;
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
                TerminalLaunchPlan::Local(
                    LocalTerminalLaunchPlan::new(directory).without_client_effects(),
                ),
                appearance(style, 1).map_err(|_| Refusal::Failed)?,
            )
            .map_err(|_| Refusal::Failed)?;
        let subscription = runtime.subscribe();
        runtime.route_client_effects();
        self.pane = Some(OwnedPane {
            device: Some(device),
            attachment_generation: None,
            lease,
            connected: true,
            runtime,
            subscription,
            screen: None,
            sent: None,
            ended: false,
            failed: false,
            appearance_generation: 1,
            focus_generation: 0,
            focus: FocusTarget::Unfocused,
            pending_paste: None,
            screen_generation: 0,
            size,
            effects: NativeEffects::default(),
            reading: reading::Reading::default(),
        });
        Ok(lease)
    }
    fn select(
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
        if let Some(pane) = &mut self.pane {
            let target = PaneTarget::from(pane.lease);
            if resume.is_some_and(|old| old != target) {
                return Err(Refusal::Stale);
            }
            if pane.device.is_some_and(|owner| owner != device)
                || (pane.device.is_none() && resume.is_some())
            {
                return Ok(Reply::Takeover { target });
            }
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
    fn handle(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Result<Reply, Refusal> {
        if matches!(request, Request::Capabilities) {
            return Ok(Reply::Capabilities {
                development_pane: true,
                pane_takeover: true,
                interactive_input: true,
                native_effects: true,
                reading: true,
            });
        }
        self.recover()?;
        self.retain_directory()?;
        match request {
            Request::StartRestored {
                server,
                pane,
                size,
                style,
            } => {
                if server != self.boot
                    || self
                        .organization
                        .as_ref()
                        .is_none_or(|org| org.pane != pane)
                {
                    return Err(Refusal::Stale);
                }
                if self.pane.is_some() {
                    return self.select(device, connection, false, size, None, style);
                }
                return self
                    .start(device, connection, size, style)
                    .map(|lease| Reply::Attached { lease });
            }
            Request::Attach {
                create,
                size,
                resume,
                style,
            } => {
                // Legacy clients cannot render a Takeover Screen or request takeover.
                if let (Some(pane), Some(old)) = (&self.pane, resume)
                    && (old.server, old.session, old.control)
                        != (pane.lease.server, pane.lease.session, pane.lease.control)
                {
                    return Err(Refusal::Stale);
                }
                let reply = self.select(
                    device,
                    connection,
                    create,
                    size,
                    resume.map(Into::into),
                    style,
                )?;
                return if matches!(reply, Reply::Takeover { .. }) {
                    Err(Refusal::Controlled)
                } else {
                    Ok(reply)
                };
            }
            Request::Select {
                create,
                size,
                resume,
                style,
            } => {
                return self.select(device, connection, create, size, resume, style);
            }
            Request::TakeOver {
                target,
                size,
                style,
            } => {
                self.reconcile_selection()?;
                let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
                if PaneTarget::from(pane.lease) != target {
                    return Err(Refusal::Stale);
                }
                return pane.claim(device, connection, size, style, true);
            }
            _ => {}
        }
        if matches!(request, Request::Shutdown) {
            self.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
            return Ok(Reply::Closed);
        }
        let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
        let lease = match &request {
            Request::Read { lease, .. }
            | Request::ReadingStatus { lease }
            | Request::SelectionChunk { lease, .. } => lease,
            Request::Poll { lease }
            | Request::NativeEffects { lease, .. }
            | Request::CompleteClipboard { lease, .. }
            | Request::Focus { lease, .. }
            | Request::Input { lease, .. }
            | Request::Key { lease, .. }
            | Request::Resize { lease, .. }
            | Request::Close { lease }
            | Request::Style { lease, .. }
            | Request::Release { lease } => lease,
            _ => return Err(Refusal::Unsupported),
        };
        if Some(device) != pane.device
            || connection != pane.lease.connection
            || *lease != pane.lease
            || !pane.connected
        {
            return Err(Refusal::Stale);
        }
        pane.drain();
        match request {
            Request::Read { operation, .. } => pane.read(operation),
            Request::ReadingStatus { .. } => pane.reading_status(),
            Request::SelectionChunk {
                generation, offset, ..
            } => {
                if generation != pane.screen_generation {
                    return Err(Refusal::Stale);
                }
                pane.reading.chunk(generation, offset)
            }
            Request::NativeEffects { generation, .. } => {
                if generation == 0 || generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                Ok(Reply::NativeEffects {
                    request: pane.effects.next(generation),
                    hidden_input: pane.subscription.hidden_input(),
                })
            }
            Request::CompleteClipboard {
                generation,
                id,
                completion,
                ..
            } => {
                if generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                pane.effects.complete(id, completion);
                Ok(Reply::Unchanged)
            }
            Request::Poll { .. } => {
                if let Some(screen) = &pane.screen
                    && pane.sent != Some(screen.generation)
                {
                    pane.sent = Some(screen.generation);
                    let mut screen = encode_screen(screen);
                    screen.generation = pane.screen_generation;
                    return Ok(Reply::Screen(Box::new(screen)));
                }
                if pane.failed {
                    return Err(Refusal::Failed);
                }
                if pane.ended {
                    self.retire_exited()?;
                    self.pane = None;
                    Ok(Reply::Ended)
                } else {
                    Ok(Reply::Unchanged)
                }
            }
            Request::Key { key, .. } => {
                if pane.focus_generation != 0 {
                    return Err(Refusal::Unsupported);
                }
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime
                    .key(decode_key(key).ok_or(Refusal::Unsupported)?);
                Ok(Reply::Unchanged)
            }
            Request::Focus {
                generation, target, ..
            } => pane.focus(generation, target),
            Request::Input {
                generation, input, ..
            } => {
                let reply = pane.input(generation, input);
                if matches!(reply, Err(Refusal::Failed)) {
                    pane.invalidate_input();
                }
                reply
            }
            Request::Resize { size, .. } => {
                size.validate().map_err(|_| Refusal::Unsupported)?;
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime.resize(geometry(size));
                pane.size = size;
                Ok(Reply::Unchanged)
            }
            Request::Style { style, .. } => {
                pane.appearance_generation += 1;
                pane.runtime.update_appearance(
                    appearance(Some(style), pane.appearance_generation)
                        .map_err(|_| Refusal::Failed)?,
                );
                Ok(Reply::Unchanged)
            }
            Request::Release { .. } => {
                pane.invalidate_input();
                pane.lease.control = nonce().map_err(|_| Refusal::Failed)?;
                pane.device = None;
                pane.connected = false;
                Ok(Reply::Unchanged)
            }
            Request::Close { .. } => {
                pane.runtime
                    .shutdown_and_wait(Duration::from_secs(5))
                    .map_err(|_| Refusal::CleanupUnresolved)?;
                self.retain(None)?;
                self.pane = None;
                Ok(Reply::Closed)
            }
            _ => Err(Refusal::Unsupported),
        }
    }

    fn retire_exited(&mut self) -> Result<(), Refusal> {
        let Some(pane) = &mut self.pane else {
            return Ok(());
        };
        pane.drain();
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
    fn reconcile_selection(&mut self) -> Result<(), Refusal> {
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
impl PaneService for DevelopmentPaneHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.organization = store.read()?;
        self.store = Some(store);
        Ok(())
    }
    fn tick(&mut self) {
        if self.recover().is_ok() && self.retain_directory().is_ok() {
            let _ = self.retire_exited();
        }
    }
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn disconnected(&mut self, connection: [u8; 32]) {
        if let Some(pane) = &mut self.pane
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        // Persist final confirmed metadata before ending live work. Refuse uncertain shutdown.
        self.recover()
            .map_err(|_| ServerError::StorageUnavailable)?;
        self.retain_directory()
            .map_err(|_| ServerError::StorageUnavailable)?;
        if let Some(pane) = &mut self.pane {
            pane.invalidate_input();
            pane.connected = false;
            pane.runtime
                .shutdown_and_wait(Duration::from_secs(5))
                .map_err(|_| ServerError::CleanupUnresolved)?;
        }
        self.pane = None;
        Ok(())
    }
}
impl OwnedPane {
    fn invalidate_input(&mut self) {
        self.reading.invalidate();
        self.runtime.client_effect_focus(None);
        self.effects.clear();
        self.focus = FocusTarget::Unfocused;
        self.pending_paste = None;
        self.runtime.focus(false);
    }

    fn focus(&mut self, generation: u64, target: FocusTarget) -> Result<Reply, Refusal> {
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

    fn input(&mut self, generation: u64, input: InteractiveInput) -> Result<Reply, Refusal> {
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
                if self.focus != FocusTarget::Terminal
                    && !(self.focus == FocusTarget::FocusedPane && key.is_input_method_commit())
                {
                    return Err(Refusal::Stale);
                }
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
                            pane::PointerPhase::Press => super::PointerPhase::Press,
                            pane::PointerPhase::Motion => super::PointerPhase::Motion,
                            pane::PointerPhase::Release => super::PointerPhase::Release,
                        },
                        button: input.button.map(|button| match button {
                            pane::PointerButton::Left => super::PointerButton::Left,
                            pane::PointerButton::Middle => super::PointerButton::Middle,
                            pane::PointerButton::Right => super::PointerButton::Right,
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
                        pane::WheelPhase::GestureStarted => super::WheelPhase::GestureStarted,
                        pane::WheelPhase::GestureChanged => super::WheelPhase::GestureChanged,
                        pane::WheelPhase::GestureEnded => super::WheelPhase::GestureEnded,
                        pane::WheelPhase::GestureCancelled => super::WheelPhase::GestureCancelled,
                        pane::WheelPhase::MomentumStarted => super::WheelPhase::MomentumStarted,
                        pane::WheelPhase::MomentumChanged => super::WheelPhase::MomentumChanged,
                        pane::WheelPhase::MomentumEnded => super::WheelPhase::MomentumEnded,
                        pane::WheelPhase::MomentumCancelled => super::WheelPhase::MomentumCancelled,
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
                        super::PasteRejection::Empty => pane::PasteRejection::Empty,
                        super::PasteRejection::TooLarge { limit } => {
                            pane::PasteRejection::TooLarge {
                                limit: limit as u32,
                            }
                        }
                        super::PasteRejection::ConfirmationPending => {
                            pane::PasteRejection::ConfirmationPending
                        }
                        super::PasteRejection::TerminalUnfocused => {
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
    fn require_terminal_focus(&self) -> Result<(), Refusal> {
        if self.focus == FocusTarget::Terminal {
            Ok(())
        } else {
            Err(Refusal::Stale)
        }
    }
    fn pointer_generation(
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
    fn claim(
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
        let appearance = if style.is_some() {
            Some(appearance(style, self.appearance_generation + 1).map_err(|_| Refusal::Failed)?)
        } else {
            None
        };
        if transfer {
            self.attachment_generation = None;
            self.reading.reset(&self.runtime);
            self.screen = None;
            self.reading.invalidate();
        }
        if transfer || connection != self.lease.connection || !self.connected {
            self.invalidate_input();
            self.focus_generation = 0;
        }
        self.device = Some(device);
        self.lease.control = control;
        self.lease.connection = connection;
        self.connected = true;
        self.sent = None;
        self.runtime.resize(geometry(size));
        self.size = size;
        if let Some(appearance) = appearance {
            self.appearance_generation += 1;
            self.runtime.update_appearance(appearance);
        }
        Ok(Reply::Attached { lease: self.lease })
    }
    fn drain(&mut self) {
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
                        .screen
                        .as_ref()
                        .is_none_or(|old| old.generation != screen.generation)
                    {
                        self.screen_generation = self.screen_generation.saturating_add(1);
                    }
                    self.screen = Some(screen);
                }
                TerminalSessionEvent::ClientEffect(_) => {}
                TerminalSessionEvent::Exited(_) => {
                    self.invalidate_input();
                    self.ended = true;
                }
                TerminalSessionEvent::Failed(_) => {
                    self.invalidate_input();
                    self.failed = true;
                }
                _ => {}
            }
        }
        if let Some(screen) = &self.screen {
            self.reading
                .update(&self.runtime, screen, self.screen_generation);
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
fn appearance(
    style: Option<PresentationStyle>,
    generation: u64,
) -> Result<TerminalAppearanceUpdate, TerminalFailure> {
    let mut update = TerminalAppearanceUpdate::builtin()?;
    update.generation = AppearanceGeneration::new(generation);
    if let Some(style) = style {
        let resolved = Arc::make_mut(&mut update.appearance);
        resolved.appearance = if style.light {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        resolved.colors.foreground = from_rgb(style.foreground);
        resolved.colors.background = from_rgb(style.background);
        resolved.colors.cursor = from_rgb(style.cursor);
        resolved.colors.cursor_text = style.cursor_text.map(from_rgb);
        resolved.colors.normal = std::array::from_fn(|i| from_rgb(style.palette[i]));
        resolved.colors.bright = std::array::from_fn(|i| from_rgb(style.palette[i + 8]));
        resolved.bold_as_bright = style.bold_as_bright;
    }
    Ok(update)
}
fn from_rgb(c: [u8; 3]) -> super::Color {
    super::Color::from_rgb_components(c[0], c[1], c[2])
}
fn rgb(color: super::Color) -> [u8; 3] {
    [color.r, color.g, color.b]
}
fn encode_color(color: TerminalColor) -> pane::Color {
    match color {
        TerminalColor::Default => pane::Color::Default,
        TerminalColor::Palette(i) => pane::Color::Palette(i),
        TerminalColor::Rgb(c) => pane::Color::Rgb(rgb(c)),
    }
}
pub(crate) fn encode_screen(screen: &ScreenSnapshot) -> Screen {
    let mut hyperlinks = Vec::new();
    let mut link_indices = std::collections::HashMap::new();
    let mut link_bytes = 0;
    let mut encoded = Screen {
        reading: Some(pane::reading::Snapshot {
            total_rows: screen.scrollbar.total_rows,
            offset_rows: screen.scrollbar.offset_rows,
            visible_rows: screen.scrollbar.visible_rows,
            selection_present: screen.selection_present,
            find: screen.find.as_ref().map(|find| pane::reading::Find {
                generation: find.generation.value(),
                total: find.total_matches,
                current: find.current_match,
                spans: find
                    .visible_spans
                    .iter()
                    .map(|s| (s.row, s.start_column, s.end_column, s.current))
                    .collect(),
            }),
        }),
        hyperlinks: Vec::new(),
        generation: 0,
        mouse_tracking: screen.mouse_tracking,
        alternate_scroll: screen.alternate_scroll,
        columns: screen.size.cols,
        rows: screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| Cell {
                        hyperlink: c.hyperlink.as_ref().and_then(|link| {
                            let key = (link.kind == HyperlinkKind::LocalPath, link.value.as_str());
                            if let Some(index) = link_indices.get(&key) {
                                return Some(*index);
                            }
                            // Link metadata must never make an otherwise renderable screen exceed
                            // the transport frame. Excess targets lose their action, not their text.
                            if hyperlinks.len() >= pane::effects::MAX_SCREEN_HYPERLINKS
                                || link.value.len() > crate::terminal::hyperlink::MAX_LINK_BYTES
                                || link_bytes + link.value.len()
                                    > pane::effects::MAX_SCREEN_HYPERLINK_BYTES
                            {
                                return None;
                            }
                            let index = hyperlinks.len() as u16;
                            link_bytes += link.value.len();
                            hyperlinks.push(match link.kind {
                                HyperlinkKind::Url => {
                                    pane::effects::Hyperlink::Url(link.value.clone())
                                }
                                HyperlinkKind::LocalPath => {
                                    pane::effects::Hyperlink::File(link.value.clone())
                                }
                            });
                            link_indices.insert(key, index);
                            Some(index)
                        }),
                        text: c.text.chars().take(64).collect(),
                        foreground: encode_color(c.foreground_source),
                        background: encode_color(c.background_source),
                        underline_color: encode_color(c.underline_source),
                        flags: u16::from(c.inverse)
                            | (u16::from(c.bold) << 1)
                            | (u16::from(c.faint) << 2)
                            | (u16::from(c.italic) << 3)
                            | (u16::from(c.blinking) << 4)
                            | (u16::from(c.invisible) << 5)
                            | (u16::from(c.strikethrough) << 6)
                            | (u16::from(c.overline) << 7)
                            | (u16::from(c.spacer_tail) << 8)
                            | (u16::from(c.selected) << 9),
                        underline: match c.underline {
                            TerminalUnderlineSnapshot::None => 0,
                            TerminalUnderlineSnapshot::Single => 1,
                            TerminalUnderlineSnapshot::Double => 2,
                            TerminalUnderlineSnapshot::Curly => 3,
                            TerminalUnderlineSnapshot::Dotted => 4,
                            TerminalUnderlineSnapshot::Dashed => 5,
                        },
                    })
                    .collect()
            })
            .collect(),
        wrapped: screen.row_soft_wrapped.to_vec(),
        foreground: rgb(screen.colors.foreground),
        background: rgb(screen.colors.background),
        palette: screen.colors.palette.iter().copied().map(rgb).collect(),
        reversed: screen.colors.reversed,
        foreground_overridden: screen.colors.foreground_source
            == TerminalDefaultColorSource::ProgramOverride,
        background_overridden: screen.colors.background_source
            == TerminalDefaultColorSource::ProgramOverride,
        cursor_overridden: screen.colors.cursor_source
            == TerminalDefaultColorSource::ProgramOverride,
        palette_overrides: screen.colors.palette_overrides.to_vec(),
        cursor: screen
            .cursor
            .position
            .map(|p| (p.column, p.row, p.width_cells)),
        cursor_visible: screen.cursor.visible,
        cursor_blinking: screen.cursor.blinking,
        cursor_shape: match screen.cursor.shape {
            CursorShapeSnapshot::Block => 0,
            CursorShapeSnapshot::Bar => 1,
            CursorShapeSnapshot::Underline => 2,
            CursorShapeSnapshot::BlockHollow => 3,
        },
        cursor_color: rgb(screen.cursor.color),
        cursor_text: rgb(screen.cursor.text_color),
        alternate: screen.active_screen == ActiveScreenSnapshot::Alternate,
    };
    encoded.hyperlinks = hyperlinks;
    encoded
}
fn decode_modifiers(bits: u16) -> InputModifiers {
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
        || key.text.as_ref().is_some_and(|t| t.len() > 4096)
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
    Some(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn server_screen_with_a_long_hyperlink_remains_within_the_transport_limit() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(120, 40),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry).unwrap();
        let url = format!("https://example.test/{}", "a".repeat(3980));
        emulator.feed("x".repeat(120 * 39).as_bytes());
        let mut screen = (*emulator.snapshot().unwrap().unwrap()).clone();
        let link = Arc::new(crate::terminal::HyperlinkTarget::url(&url).unwrap());
        for row in Arc::make_mut(&mut screen.rows).iter_mut().take(39) {
            for cell in Arc::make_mut(row) {
                cell.hyperlink = Some(Arc::clone(&link));
            }
        }
        let encoded = encode_screen(&screen);
        assert_eq!(encoded.hyperlinks.len(), 1);
        let reply = Reply::Screen(Box::new(encoded));
        let bytes = serde_json::to_vec(&reply).unwrap();
        assert!(
            bytes.len() < pane::MAX_TERMINAL_FRAME,
            "screen frame has {} bytes",
            bytes.len()
        );
    }
    #[test]
    fn server_screen_with_many_distinct_links_retains_text_with_bounded_metadata() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(120, 40),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        );
        let mut emulator = crate::terminal::emulator::TerminalEmulator::new(geometry).unwrap();
        emulator.feed(b"fixture");
        let mut screen = (*emulator.snapshot().unwrap().unwrap()).clone();
        let mut index = 0;
        for row in Arc::make_mut(&mut screen.rows) {
            for cell in Arc::make_mut(row) {
                let url = format!("https://example.test/{index}/{}", "a".repeat(3970));
                cell.hyperlink = Some(Arc::new(
                    crate::terminal::HyperlinkTarget::url(&url).unwrap(),
                ));
                index += 1;
            }
        }
        let encoded = encode_screen(&screen);
        assert!(encoded.rows[0][0].hyperlink.is_some());
        assert!(
            encoded
                .rows
                .last()
                .unwrap()
                .last()
                .unwrap()
                .hyperlink
                .is_none()
        );
        assert_eq!(encoded.rows[0][0].text, "f");
        assert!(
            serde_json::to_vec(&Reply::Screen(Box::new(encoded)))
                .unwrap()
                .len()
                < pane::MAX_TERMINAL_FRAME
        );
    }

    pub(super) fn native_command(
        host: &mut impl PaneService,
        lease: Lease,
        generation: u64,
        text: &str,
    ) {
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

    #[test]
    fn native_clipboard_requests_reach_only_the_current_controller() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        native_command(&mut host, lease, 1, "printf '\\033]52;c;aGVsbG8=\\007'");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let reply = host.request(
                [1; 32],
                lease.connection,
                Request::NativeEffects {
                    lease,
                    generation: 1,
                },
            );
            match reply {
                Reply::NativeEffects {
                    request: Some(request),
                    ..
                } => {
                    assert!(
                        matches!(request.effect, pane::effects::Effect::ClipboardWrite { text, .. } if text == "hello")
                    );
                    break;
                }
                Reply::NativeEffects { request: None, .. } => {}
                other => panic!("native effects unavailable: {other:?}"),
            }
            assert!(
                Instant::now() < deadline,
                "clipboard request did not arrive"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        host.shutdown().unwrap();
    }

    #[test]
    fn interactive_input_requires_current_focus_and_legacy_keys_cannot_bypass_it() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        let key = Key {
            physical: PhysicalKey::Enter as u32,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        };
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Focus {
                    lease,
                    generation: 1,
                    target: FocusTarget::Unfocused
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Key {
                    lease,
                    key: key.clone()
                }
            ),
            Reply::Refused(Refusal::Unsupported)
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Input {
                    lease,
                    generation: 1,
                    input: InteractiveInput::Key(key.clone())
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Focus {
                    lease,
                    generation: 2,
                    target: FocusTarget::Terminal
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Input {
                    lease,
                    generation: 2,
                    input: InteractiveInput::Key(key)
                }
            ),
            Reply::Unchanged
        ));
        host.shutdown().unwrap();
    }

    pub(super) fn focus(
        host: &mut impl PaneService,
        lease: Lease,
        generation: u64,
        target: FocusTarget,
    ) {
        assert!(matches!(
            host.request(
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
    fn input(
        host: &mut impl PaneService,
        lease: Lease,
        generation: u64,
        input: InteractiveInput,
    ) -> Reply {
        host.request(
            [1; 32],
            lease.connection,
            Request::Input {
                lease,
                generation,
                input,
            },
        )
    }
    fn text_key(text: &str, ime: bool) -> InteractiveInput {
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
    fn capture(host: &mut DevelopmentPaneHost, lease: Lease, bytes: usize, modes: &str) {
        command(
            host,
            1,
            lease,
            &format!(
                "stty raw -echo; printf '{modes}\\033[2J\\033[HREADY\\r\\n'; dd bs=1 count={bytes} 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'"
            ),
        );
        await_text(host, 1, lease, "READY");
    }
    fn next_screen(host: &mut DevelopmentPaneHost, lease: Lease) -> Box<Screen> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Reply::Screen(screen) =
                host.request([1; 32], lease.connection, Request::Poll { lease })
            {
                return screen;
            }
            assert!(Instant::now() < deadline, "screen did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn focus_edges_and_focused_pane_ime_are_observable_at_the_pty() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 16, "\\033[?1004h");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        focus(&mut host, lease, 2, FocusTarget::FocusedPane);
        assert!(matches!(
            input(&mut host, lease, 2, text_key("discarded", false)),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            input(&mut host, lease, 2, text_key("界", true)),
            Reply::Unchanged
        ));
        focus(&mut host, lease, 3, FocusTarget::Unfocused);
        assert!(matches!(
            input(&mut host, lease, 3, text_key("discarded", true)),
            Reply::Refused(Refusal::Stale)
        ));
        focus(&mut host, lease, 4, FocusTarget::Terminal);
        assert!(matches!(
            input(&mut host, lease, 1, text_key("discarded", false)),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            input(&mut host, lease, 4, text_key("K", false)),
            Reply::Unchanged
        ));
        await_text(&mut host, 1, lease, "1b5b4f1b5b491b5b4fe7958c1b5b494b");
        host.shutdown().unwrap();
    }

    fn unsafe_paste(host: &mut DevelopmentPaneHost, lease: Lease, generation: u64) -> u64 {
        let Reply::Paste(PasteOutcome::ConfirmationRequired(confirmation)) = input(
            host,
            lease,
            generation,
            InteractiveInput::Paste {
                text: "a\nb".into(),
            },
        ) else {
            panic!("expected unsafe paste confirmation")
        };
        assert_eq!(confirmation.byte_len, 3);
        assert_eq!(confirmation.line_count, 2);
        assert!(confirmation.multiline);
        confirmation.id
    }

    #[test]
    fn paste_confirmation_preserves_exact_modal_authority_and_focus_loss_cancels_it() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 5, "");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        let first = unsafe_paste(&mut host, lease, 1);
        focus(
            &mut host,
            lease,
            1,
            FocusTarget::PasteConfirmation { id: first },
        );
        focus(&mut host, lease, 2, FocusTarget::Unfocused);
        focus(&mut host, lease, 3, FocusTarget::Terminal);
        assert!(matches!(
            input(
                &mut host,
                lease,
                3,
                InteractiveInput::ResolvePaste {
                    id: first,
                    confirm: true
                }
            ),
            Reply::Paste(PasteOutcome::Stale)
        ));
        let cancelled = unsafe_paste(&mut host, lease, 3);
        assert!(matches!(
            input(
                &mut host,
                lease,
                3,
                InteractiveInput::ResolvePaste {
                    id: cancelled,
                    confirm: false
                }
            ),
            Reply::Paste(PasteOutcome::Cancelled)
        ));
        assert!(matches!(
            input(
                &mut host,
                lease,
                3,
                InteractiveInput::Paste { text: "ok".into() }
            ),
            Reply::Paste(PasteOutcome::Written)
        ));
        let id = unsafe_paste(&mut host, lease, 3);
        assert_ne!(id, first);
        focus(&mut host, lease, 3, FocusTarget::PasteConfirmation { id });
        assert!(matches!(
            input(
                &mut host,
                lease,
                3,
                InteractiveInput::ResolvePaste { id, confirm: true }
            ),
            Reply::Paste(PasteOutcome::Written)
        ));
        assert!(matches!(
            input(
                &mut host,
                lease,
                3,
                InteractiveInput::ResolvePaste { id, confirm: true }
            ),
            Reply::Paste(PasteOutcome::Stale)
        ));
        await_text(&mut host, 1, lease, "6f6b610d62");
        host.shutdown().unwrap();
    }

    fn screen_with_marker(
        host: &mut DevelopmentPaneHost,
        lease: Lease,
        marker: &str,
    ) -> Box<Screen> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Reply::Screen(screen) = host.request([1; 32], [1; 32], Request::Poll { lease })
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        .starts_with(marker)
                })
            {
                return screen;
            }
            assert!(Instant::now() < deadline, "missing PTY marker {marker}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn redraw_between_pointer_press_and_release_does_not_swallow_the_next_click() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        command(
            &mut host,
            1,
            lease,
            "stty raw -echo; printf '\\033[?1002h\\033[?1006h\\033[2J\\033[HREADY\\r\\n'; dd bs=1 count=9 >/dev/null 2>/dev/null; printf 'CHANGED\\r\\n'; reports=; while :; do byte=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); [ \"$byte\" = 21 ] && break; reports=$reports$byte; done; printf 'R:%s\\r\\n' \"$reports\"",
        );
        let original = screen_with_marker(&mut host, lease, "READY");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        let press = Pointer {
            screen_generation: original.generation,
            phase: pane::PointerPhase::Press,
            button: Some(pane::PointerButton::Left),
            x: 4.0,
            y: 5.0,
            modifiers: 0,
            shift_selection: false,
        };
        assert!(matches!(
            input(&mut host, lease, 1, InteractiveInput::Pointer(press)),
            Reply::Unchanged
        ));
        let changed = screen_with_marker(&mut host, lease, "CHANGED");
        assert_ne!(changed.generation, original.generation);
        // An accepted application gesture continues across output in the same geometry.
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer {
                    phase: pane::PointerPhase::Motion,
                    x: 20.0,
                    y: 23.0,
                    ..press
                })
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer {
                    phase: pane::PointerPhase::Release,
                    x: 24.0,
                    y: 41.0,
                    ..press
                })
            ),
            Reply::Unchanged
        ));
        let fresh = Pointer {
            screen_generation: changed.generation,
            x: 12.0,
            y: 23.0,
            ..press
        };
        for phase in [pane::PointerPhase::Press, pane::PointerPhase::Release] {
            assert!(matches!(
                input(
                    &mut host,
                    lease,
                    1,
                    InteractiveInput::Pointer(Pointer { phase, ..fresh })
                ),
                Reply::Unchanged
            ));
        }
        // With no active gesture, the old snapshot cannot start another click.
        input(&mut host, lease, 1, InteractiveInput::Pointer(press));
        input(&mut host, lease, 1, text_key("!", false));
        let screen = screen_with_marker(&mut host, lease, "R:");
        let report = screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .find(|row| row.starts_with("R:"))
            .unwrap();
        assert_eq!(
            report.trim_end(),
            "R:1b5b3c33323b333b324d1b5b3c303b343b336d1b5b3c303b323b324d1b5b3c303b323b326d"
        );
        assert!(
            !host
                .pane
                .as_ref()
                .unwrap()
                .screen
                .as_ref()
                .unwrap()
                .selection_present
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn geometry_change_balances_pointer_release_and_publishes_without_program_output() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        command(
            &mut host,
            1,
            lease,
            "stty raw -echo; printf '\\033[?1000h\\033[?1006h\\033[2J\\033[HREADY\\r\\n'; reports=; while :; do byte=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); [ \"$byte\" = 21 ] && break; reports=$reports$byte; done; printf 'R:%s\\r\\n' \"$reports\"",
        );
        let original = screen_with_marker(&mut host, lease, "READY");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        let press = Pointer {
            screen_generation: original.generation,
            phase: pane::PointerPhase::Press,
            button: Some(pane::PointerButton::Left),
            x: 4.0,
            y: 5.0,
            modifiers: 0,
            shift_selection: false,
        };
        input(&mut host, lease, 1, InteractiveInput::Pointer(press));
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::Resize {
                    lease,
                    size: PaneSize {
                        cell_width: 9,
                        ..size()
                    }
                }
            ),
            Reply::Unchanged
        ));
        let resized = next_screen(&mut host, lease);
        assert!(resized.generation > original.generation);
        assert_eq!((resized.columns, resized.rows.len()), (80, 24));
        // A discarded gesture cannot be resumed by its delayed release or a stale press.
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Pointer(Pointer {
                phase: pane::PointerPhase::Release,
                x: 24.0,
                y: 41.0,
                ..press
            }),
        );
        input(&mut host, lease, 1, InteractiveInput::Pointer(press));
        let fresh = Pointer {
            screen_generation: resized.generation,
            x: 12.0,
            y: 23.0,
            ..press
        };
        for phase in [pane::PointerPhase::Press, pane::PointerPhase::Release] {
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer { phase, ..fresh }),
            );
        }
        input(&mut host, lease, 1, text_key("!", false));
        let screen = screen_with_marker(&mut host, lease, "R:");
        let report = screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .find(|row| row.starts_with("R:"))
            .unwrap();
        assert_eq!(
            report.trim_end(),
            "R:1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c303b323b324d1b5b3c303b323b326d"
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn application_pointer_and_wheel_encode_at_the_pty_and_stale_screens_are_dropped() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 28, "\\033[?1000h\\033[?1006h");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        // Poll again after a deliberate resize to obtain a fresh authoritative mapping.
        host.request(
            [1; 32],
            [1; 32],
            Request::Resize {
                lease,
                size: PaneSize {
                    columns: 81,
                    ..size()
                },
            },
        );
        let screen = next_screen(&mut host, lease);
        assert!(screen.mouse_tracking);
        let pointer = Pointer {
            screen_generation: screen.generation,
            phase: pane::PointerPhase::Press,
            button: Some(pane::PointerButton::Left),
            x: 4.0,
            y: 5.0,
            modifiers: 0,
            shift_selection: true,
        };
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer {
                    screen_generation: u64::MAX,
                    ..pointer
                })
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            input(&mut host, lease, 1, InteractiveInput::Pointer(pointer)),
            Reply::Unchanged
        ));
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer {
                    screen_generation: screen.generation,
                    phase: pane::PointerPhase::Release,
                    ..pointer
                })
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Wheel(Wheel {
                    screen_generation: screen.generation,
                    horizontal_steps: 0,
                    vertical_steps: 1,
                    phase: pane::WheelPhase::GestureChanged,
                    x: 4.0,
                    y: 5.0,
                    modifiers: 0,
                    shift_selection: true
                })
            ),
            Reply::Unchanged
        ));
        await_text(
            &mut host,
            1,
            lease,
            "1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c36343b313b314d",
        );
        assert!(
            !host
                .pane
                .as_ref()
                .unwrap()
                .screen
                .as_ref()
                .unwrap()
                .selection_present
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn bracketed_paste_reuses_bounds_normalization_and_control_sanitization() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 30, "\\033[?2004h");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        for (text, expected) in [
            (String::new(), pane::PasteRejection::Empty),
            (
                "x".repeat(1_048_577),
                pane::PasteRejection::TooLarge { limit: 1_048_576 },
            ),
        ] {
            assert!(
                matches!(input(&mut host, lease, 1, InteractiveInput::Paste { text }), Reply::Paste(PasteOutcome::Rejected(reason)) if reason == expected)
            );
        }
        let Reply::Paste(PasteOutcome::ConfirmationRequired(confirmation)) = input(
            &mut host,
            lease,
            1,
            InteractiveInput::Paste {
                text: "x\x1b[201~y".into(),
            },
        ) else {
            panic!("closing fence must require confirmation")
        };
        assert!(confirmation.closing_fence);
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::ResolvePaste {
                id: confirmation.id,
                confirm: false,
            },
        );
        for text in ["a\r\nb", "a\x1bb"] {
            assert!(matches!(
                input(
                    &mut host,
                    lease,
                    1,
                    InteractiveInput::Paste { text: text.into() }
                ),
                Reply::Paste(PasteOutcome::Written)
            ));
        }
        await_text(
            &mut host,
            1,
            lease,
            "1b5b3230307e610a621b5b3230317e1b5b3230307e6120621b5b3230317e",
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn reconnect_takeover_and_wrong_device_invalidate_paste_and_input_authority() {
        let mut host = host();
        let original = attach(&mut host, 1, true);
        capture(&mut host, original, 2, "");
        focus(&mut host, original, 1, FocusTarget::Terminal);
        let pending = unsafe_paste(&mut host, original, 1);
        host.disconnected(original.connection);
        assert!(matches!(
            input(&mut host, original, 1, text_key("bad", false)),
            Reply::Refused(Refusal::Stale)
        ));
        let resumed = attach(&mut host, 2, false);
        assert_eq!(resumed.session, original.session);
        assert!(matches!(
            input(&mut host, resumed, 1, text_key("bad", false)),
            Reply::Refused(Refusal::Stale)
        ));
        focus(&mut host, resumed, 1, FocusTarget::Terminal);
        assert!(matches!(
            input(
                &mut host,
                resumed,
                1,
                InteractiveInput::ResolvePaste {
                    id: pending,
                    confirm: true
                }
            ),
            Reply::Paste(PasteOutcome::Stale)
        ));
        unsafe_paste(&mut host, resumed, 1);
        let Reply::Attached { lease: successor } = host.request(
            [2; 32],
            [3; 32],
            Request::TakeOver {
                target: resumed.into(),
                size: size(),
                style: None,
            },
        ) else {
            panic!("takeover failed")
        };
        for request in [
            Request::Focus {
                lease: resumed,
                generation: 2,
                target: FocusTarget::Terminal,
            },
            Request::Input {
                lease: resumed,
                generation: 1,
                input: text_key("bad", false),
            },
            Request::Input {
                lease: resumed,
                generation: 1,
                input: InteractiveInput::ResolvePaste {
                    id: pending,
                    confirm: true,
                },
            },
        ] {
            assert!(matches!(
                host.request([1; 32], [2; 32], request),
                Reply::Refused(Refusal::Stale)
            ));
        }
        assert!(matches!(
            host.request(
                [1; 32],
                [3; 32],
                Request::Focus {
                    lease: successor,
                    generation: 1,
                    target: FocusTarget::Terminal
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [2; 32],
                [3; 32],
                Request::Focus {
                    lease: successor,
                    generation: 1,
                    target: FocusTarget::Terminal
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request(
                [2; 32],
                [3; 32],
                Request::Input {
                    lease: successor,
                    generation: 1,
                    input: InteractiveInput::ResolvePaste {
                        id: pending,
                        confirm: true
                    }
                }
            ),
            Reply::Paste(PasteOutcome::Stale)
        ));
        assert!(matches!(
            host.request(
                [2; 32],
                [3; 32],
                Request::Input {
                    lease: successor,
                    generation: 1,
                    input: text_key("ok", false)
                }
            ),
            Reply::Unchanged
        ));
        // The new device has exactly the same running PTY, with no cancelled payload bytes.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Reply::Screen(screen) =
                host.request([2; 32], [3; 32], Request::Poll { lease: successor })
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        == "6f6b"
                })
            {
                break;
            }
            assert!(Instant::now() < deadline, "successor PTY witness missing");
            std::thread::sleep(Duration::from_millis(5));
        }
        host.shutdown().unwrap();
    }

    #[test]
    fn malformed_interactive_values_never_reach_the_pty() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        capture(&mut host, lease, 2, "");
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        let generation = host.pane.as_ref().unwrap().screen_generation;
        for invalid in [
            InteractiveInput::Key(Key {
                physical: u32::MAX,
                action: 0,
                logical: "x".into(),
                text: Some("bad".into()),
                unshifted: None,
                modifiers: 0,
                consumed: 0,
                option_as_alt: false,
            }),
            InteractiveInput::Pointer(Pointer {
                screen_generation: generation,
                phase: pane::PointerPhase::Press,
                button: None,
                x: 1.0,
                y: 1.0,
                modifiers: 0,
                shift_selection: false,
            }),
            InteractiveInput::Pointer(Pointer {
                screen_generation: generation,
                phase: pane::PointerPhase::Press,
                button: Some(pane::PointerButton::Left),
                x: f32::NAN,
                y: 1.0,
                modifiers: 0,
                shift_selection: false,
            }),
            InteractiveInput::Wheel(Wheel {
                screen_generation: generation,
                horizontal_steps: i32::MIN,
                vertical_steps: 0,
                phase: pane::WheelPhase::GestureChanged,
                x: 1.0,
                y: 1.0,
                modifiers: 0,
                shift_selection: false,
            }),
        ] {
            assert!(matches!(
                input(&mut host, lease, 1, invalid),
                Reply::Refused(Refusal::Unsupported)
            ));
        }
        input(&mut host, lease, 1, text_key("ok", false));
        await_text(&mut host, 1, lease, "6f6b");
        host.shutdown().unwrap();
    }

    #[test]
    fn application_input_never_starts_selection_or_scrolls_server_history() {
        for modes in ["", "\\033[?1049h\\033[?1000h\\033[?1007h"] {
            let mut host = host();
            let lease = attach(&mut host, 1, true);
            capture(&mut host, lease, 2, modes);
            focus(&mut host, lease, 1, FocusTarget::Terminal);
            let generation = host.pane.as_ref().unwrap().screen_generation;
            let pointer = Pointer {
                screen_generation: generation,
                phase: pane::PointerPhase::Press,
                button: Some(pane::PointerButton::Left),
                x: 4.0,
                y: 5.0,
                modifiers: 1,
                shift_selection: true,
            };
            input(&mut host, lease, 1, InteractiveInput::Pointer(pointer));
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Pointer(Pointer {
                    phase: pane::PointerPhase::Release,
                    ..pointer
                }),
            );
            input(
                &mut host,
                lease,
                1,
                InteractiveInput::Wheel(Wheel {
                    screen_generation: generation,
                    horizontal_steps: 0,
                    vertical_steps: 3,
                    phase: pane::WheelPhase::GestureChanged,
                    x: 4.0,
                    y: 5.0,
                    modifiers: 1,
                    shift_selection: true,
                }),
            );
            input(&mut host, lease, 1, text_key("ok", false));
            await_text(&mut host, 1, lease, "6f6b");
            let screen = host.pane.as_ref().unwrap().screen.as_ref().unwrap();
            assert!(!screen.selection_present);
            assert_eq!(
                screen.viewport.offset_rows + screen.viewport.visible_rows,
                screen.scrollbar.total_rows
            );
            host.shutdown().unwrap();
        }
    }

    #[test]
    fn shared_interactive_input_uses_controlling_size_and_terminal_visible_appearance() {
        let mut host = SharedWorkspaceHost::new(host());
        create_shared(&mut host, 1);
        let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
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
            panic!("shared Pane missing")
        };
        let style = PresentationStyle {
            light: true,
            foreground: [20; 3],
            background: [240; 3],
            palette: [[0; 3]; 16],
            cursor: [20; 3],
            cursor_text: None,
            bold_as_bright: false,
        };
        assert!(matches!(
            host.request(
                [2; 32],
                [2; 32],
                Request::Style {
                    lease,
                    style: style.clone()
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request([1; 32], [1; 32], Request::Style { lease, style }),
            Reply::Unchanged
        ));
        host.request(
            [1; 32],
            [1; 32],
            Request::Resize {
                lease,
                size: PaneSize {
                    columns: 91,
                    rows: 31,
                    cell_width: 9,
                    cell_height: 19,
                },
            },
        );
        focus(&mut host, lease, 1, FocusTarget::Terminal);
        input(
            &mut host,
            lease,
            1,
            text_key(
                "stty -echo; stty size; stty raw -echo; printf '\\033[?996n'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n'",
                false,
            ),
        );
        input(
            &mut host,
            lease,
            1,
            InteractiveInput::Key(Key {
                physical: PhysicalKey::Enter as u32,
                action: 0,
                logical: "enter".into(),
                text: None,
                unshifted: None,
                modifiers: 0,
                consumed: 0,
                option_as_alt: false,
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Reply::Screen(screen) = host.request([1; 32], [1; 32], Request::Poll { lease }) {
                let rows = screen
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                if rows
                    .iter()
                    .any(|row| row.trim_end() == "1b5b3f3939373b326e")
                {
                    assert_eq!((screen.columns, screen.rows.len()), (91, 31));
                    assert_eq!(screen.background, [240; 3]);
                    assert!(rows.iter().any(|row| row.trim_end() == "31 91"));
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "controlling appearance query response missing"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        host.shutdown().unwrap();
    }

    pub(super) fn shared_inventory(
        host: &mut SharedWorkspaceHost,
    ) -> spaceterm_server::hierarchy::Inventory {
        let Reply::Inventory { inventory, .. } = host.request([2; 32], [2; 32], Request::Inventory)
        else {
            panic!("inventory unavailable")
        };
        inventory
    }

    pub(super) fn create_shared(host: &mut SharedWorkspaceHost, sequence: u64) -> [u8; 32] {
        use spaceterm_server::hierarchy::{Mutation, Outcome};
        let Reply::Outcome(result) = host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence,
                mutation: Mutation::CreateWorkspace {
                    name: "Build".into(),
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
    fn shared_kill_ends_another_controllers_work_and_replays_without_touching_survivors() {
        use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
        let mut host = SharedWorkspaceHost::new(host());
        let workspace = create_shared(&mut host, 1);
        let other = create_shared(&mut host, 2);
        let inventory = shared_inventory(&mut host);
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
        let target = CloseTarget::Workspace { workspace };
        let confirmation = shared_inventory(&mut host)
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
        assert!(matches!(
            host.request([1; 32], [1; 32], Request::Poll { lease }),
            Reply::Refused(Refusal::Missing)
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
        let inventory = shared_inventory(&mut host);
        assert_eq!(inventory.workspaces.len(), 1);
        assert_eq!(inventory.workspaces[0].id, other);
        host.shutdown().unwrap();
    }

    #[test]
    fn shared_close_rejects_a_session_started_after_confirmation_but_allows_takeover() {
        use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
        let mut host = SharedWorkspaceHost::new(host());
        let workspace = create_shared(&mut host, 1);
        let initial = shared_inventory(&mut host);
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
        let confirmation = shared_inventory(&mut host)
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
        assert!(shared_inventory(&mut host).workspaces.is_empty());
    }

    #[test]
    fn shared_close_rejects_membership_changes_and_confirmations_from_another_boot() {
        use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};
        let mut host = SharedWorkspaceHost::new(host());
        let workspace = create_shared(&mut host, 1);
        let target = CloseTarget::Workspace { workspace };
        let confirmation = shared_inventory(&mut host)
            .close_confirmation(target)
            .unwrap();
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: Mutation::CreateTab { workspace },
            },
        );
        assert!(
            matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 1, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation)
        );
        let mut confirmation = shared_inventory(&mut host)
            .close_confirmation(target)
            .unwrap();
        confirmation.boot = [0; 32];
        assert!(
            matches!(host.request([2; 32], [2; 32], Request::Mutate { sequence: 2, mutation: Mutation::Close { target, confirmation } }), Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation)
        );
        assert_eq!(shared_inventory(&mut host).workspaces[0].tabs.len(), 2);
    }

    #[test]
    fn shared_close_rejects_new_ancestor_deletion_after_a_sibling_closes() {
        use spaceterm_server::hierarchy::{Axis, CloseTarget, Mutation, Outcome};
        for close_pane in [true, false] {
            let mut host = SharedWorkspaceHost::new(host());
            let workspace = create_shared(&mut host, 1);
            let initial = shared_inventory(&mut host);
            let tab = initial.workspaces[0].root_tab;
            let pane = initial.workspaces[0].tabs[0].root_pane;
            let mutation = if close_pane {
                Mutation::SplitPane {
                    workspace,
                    tab,
                    source: pane,
                    axis: Axis::Down,
                }
            } else {
                Mutation::CreateTab { workspace }
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
            let inventory = shared_inventory(&mut host);
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
            let remaining = shared_inventory(&mut host);
            assert_eq!(remaining.workspaces[0].id, workspace);
            let confirmation = remaining.close_confirmation(target).unwrap();
            assert!(matches!(host.request(
                [3; 32], [3; 32], Request::Mutate { sequence: 2, mutation: Mutation::Close {
                    target, confirmation,
                } },
            ), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
            assert!(shared_inventory(&mut host).workspaces.is_empty());
        }
    }

    #[test]
    fn shared_session_reset_retains_membership_but_ordinary_final_shell_exit_removes_it() {
        let mut host = SharedWorkspaceHost::new(host());
        create_shared(&mut host, 1);
        let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
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
            shared_inventory(&mut host).workspaces[0].tabs[0].root_pane,
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
        let reply = command(&mut host, 1, lease, "exit\n");
        assert!(matches!(reply, Reply::Unchanged));
        let until = Instant::now() + Duration::from_secs(3);
        while !shared_inventory(&mut host).workspaces.is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared_inventory(&mut host).workspaces.is_empty());
        host.shutdown().unwrap();
    }

    #[test]
    fn opening_the_legacy_pane_cannot_consume_a_shared_edits_operation_identity() {
        use spaceterm_server::hierarchy::{Mutation, Outcome};
        let mut host = SharedWorkspaceHost::new(host());
        let Reply::Inventory { next_sequence, .. } =
            host.request([1; 32], [1; 32], Request::Inventory)
        else {
            panic!()
        };
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: true,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Attached { .. }
        ));
        assert!(
            matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: next_sequence, mutation: Mutation::CreateWorkspace { name: "Explicit creation".into() } }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. }))
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn shared_membership_preserves_independent_pane_grants_and_lazy_startup() {
        use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};
        let mut host = SharedWorkspaceHost::new(host());
        let created = host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 1,
                mutation: Mutation::CreateWorkspace {
                    name: "Build".into(),
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
                mutation: Mutation::CreateTab { workspace: w.id },
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

    pub(super) fn host() -> DevelopmentPaneHost {
        let filesystem = local_filesystem();
        let directory = filesystem
            .validate_directory(&std::env::temp_dir())
            .unwrap();
        let factory = NativeTerminalSessionFactory::new(
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
            filesystem,
            LocalMachine::default(),
        );
        DevelopmentPaneHost::new(factory, directory).unwrap()
    }
    pub(super) fn size() -> PaneSize {
        PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        }
    }
    pub(super) fn attach(host: &mut DevelopmentPaneHost, connection: u8, create: bool) -> Lease {
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [connection; 32],
            Request::Attach {
                create,
                size: size(),
                resume: None,
                style: None,
            },
        ) else {
            panic!("expected attachment")
        };
        lease
    }
    pub(super) fn command(
        host: &mut impl PaneService,
        connection: u8,
        lease: Lease,
        text: &str,
    ) -> Reply {
        let result = host.request(
            [1; 32],
            [connection; 32],
            Request::Key {
                lease,
                key: Key {
                    physical: PhysicalKey::Unidentified as u32,
                    action: 0,
                    logical: "text-input".into(),
                    text: Some(text.trim_end_matches('\n').into()),
                    unshifted: None,
                    modifiers: 0,
                    consumed: 0,
                    option_as_alt: false,
                },
            },
        );
        if !matches!(result, Reply::Unchanged) {
            return result;
        }
        host.request(
            [1; 32],
            [connection; 32],
            Request::Key {
                lease,
                key: Key {
                    physical: PhysicalKey::Enter as u32,
                    action: 0,
                    logical: "enter".into(),
                    text: None,
                    unshifted: None,
                    modifiers: 0,
                    consumed: 0,
                    option_as_alt: false,
                },
            },
        )
    }
    pub(super) fn await_text(
        host: &mut impl PaneService,
        connection: u8,
        lease: Lease,
        text: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Reply::Screen(screen) =
                host.request([1; 32], [connection; 32], Request::Poll { lease })
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|c| c.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        == text
                })
            {
                return;
            }
            assert!(Instant::now() < deadline, "expected terminal output {text}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn select(host: &mut DevelopmentPaneHost, device: u8, connection: u8) -> Reply {
        host.request(
            [device; 32],
            [connection; 32],
            Request::Select {
                create: true,
                size: size(),
                resume: None,
                style: None,
            },
        )
    }

    #[test]
    fn a_restored_pane_waits_for_explicit_first_use_and_starts_only_once() {
        let mut host = host();
        host.organization = Some(Organization::first(host.directory.clone()).unwrap());
        let pane_id = host.organization.as_ref().unwrap().pane;
        for resume in [
            None,
            Some(PaneTarget {
                server: [8; 32],
                session: [9; 32],
            }),
        ] {
            assert!(matches!(host.request([1; 32], [1; 32], Request::Select {
                create: false, size: size(), resume, style: None,
            }), Reply::Restored { pane, .. } if pane == pane_id));
            host.tick();
            assert!(host.pane.is_none());
        }
        let request = Request::StartRestored {
            server: host.boot,
            pane: pane_id,
            size: size(),
            style: None,
        };
        let Reply::Attached { lease } = host.request([1; 32], [1; 32], request.clone()) else {
            panic!("first use");
        };
        assert!(
            matches!(host.request([2; 32], [2; 32], request), Reply::Takeover { target } if target == lease.into())
        );
        assert_eq!(host.organization.as_ref().unwrap().pane, pane_id);
        host.shutdown().unwrap();
    }

    #[test]
    fn a_missing_confirmed_directory_refuses_startup_without_falling_back() {
        let mut host = host();
        let mut organization = Organization::first(host.directory.clone()).unwrap();
        organization.confirmed_directory = Some(host.directory.join(format!(
            "missing-{:x}",
            u128::from_be_bytes(nonce().unwrap()[..16].try_into().unwrap())
        )));
        host.organization = Some(organization.clone());
        assert!(matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::StartRestored {
                    server: host.boot,
                    pane: organization.pane,
                    size: size(),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::DirectoryUnavailable)
        ));
        assert!(host.pane.is_none());
        assert!(host.organization.as_ref() == Some(&organization));
    }

    #[test]
    fn ordinary_exit_retires_organization_without_a_connected_client() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        command(&mut host, 1, lease, "exit");
        host.disconnected([1; 32]);
        let until = Instant::now() + Duration::from_secs(3);
        while host.organization.is_some() && Instant::now() < until {
            host.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            host.organization.is_none(),
            "ordinary exit closes the final Pane even without a client polling"
        );
    }

    #[test]
    fn only_owning_shell_directory_confirmation_changes_restoration() {
        let mut host = host();
        let lease = attach(&mut host, 1, true);
        command(
            &mut host,
            1,
            lease,
            "stty -echo; printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HCONFIRMED\\n'",
        );
        await_text(&mut host, 1, lease, "CONFIRMED");
        host.tick();
        assert_eq!(
            host.organization
                .as_ref()
                .unwrap()
                .confirmed_directory
                .as_deref(),
            Some(std::path::Path::new("/"))
        );
        command(
            &mut host,
            1,
            lease,
            "printf '\\033]7;file://localhost/tmp\\007\\033]133;A;spaceterm=foreign\\007'; printf 'NESTED\\n'",
        );
        await_text(&mut host, 1, lease, "NESTED");
        host.tick();
        assert_eq!(
            host.organization
                .as_ref()
                .unwrap()
                .confirmed_directory
                .as_deref(),
            Some(std::path::Path::new("/"))
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn takeover_preserves_work_and_rejects_former_controller_operations() {
        let mut host = host();
        let original = attach(&mut host, 2, true);
        command(
            &mut host,
            2,
            original,
            "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 1; printf 'CONTINUED\\n'\n",
        );
        await_text(&mut host, 2, original, "STARTED");
        host.disconnected([2; 32]);
        let Reply::Takeover { target } = select(&mut host, 9, 9) else {
            panic!("selection must not replace a disconnected controller");
        };
        let Reply::Attached { lease: transferred } = host.request(
            [9; 32],
            [9; 32],
            Request::TakeOver {
                target,
                size: PaneSize {
                    columns: 90,
                    ..size()
                },
                style: None,
            },
        ) else {
            panic!("explicit takeover must transfer control");
        };
        assert_eq!(original.session, transferred.session);
        assert_ne!(original.control, transferred.control);
        for request in [
            Request::Poll { lease: original },
            Request::Resize {
                lease: original,
                size: size(),
            },
            Request::Close { lease: original },
            Request::Release { lease: original },
        ] {
            assert!(matches!(
                host.request([1; 32], [2; 32], request),
                Reply::Refused(Refusal::Stale)
            ));
        }
        assert!(matches!(
            command(&mut host, 2, original, "printf 'STALE'\n"),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [3; 32],
                Request::Select {
                    create: false,
                    size: size(),
                    resume: Some(original.into()),
                    style: None,
                }
            ),
            Reply::Takeover { .. }
        ));
        // Take it back explicitly, keeping the running command and the same Session.
        let Reply::Attached { lease: returned } = host.request(
            [1; 32],
            [3; 32],
            Request::TakeOver {
                target,
                size: size(),
                style: None,
            },
        ) else {
            panic!("take back");
        };
        assert_eq!(original.session, returned.session);
        await_text(&mut host, 3, returned, "CONTINUED");
        host.shutdown().unwrap();
    }

    #[test]
    fn detached_attachment_fences_lost_claim_replies_and_late_release_preserves_reattachment() {
        use spaceterm_server::{attachments::Attachment, hierarchy::Mutation};
        let mut host = SharedWorkspaceHost::new(host());
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 1,
                mutation: Mutation::CreateWorkspace {
                    name: "work".into(),
                },
            },
        );
        let inventory = shared_inventory(&mut host);
        let workspace = inventory.workspaces[0].id;
        let pane = inventory.workspaces[0].tabs[0].root_pane;
        let original = Attachment {
            workspace,
            generation: 1,
        };
        let claim = |attachment, explicit| Request::ClaimPane {
            attachment,
            pane,
            claim: AttachmentClaim::Select { explicit },
            size: size(),
            style: None,
        };
        let Reply::Attached { lease: first } =
            host.request([1; 32], [2; 32], claim(original, true))
        else {
            panic!("claim");
        };
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 2,
                mutation: Mutation::SplitPane {
                    workspace,
                    tab: inventory.workspaces[0].root_tab,
                    source: pane,
                    axis: spaceterm_server::hierarchy::Axis::Right,
                },
            },
        );
        let split = shared_inventory(&mut host).workspaces[0].tabs[0].panes[1].id;
        let Reply::Attached { lease: split_lease } = host.request(
            [1; 32],
            [6; 32],
            Request::ClaimPane {
                attachment: original,
                pane: split,
                claim: AttachmentClaim::Select { explicit: true },
                size: size(),
                style: None,
            },
        ) else {
            panic!("second Pane claim");
        };
        // The acquisition committed, but the desktop lost its reply before offline detach.
        host.disconnected([2; 32]);
        host.disconnected([6; 32]);
        assert!(matches!(
            host.request(
                [1; 32],
                [3; 32],
                Request::ReleaseAttachment {
                    attachment: original
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request([1; 32], [2; 32], claim(original, true)),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(
            matches!(
                host.request([1; 32], [6; 32], Request::Poll { lease: split_lease }),
                Reply::Refused(Refusal::Stale)
            ),
            "detach releases every original Pane grant"
        );
        let later = Attachment {
            workspace,
            generation: 2,
        };
        assert!(matches!(
            host.request([1; 32], [4; 32], claim(later, false)),
            Reply::Takeover { .. }
        ));
        let Reply::Attached { lease: current } = host.request([1; 32], [4; 32], claim(later, true))
        else {
            panic!("reattach selects without resuming old control");
        };
        assert_eq!(current.session, first.session);
        assert_ne!(current.control, first.control);
        // Lost release acknowledgement is safely retried after a later grant exists.
        assert!(matches!(
            host.request(
                [1; 32],
                [5; 32],
                Request::ReleaseAttachment {
                    attachment: original
                }
            ),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request([1; 32], [4; 32], Request::Poll { lease: current }),
            Reply::Screen(_) | Reply::Unchanged
        ));
        let Reply::Attached { lease: other } = host.request(
            [9; 32],
            [9; 32],
            Request::TakeOver {
                target: current.into(),
                size: size(),
                style: None,
            },
        ) else {
            panic!("takeover");
        };
        host.request(
            [1; 32],
            [5; 32],
            Request::ReleaseAttachment { attachment: later },
        );
        assert!(matches!(
            host.request([9; 32], [9; 32], Request::Poll { lease: other }),
            Reply::Screen(_) | Reply::Unchanged
        ));
        assert_eq!(shared_inventory(&mut host).workspaces.len(), 1);
        host.shutdown().unwrap();
    }

    #[test]
    fn released_control_has_one_selection_winner_and_never_resumes_old_grants() {
        let mut host = host();
        let original = attach(&mut host, 2, true);
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Release { lease: original }),
            Reply::Unchanged
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [3; 32],
                Request::Select {
                    create: false,
                    size: size(),
                    resume: Some(original.into()),
                    style: None,
                }
            ),
            Reply::Takeover { .. }
        ));
        let host = Arc::new(std::sync::Mutex::new(host));
        let start = Arc::new(std::sync::Barrier::new(3));
        let workers = [(9, 9), (8, 8)].map(|(device, connection)| {
            let host = Arc::clone(&host);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                select(&mut host.lock().unwrap(), device, connection)
            })
        });
        start.wait();
        let replies = workers.map(|worker| worker.join().unwrap());
        assert_eq!(
            replies
                .iter()
                .filter(|reply| matches!(reply, Reply::Attached { .. }))
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|reply| matches!(reply, Reply::Takeover { .. }))
                .count(),
            1
        );
        let mut host = host.lock().unwrap();
        assert!(matches!(select(&mut host, 1, 3), Reply::Takeover { .. }));
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Poll { lease: original }),
            Reply::Refused(Refusal::Stale)
        ));
        host.shutdown().unwrap();
    }

    #[test]
    fn reconnect_keeps_running_work_and_fences_the_old_connection() {
        let mut host = host();
        let first = attach(&mut host, 2, true);
        command(
            &mut host,
            2,
            first,
            "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 1; printf 'CONTINUED\\n'\n",
        );
        await_text(&mut host, 2, first, "STARTED");
        host.disconnected([2; 32]);
        std::thread::sleep(Duration::from_millis(1100));
        let second = attach(&mut host, 3, false);
        assert_eq!(first.session, second.session);
        assert!(matches!(
            command(&mut host, 2, first, "printf 'STALE'\n"),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [9; 32],
                [9; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Controlled)
        ));
        await_text(&mut host, 3, second, "CONTINUED");
        host.shutdown().unwrap();
    }

    #[test]
    fn reconnect_and_stale_generations_cannot_start_or_mutate_a_replacement() {
        let mut host = host();
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let old = attach(&mut host, 2, true);
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Close { lease: old }),
            Reply::Closed
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let live = attach(&mut host, 2, true);
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: PaneSize {
                        columns: 90,
                        ..size()
                    },
                    style: None,
                    resume: Some(old)
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        for stale in [
            Lease {
                server: old.server.map(|b| b ^ 1),
                ..live
            },
            Lease {
                session: old.session,
                ..live
            },
            Lease {
                control: old.control,
                ..live
            },
        ] {
            assert!(matches!(
                host.request([1; 32], [2; 32], Request::Close { lease: stale }),
                Reply::Refused(Refusal::Stale)
            ));
            assert!(matches!(
                host.request(
                    [1; 32],
                    [2; 32],
                    Request::Resize {
                        lease: stale,
                        size: PaneSize {
                            columns: 90,
                            ..size()
                        }
                    }
                ),
                Reply::Refused(Refusal::Stale)
            ));
        }
        command(
            &mut host,
            2,
            live,
            "stty -echo; printf '\\033[2J\\033[HLIVE\\n'; stty size\n",
        );
        await_text(&mut host, 2, live, "24 80");
        host.shutdown().unwrap();
    }

    #[test]
    fn a_waiting_client_observes_exit_without_the_controller_polling() {
        let mut host = host();
        let lease = attach(&mut host, 2, true);
        command(
            &mut host,
            2,
            lease,
            "stty -echo; printf '\\033[2J\\033[HEXITING\\n'; sleep 0.2; exit\n",
        );
        await_text(&mut host, 2, lease, "EXITING");
        host.disconnected([2; 32]);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let reply = host.request(
                [9; 32],
                [9; 32],
                Request::Select {
                    create: false,
                    size: size(),
                    resume: Some(lease.into()),
                    style: None,
                },
            );
            if matches!(reply, Reply::Refused(Refusal::Missing)) {
                break;
            }
            assert!(matches!(reply, Reply::Takeover { .. }));
            assert!(
                Instant::now() < deadline,
                "an exited shell still offers takeover"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            host.request(
                [9; 32],
                [9; 32],
                Request::TakeOver {
                    target: lease.into(),
                    size: size(),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let replacement = attach(&mut host, 3, true);
        assert_ne!(replacement.session, lease.session);
        host.shutdown().unwrap();
    }

    #[test]
    fn ordinary_exit_allows_a_later_explicit_launch_but_never_a_reconnect() {
        let mut host = host();
        let old = attach(&mut host, 2, true);
        command(&mut host, 2, old, "exit\n");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if matches!(
                host.request([1; 32], [2; 32], Request::Poll { lease: old }),
                Reply::Ended
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "shell exit did not arrive");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            host.request(
                [1; 32],
                [3; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: Some(old),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let fresh = attach(&mut host, 3, true);
        assert_ne!(fresh.session, old.session);
        command(
            &mut host,
            3,
            fresh,
            "stty -echo; printf '\\033[2J\\033[HNEW-SHELL\\n'\n",
        );
        await_text(&mut host, 3, fresh, "NEW-SHELL");
        host.shutdown().unwrap();
    }
}
