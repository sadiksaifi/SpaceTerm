//! Request dispatch and the connection-facing PaneService boundary of the Development host.
use super::*;

impl DevelopmentPaneHost {
    pub(super) fn handle(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Result<Reply, Refusal> {
        let (request, changes) = match request {
            Request::PollChanges { lease, base } => (Request::Poll { lease }, Some(base)),
            request => (request, None),
        };
        if matches!(request, Request::Capabilities) {
            return Ok(Reply::Capabilities {
                development_pane: true,
                pane_takeover: true,
                interactive_input: true,
                key_input_batches: true,
                native_effects: true,
                reading: true,
                graphics: true,
                accessibility: true,
                directory_recovery: true,
                directory_creation: true,
                directory_matching: true,
                worktree_writes: false,
                end_all_sessions: false,
                workspace_policy: false,
                presentation_changes: true,
            });
        }
        self.recover()?;
        self.retain_directory()?;
        if let Request::DirectoryRead(request) = request {
            return Ok(Reply::DirectoryRead(
                self.directories.request(connection, request),
            ));
        }
        if let Request::DirectoryCreate(request) = request {
            return Ok(Reply::DirectoryCreate(
                self.directories.create(connection, request),
            ));
        }
        match request {
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => {
                return self.repair_directory(device, connection, target, replacement, size, style);
            }
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
                return self.select(device, connection, true, size, None, style);
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
        if let Request::Poll { lease } = &request
            && self.pane.is_none()
            && self.failed_directory_session == Some((*lease).into())
        {
            return Err(Refusal::DirectoryUnavailable);
        }
        // The wire reader enforces this bound too. Check direct PaneService callers before
        // decoding or dispatching any key, including malformed later events in a batch.
        if let Request::Keys { keys, .. } = &request
            && (keys.is_empty()
                || keys.len() > MAX_KEY_BATCH
                || serde_json::to_vec(&request)
                    .map_or(true, |frame| frame.len() > MAX_TERMINAL_FRAME))
        {
            return Err(Refusal::Unsupported);
        }
        let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
        let lease = match &request {
            Request::Accessibility { lease, .. }
            | Request::EndAccessibilityDemand { lease }
            | Request::AccessibilitySelection { lease, .. }
            | Request::Read { lease, .. }
            | Request::ReadingStatus { lease }
            | Request::GraphicsChunk { lease, .. }
            | Request::SelectionChunk { lease, .. } => lease,
            Request::Poll { lease }
            | Request::PresentationDemand { lease, .. }
            | Request::NativeEffects { lease, .. }
            | Request::CompleteClipboard { lease, .. }
            | Request::Focus { lease, .. }
            | Request::Input { lease, .. }
            | Request::Keys { lease, .. }
            | Request::Key { lease, .. }
            | Request::Resize { lease, .. }
            | Request::ClearScreenAndScrollback { lease }
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
        if matches!(request, Request::Poll { .. }) && pane.directory_failed {
            self.retire_exited()?;
            return Err(Refusal::DirectoryUnavailable);
        }
        match request {
            Request::PresentationDemand { visible, .. } => {
                pane.presentation_demand(visible);
                Ok(Reply::Unchanged)
            }
            Request::EndAccessibilityDemand { .. } => {
                pane.runtime.end_server_accessibility_demand();
                Ok(Reply::Unchanged)
            }
            Request::Accessibility {
                revision,
                offset,
                base,
                ..
            } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                if let Some(demand) = pane.runtime.accessibility_demand_sender() {
                    demand.request();
                }
                Ok(Reply::Accessibility(
                    pane.accessibility.chunk(revision, offset, base),
                ))
            }
            Request::AccessibilitySelection {
                focus_generation,
                selection,
                ..
            } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                if focus_generation == 0 || focus_generation != pane.focus_generation {
                    return Err(Refusal::Stale);
                }
                pane.require_terminal_focus()?;
                let _completion = pane.runtime.set_server_accessibility_selection(
                    selection
                        .map(accessibility::decode_selection)
                        .transpose()
                        .map_err(|_| Refusal::Unsupported)?,
                );
                pane.reading.fence(&pane.runtime);
                Ok(Reply::Reading { accepted: true })
            }
            Request::GraphicsChunk { image, offset, .. } => {
                Ok(Reply::GraphicsChunk(pane.graphics_chunk(image, offset)))
            }
            Request::Read { operation, .. } => pane.read(operation),
            Request::ReadingStatus { .. } => pane.reading_status(),
            Request::SelectionChunk {
                generation, offset, ..
            } => {
                if pane
                    .screen
                    .as_ref()
                    .is_none_or(|screen| screen.selection_revision != generation)
                {
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
                if let Some(frame) = pane.screen_frame(changes) {
                    return Ok(frame);
                }
                if pane.failed {
                    return Err(Refusal::Failed);
                }
                if pane.ended {
                    self.retire_exited()?;
                    self.pane = None;
                    Ok(Reply::Ended)
                } else {
                    if let Some(revision) = pane.accessibility.take_changed_revision() {
                        Ok(Reply::AccessibilityRevision { revision })
                    } else {
                        Ok(Reply::Unchanged)
                    }
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
            Request::Keys {
                generation, keys, ..
            } => {
                let reply = pane.keys(generation, keys);
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
            Request::ClearScreenAndScrollback { .. } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime.clear_screen_and_scrollback();
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
}
impl PaneService for DevelopmentPaneHost {
    fn restore(&mut self, store: OrganizationStore) -> Result<(), ServerError> {
        self.organization = store.read()?;
        self.store = Some(store);
        Ok(())
    }
    fn tick(&mut self) {
        self.directories.tick();
        if self.recover().is_ok() && self.retain_directory().is_ok() {
            let _ = self.retire_exited();
        }
    }
    fn admitted(&mut self, client: [u8; 32], name: control::ClientName) {
        self.names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(client, name);
    }
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn disconnected(&mut self, connection: [u8; 32]) {
        self.directories.cancel_connection(connection);
        if let Some(pane) = &mut self.pane
            && pane.lease.connection == connection
        {
            pane.invalidate_input();
            pane.connected = false;
        }
    }
    fn revoked(&mut self, client: [u8; 32]) {
        if let Some(dormant) = &mut self.dormant
            && dormant.device == Some(client)
        {
            dormant.device = None;
            dormant.attachment_generation = None;
        }
        if let Some(pane) = &mut self.pane
            && pane.device == Some(client)
        {
            self.directories.cancel_connection(pane.lease.connection);
            pane.invalidate_input();
            pane.device = None;
            pane.connected = false;
            pane.attachment_generation = None;
            pane.lease.control = [0; 32];
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        self.directories.stop();
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
