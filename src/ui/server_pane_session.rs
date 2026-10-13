//! A disposable desktop presentation of a server-owned Terminal Session.
//!
//! Input goes straight through PaneChannel admission. Content work uses replaceable asynchronous
//! tasks; synchronous Copy and Services inspect only a complete projection-owned result.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{BackgroundExecutor, Task};
use spaceterm_client::pane_channel::{
    EffectCorrelation, EffectKind, EffectPermit, NativeRequestLease, PaneChannel, PaneContextFacts,
    PaneEvent, PaneReadItem, PaneReadRequest, PaneRuntimeEffect, RuntimeEffect,
};
use spaceterm_client::projection::{FindNavigation, FrameLease, PreparedCopyLease, Viewport};
use spaceterm_core::content::{self, CellRef};
use spaceterm_core::content_queries::*;
use spaceterm_core::geometry::{CellPixels, MeasuredGrid, SUPPORTED_GRID_LIMITS};
use spaceterm_client::failure::{ClientFailure, ClientFailureClass};
use spaceterm_core::native_requests::{NativeRequestKind, NativeText};
use spaceterm_terminal::runtime_contracts::{ApplicationPointerInput, ApplicationWheelInput};

use super::server_desktop_contracts::{DesktopClientError, PaneViewToken};
use crate::terminal::geometry::TerminalGeometry;
use crate::terminal::{
    FindDirection, FindQueryGeneration, InputModifiers, KeyInput, PasteConfirmation, PasteDecision, PastePayload,
    PasteRequestOutcome, PasteResolution, PointerButton, PointerInput, PointerPhase,
    PresentationGeneration, ScreenSnapshot, ScrollbackMovement, SelectionCopy, SelectionCopyError,
    ShiftSelectionPolicy, TerminalAppearanceUpdate, TerminalSessionHandle, WheelInput,
};

pub(crate) type CopyPreparationRequest = CopyRequest;
pub(crate) type CopyPreparationTicket = Task<Result<PreparedCopyLease, DesktopClientError>>;

type FrameIdentity = (
    spaceterm_core::authority::EffectFence,
    content::ContentBase,
    Viewport,
);

#[derive(Default)]
struct FrameRequests {
    pending: Option<FrameIdentity>,
    installed: Option<FrameIdentity>,
    failed: Option<(FrameIdentity, Instant)>,
}

#[derive(Clone, Copy)]
struct ScrollbarFacts {
    base: content::ContentBase,
    viewport: Viewport,
    total_rows: u64,
    first_row: Option<u64>,
}

enum PendingPaste {
    Request {
        correlation: EffectCorrelation,
        permit: EffectPermit,
        reply: async_channel::Sender<Result<PasteRequestOutcome, String>>,
        expires: Instant,
        _deadline: Task<()>,
    },
    Approval {
        request: NativeRequestLease,
        confirmation: PasteConfirmation,
        expires: Instant,
        _deadline: Task<()>,
    },
    Resolution {
        correlation: EffectCorrelation,
        permit: EffectPermit,
        decision: PasteDecision,
        reply: async_channel::Sender<Result<PasteResolution, String>>,
        expires: Instant,
        _deadline: Task<()>,
    },
}
impl PendingPaste {
    fn is_current(&self, now: Instant) -> bool {
        match self {
            Self::Request { permit, expires, .. } | Self::Resolution { permit, expires, .. } =>
                now < *expires && permit.is_current(),
            Self::Approval { request, expires, .. } => now < *expires && request.is_current(),
        }
    }
    fn cancel(self) {
        match self {
            Self::Request { reply, .. } => {
                let _ = reply.try_send(Ok(PasteRequestOutcome::Rejected(
                    crate::terminal::paste::PasteRejection::TerminalUnfocused,
                )));
            }
            Self::Resolution { reply, .. } => { let _ = reply.try_send(Ok(PasteResolution::Stale)); }
            Self::Approval { .. } => {}
        }
    }
}

pub(crate) struct ServerPaneSession {
    channel: PaneChannel,
    token: Cell<PaneViewToken>,
    own_client: Cell<Option<spaceterm_core::identity::ClientId>>,
    closed: Arc<AtomicBool>,
    enabled: Cell<bool>,
    executor: RefCell<Option<BackgroundExecutor>>,
    updates: async_channel::Sender<()>,
    receiver: async_channel::Receiver<()>,
    frame_task: RefCell<Option<Task<()>>>,
    accessibility_copy_task: RefCell<Option<CopyPreparationTicket>>,
    find_task: RefCell<Option<Task<()>>>,
    find_navigation_task: RefCell<Option<Task<()>>>,
    displayed: RefCell<Option<(PresentationGeneration, FrameLease, Vec<CellRef>)>>,
    geometry: Cell<Option<TerminalGeometry>>,
    presentation_generation: Cell<PresentationGeneration>,
    frame_requests: Arc<Mutex<FrameRequests>>,
    scrollbar_facts: Arc<Mutex<Option<ScrollbarFacts>>>,
    find_generation: Cell<FindQueryGeneration>,
    pending_paste: RefCell<Option<PendingPaste>>,
    next_paste_confirmation: Cell<u64>,
    paste_confirmation_focused: Cell<bool>,
    selection_drag: Cell<Option<(QueryStamp, ContentSelection)>>,
    accessibility_model: RefCell<Option<crate::terminal::TerminalAccessibilityModel>>,
    accessibility_publication:
        RefCell<Option<crate::platform::terminal_accessibility::AccessibilityPublication>>,
}

impl ServerPaneSession {
    pub(crate) fn bind(
        channel: PaneChannel,
        token: PaneViewToken,
    ) -> Result<Rc<Self>, DesktopClientError> {
        if channel.pane() != token.pane {
            return Err(DesktopClientError::WrongServer);
        }
        if channel.state().borrow().context != token.context_generation {
            return Err(DesktopClientError::StaleContext);
        }
        let (updates, receiver) = async_channel::bounded(1);
        Ok(Rc::new(Self {
            own_client: Cell::new(channel.state().borrow().connection.map(|info| info.client)),
            channel,
            token: Cell::new(token),
            closed: Arc::new(AtomicBool::new(false)),
            enabled: Cell::new(true),
            executor: RefCell::new(None),
            updates,
            receiver,
            frame_task: RefCell::new(None),
            accessibility_copy_task: RefCell::new(None),
            find_task: RefCell::new(None),
            find_navigation_task: RefCell::new(None),
            displayed: RefCell::new(None),
            geometry: Cell::new(None),
            presentation_generation: Cell::new(PresentationGeneration::default()),
            frame_requests: Arc::new(Mutex::new(FrameRequests::default())),
            scrollbar_facts: Arc::new(Mutex::new(None)),
            find_generation: Cell::new(FindQueryGeneration::default()),
            pending_paste: RefCell::new(None),
            next_paste_confirmation: Cell::new(0),
            paste_confirmation_focused: Cell::new(false),
            selection_drag: Cell::new(None),
            accessibility_publication: RefCell::new(None),
            accessibility_model: RefCell::new(None),
        }))
    }
    pub(crate) fn own_client(&self) -> Option<spaceterm_core::identity::ClientId> {
        if let Some(info) = self.channel.state().borrow().connection {
            self.own_client.set(Some(info.client));
        }
        self.own_client.get()
    }
    pub(crate) fn token(&self) -> PaneViewToken {
        self.token.get()
    }
    pub(crate) fn channel(&self) -> &PaneChannel {
        &self.channel
    }
    pub(crate) fn updates(&self) -> async_channel::Receiver<()> {
        self.receiver.clone()
    }
    pub(crate) fn start_content_tasks(&self, executor: BackgroundExecutor) {
        *self.executor.borrow_mut() = Some(executor);
    }
    pub(crate) fn set_context(&self, token: PaneViewToken) -> Result<(), DesktopClientError> {
        if token.pane != self.token.get().pane
            || token.attachment_generation != self.token.get().attachment_generation
        {
            return Err(DesktopClientError::WrongServer);
        }
        if token.context_generation != self.channel.state().borrow().context {
            return Err(DesktopClientError::StaleContext);
        }
        if token != self.token.get() {
            self.cancel_content_tasks();
            self.token.set(token);
        }
        Ok(())
    }
    pub(crate) fn update_context(&self, facts: PaneContextFacts) -> Result<(), DesktopClientError> {
        let context = self
            .channel
            .update_context(facts)
            .map_err(DesktopClientError::from)?;
        let mut token = self.token.get();
        token.context_generation = context;
        self.set_context(token)
    }
    pub(crate) fn set_enabled(&self, enabled: bool) {
        if self.enabled.replace(enabled) && !enabled {
            self.cancel_content_tasks();
        }
    }
    pub(crate) fn native_permit(&self) -> Option<EffectPermit> {
        if self.closed.load(Ordering::Acquire) || !self.enabled.get() {
            return None;
        }
        let displayed = self.displayed.borrow();
        if !displayed.as_ref()?.1.permits_native_content_use() {
            return None;
        }
        self.channel.permit(EffectKind::Read).ok()
    }
    pub(crate) fn accessibility_model(
        &self,
    ) -> Option<crate::terminal::TerminalAccessibilityModel> {
        self.native_permit()?;
        self.accessibility_model.borrow().clone()
    }
    pub(crate) fn accessibility_publication(
        &self,
        generation: PresentationGeneration,
    ) -> Option<crate::platform::terminal_accessibility::AccessibilityPublication> {
        let displayed = self.displayed.borrow();
        let (current, frame, _) = displayed.as_ref()?;
        if generation != *current {
            return None;
        }
        let permit = self.native_permit()?;
        let mut publication = self.accessibility_publication.borrow_mut();
        if let Some(previous) = publication.take() {
            previous.revoke();
        }
        let next = crate::platform::terminal_accessibility::AccessibilityPublication::new(
            permit,
            frame.clone(),
            generation,
        );
        *publication = Some(next.clone());
        Some(next)
    }
    pub(crate) fn accepts_input(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.enabled.get()
            && !self.paste_confirmation_focused.get()
            && self.channel.permit(EffectKind::ImeCommit).is_ok()
    }
    fn effect(&self, kind: EffectKind, effect: RuntimeEffect) {
        if self.closed.load(Ordering::Acquire) || !self.enabled.get()
            || (self.paste_confirmation_focused.get()
                && matches!(kind, EffectKind::Key | EffectKind::ImeCommit | EffectKind::Paste | EffectKind::Pointer)) {
            return;
        }
        if let Ok(permit) = self.channel.permit(kind) {
            let _ = self.channel.try_effect(&permit, effect);
        }
    }
    pub(crate) fn input_method_commit(&self, text: &str) {
        if let Ok(text) =
            spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new(text.to_owned())
        {
            self.effect(
                EffectKind::ImeCommit,
                RuntimeEffect::InputMethodCommit(text),
            );
        }
    }
    fn wake(&self) {
        let _ = self.updates.try_send(());
    }
    pub(crate) fn cancel_content_tasks(&self) {
        if let Some(paste) = self.pending_paste.borrow_mut().take() { paste.cancel(); }
        self.paste_confirmation_focused.set(false);
        if let Some(publication) = self.accessibility_publication.borrow_mut().take() {
            publication.revoke();
        }
        self.frame_task.borrow_mut().take();
        if let Ok(mut requests) = self.frame_requests.lock() {
            *requests = FrameRequests::default();
        }
        self.accessibility_copy_task.borrow_mut().take();
        self.find_task.borrow_mut().take();
        self.find_navigation_task.borrow_mut().take();
        self.displayed.borrow_mut().take();
        self.accessibility_model.borrow_mut().take();
        self.selection_drag.set(None);
        if let Ok(mut facts) = self.scrollbar_facts.lock() {
            *facts = None;
        }
    }
    pub(crate) fn close_presentation(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cancel_content_tasks();
        // Revokes this presentation's permits, but neither releases control nor retires a Session.
        let _ = self.channel.update_context(PaneContextFacts::default());
        self.updates.close();
    }
    fn now(&self) -> Instant {
        self.executor.borrow().as_ref().map_or_else(Instant::now, BackgroundExecutor::now)
    }
    fn expire_paste(&self) {
        let expired = self.pending_paste.borrow().as_ref().is_some_and(|paste| !paste.is_current(self.now()));
        if expired && let Some(paste) = self.pending_paste.borrow_mut().take() { paste.cancel(); }
    }
    pub(crate) fn paste_confirmation_is_current(&self, id: crate::terminal::paste::PasteConfirmationId) -> bool {
        self.expire_paste();
        matches!(&*self.pending_paste.borrow(), Some(PendingPaste::Approval { confirmation, .. }) if confirmation.id == id)
    }
    pub(crate) fn permits_native_request(&self) -> bool {
        !self.paste_confirmation_focused.get() && self.native_permit().is_some()
    }
    fn paste_deadline<T: Send + 'static>(
        &self,
        executor: &BackgroundExecutor,
        permit: &EffectPermit,
        expires: Instant,
        reply: async_channel::Sender<Result<T, String>>,
        fallback: T,
    ) -> Task<()> {
        let mut state = self.channel.state();
        let permit = permit.clone();
        let timer = executor.timer(expires.saturating_duration_since(executor.now()));
        let updates = self.updates.clone();
        executor.spawn(async move {
            wait_for_paste_invalidation(&permit, &mut state, timer).await;
            let _ = reply.try_send(Ok(fallback));
            let _ = updates.try_send(());
        })
    }
    /// One subscriber routes non-native events here. Queue admission never completes a paste.
    pub(crate) fn accept_event(&self, event: PaneEvent) {
        self.expire_paste();
        match event {
            PaneEvent::Invalidated => {
                if let Some(paste) = self.pending_paste.borrow_mut().take() { paste.cancel(); }
            }
            PaneEvent::PasteWritten { correlation, result } => {
                let mut pending = self.pending_paste.borrow_mut();
                if !matches!(&*pending, Some(PendingPaste::Request { correlation: current, .. }) if *current == correlation) { return; }
                if let Some(PendingPaste::Request { reply, .. }) = pending.take() {
                    let _ = reply.try_send(result.map(|()| PasteRequestOutcome::Written).map_err(|error| error.to_string()));
                }
            }
            PaneEvent::PasteApproval { correlation, request, timing } => {
                let mut pending = self.pending_paste.borrow_mut();
                if !matches!(&*pending, Some(PendingPaste::Request { correlation: current, .. }) if *current == correlation)
                    || !request.is_current() || !self.enabled.get() || self.closed.load(Ordering::Acquire) { return; }
                let NativeRequestKind::UnsafePaste { text, .. } = request.kind() else { return; };
                let Some(id) = self.next_paste_confirmation.get().checked_add(1) else { return; };
                self.next_paste_confirmation.set(id);
                let text = text.as_str();
                let confirmation = PasteConfirmation {
                    id: crate::terminal::paste::PasteConfirmationId::new(id),
                    byte_len: text.len(),
                    line_count: text.bytes().filter(|byte| *byte == b'\n').count() + 1,
                    risk: crate::terminal::PasteRisk {
                        multiline: text.contains('\n'),
                        control_bytes: text.chars().any(|character| character.is_control() && !matches!(character, '\n' | '\t')),
                        closing_fence: text.contains("\x1b[201~"),
                    },
                };
                let Some(executor) = self.executor.borrow().clone() else { return; };
                let Some(PendingPaste::Request { reply, expires: original, .. }) = pending.take() else { return; };
                let expires = original.min(executor.now() + timing.expires_after.unwrap_or_default().min(Duration::from_secs(30)));
                let updates = self.updates.clone();
                let timer = executor.timer(expires.saturating_duration_since(executor.now()));
                let deadline = executor.spawn(async move { timer.await; let _ = updates.try_send(()); });
                *pending = Some(PendingPaste::Approval { request, confirmation, expires, _deadline: deadline });
                let _ = reply.try_send(Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)));
            }
            PaneEvent::PasteResolved { correlation, decision: native_decision, result, .. } => {
                let mut pending = self.pending_paste.borrow_mut();
                if !matches!(&*pending, Some(PendingPaste::Resolution { correlation: current, .. }) if *current == correlation) { return; }
                if let Some(PendingPaste::Resolution { reply, decision, .. }) = pending.take() {
                    let cancelled = decision == PasteDecision::Cancel && native_decision == spaceterm_core::native_requests::PasteDecision::Cancel;
                    let result = match result {
                        Ok(()) => Ok(if cancelled { PasteResolution::Cancelled } else { PasteResolution::Written }),
                        Err(error) if cancelled && error.class() == ClientFailureClass::Cancelled => Ok(PasteResolution::Cancelled),
                        Err(error) if matches!(error.class(), ClientFailureClass::StaleAuthority | ClientFailureClass::Cancelled) => Ok(PasteResolution::Stale),
                        Err(error) => Err(error.to_string()),
                    };
                    let _ = reply.try_send(result);
                }
            }
            PaneEvent::Metadata { .. } => self.wake(),
            PaneEvent::Native { .. } => {}
        }
    }

    pub(crate) fn request_frame(&self) -> Result<(), DesktopClientError> {
        if !self.enabled.get() {
            return Err(DesktopClientError::StaleContext);
        }
        let executor = self
            .executor
            .borrow()
            .clone()
            .ok_or(DesktopClientError::UnsupportedCapability)?;
        let permit = self
            .channel
            .permit(EffectKind::Read)
            .map_err(DesktopClientError::from)?;
        let stamp = self
            .channel
            .query_stamp(&permit, RequestGeneration::new(0))
            .map_err(DesktopClientError::from)?;
        let viewport = self
            .channel
            .projection()
            .lock()
            .map_err(|_| DesktopClientError::StaleContext)?
            .viewport();
        let identity = (permit.fence(), stamp.base(), viewport);
        {
            let mut requests = self
                .frame_requests
                .lock()
                .map_err(|_| DesktopClientError::StaleContext)?;
            if requests.pending == Some(identity)
                || (requests.installed == Some(identity)
                    && self.channel.current_frame().is_some_and(|frame| {
                        frame
                            .frame()
                            .is_some_and(|frame| frame.base() == stamp.base())
                    }))
                || requests.failed.is_some_and(|(failed, retry_after)| {
                    failed == identity && executor.now() < retry_after
                })
            {
                return Ok(());
            }
            requests.pending = Some(identity);
            requests.failed = None;
        }
        let channel = self.channel.clone();
        let closed = Arc::clone(&self.closed);
        let updates = self.updates.clone();
        let requests = Arc::clone(&self.frame_requests);
        let scrollbar = Arc::clone(&self.scrollbar_facts);
        let clock = executor.clone();
        *self.frame_task.borrow_mut() = Some(executor.spawn(async move {
            let mut result = Err(DesktopClientError::StaleContext);
            let mut stamp = stamp;
            for attempt in 0..3 {
                if closed.load(Ordering::Acquire) || !permit.is_current() {
                    break;
                }
                if attempt != 0 {
                    // Restart partial staging under a fresh read identity, never a new permit.
                    match channel.query_stamp(&permit, RequestGeneration::new(0)) {
                        Ok(next) => stamp = next,
                        Err(error) => {
                            result = Err(DesktopClientError::from(error));
                            break;
                        }
                    }
                }
                result = read_frame(&channel, &permit, stamp, viewport, &scrollbar, &updates).await;
                if result != Err(DesktopClientError::LimitReached) || attempt == 2 {
                    break;
                }
                // Transfer pressure gets two bounded retries. Persistent failures wait for a
                // later repaint or publication, with a cooldown instead of an update loop.
                clock.timer(Duration::from_millis(100 << attempt)).await;
            }
            if let Ok(mut requests) = requests.lock()
                && requests.pending == Some(identity)
            {
                requests.pending = None;
                if result.is_ok() && permit.is_current() && !closed.load(Ordering::Acquire) {
                    requests.installed = Some(identity);
                } else {
                    requests.failed = Some((identity, clock.now() + Duration::from_secs(1)));
                }
            }
            if !closed.load(Ordering::Acquire) {
                let _ = updates.try_send(());
            }
        }));
        Ok(())
    }
    fn request_scroll(&self, target: ScrollTarget) {
        if !self.enabled.get() {
            return;
        }
        let Some(executor) = self.executor.borrow().clone() else {
            return;
        };
        let Ok(permit) = self.channel.permit(EffectKind::Read) else {
            return;
        };
        let Ok(stamp) = self.channel.query_stamp(&permit, RequestGeneration::new(0)) else {
            return;
        };
        let channel = self.channel.clone();
        let updates = self.updates.clone();
        let closed = Arc::clone(&self.closed);
        let visible = self
            .geometry
            .get()
            .map_or(1, |geometry| geometry.grid().rows);
        if let Ok(mut requests) = self.frame_requests.lock() {
            *requests = FrameRequests::default();
        }
        let scrollbar = Arc::clone(&self.scrollbar_facts);
        *self.frame_task.borrow_mut() = Some(executor.spawn(async move {
            let Ok(rows) = topology_rows(&channel, &permit, stamp).await else {
                return;
            };
            if rows.is_empty() || closed.load(Ordering::Acquire) || !permit.is_current() {
                return;
            }
            let bottom = rows.len().saturating_sub(usize::from(visible));
            let anchor = channel
                .projection()
                .lock()
                .ok()
                .and_then(|projection| projection.viewport().anchor);
            let current = anchor
                .and_then(|anchor| rows.iter().position(|row| row.row() == anchor))
                .unwrap_or(bottom);
            let first = match target {
                ScrollTarget::Top => 0,
                ScrollTarget::Bottom => bottom,
                ScrollTarget::Offset(offset) => {
                    bottom.saturating_sub(usize::try_from(offset).unwrap_or(usize::MAX))
                }
                ScrollTarget::Relative(delta) => {
                    if delta < 0 {
                        current.saturating_sub(delta.unsigned_abs() as usize)
                    } else {
                        current.saturating_add(delta as usize).min(bottom)
                    }
                }
            };
            let last = (first + usize::from(visible).saturating_sub(1)).min(rows.len() - 1);
            let viewport = Viewport {
                anchor: (first != bottom).then_some(rows[first].row()),
                rows: visible,
            };
            if channel
                .projection()
                .lock()
                .ok()
                .and_then(|mut projection| projection.set_viewport(viewport).ok())
                .is_none()
            {
                return;
            }
            if let Ok(mut facts) = scrollbar.lock() {
                *facts = Some(ScrollbarFacts {
                    base: stamp.base(),
                    viewport,
                    total_rows: rows.len() as u64,
                    first_row: Some(first as u64),
                });
            }
            let range = ContentRange::History {
                first: Some(rows[first].row()),
                last: Some(rows[last].row()),
            };
            if read_pages(&channel, &permit, stamp, range).await.is_ok()
                && !closed.load(Ordering::Acquire)
            {
                let _ = updates.try_send(());
            }
        }));
    }

    pub(crate) fn prepare_copy(
        &self,
        request: CopyPreparationRequest,
    ) -> Result<CopyPreparationTicket, DesktopClientError> {
        if !self.enabled.get() {
            return Err(DesktopClientError::StaleContext);
        }
        let executor = self
            .executor
            .borrow()
            .clone()
            .ok_or(DesktopClientError::UnsupportedCapability)?;
        let permit = self
            .channel
            .permit(EffectKind::Read)
            .map_err(DesktopClientError::from)?;
        if permit.fence() != request.stamp().fence() {
            return Err(DesktopClientError::StaleContext);
        }
        let mut preparation = self
            .channel
            .projection()
            .lock()
            .map_err(|_| DesktopClientError::StaleContext)?
            .begin_copy(request)
            .map_err(DesktopClientError::from)?;
        self.accessibility_copy_task.borrow_mut().take();
        let channel = self.channel.clone();
        let closed = Arc::clone(&self.closed);
        let updates = self.updates.clone();
        Ok(executor.spawn(async move {
            let mut stream = channel
                .read(&permit, PaneReadRequest::Copy(request))
                .await
                .map_err(DesktopClientError::from)?;
            while let Some(item) = stream.next().await.map_err(DesktopClientError::from)? {
                if closed.load(Ordering::Acquire) || !permit.is_current() {
                    return Err(DesktopClientError::StaleContext);
                }
                let PaneReadItem::Copy(item) = item else {
                    return Err(DesktopClientError::StaleContext);
                };
                if let Some(lease) = preparation.accept(item).map_err(DesktopClientError::from)? {
                    channel
                        .projection()
                        .lock()
                        .map_err(|_| DesktopClientError::StaleContext)?
                        .install_copy(lease.clone())
                        .map_err(DesktopClientError::from)?;
                    let _ = updates.try_send(());
                    return Ok(lease);
                }
            }
            Err(DesktopClientError::StaleContext)
        }))
    }
    pub(crate) fn copy_request(
        &self,
    ) -> Result<Option<CopyPreparationRequest>, DesktopClientError> {
        let permit = self
            .channel
            .permit(EffectKind::Read)
            .map_err(DesktopClientError::from)?;
        let selection = self
            .channel
            .projection()
            .lock()
            .map_err(|_| DesktopClientError::StaleContext)?
            .selection();
        let Some(selection) = selection else {
            return Ok(None);
        };
        let stamp = self
            .channel
            .query_stamp(&permit, RequestGeneration::new(0))
            .map_err(DesktopClientError::from)?;
        CopyRequest::new(
            stamp,
            selection,
            CopyOptions {
                unwrap_soft_wraps: true,
                trim_trailing_spaces: true,
                include_html: true,
            },
        )
        .map(Some)
        .map_err(|_| DesktopClientError::StaleContext)
    }
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "The frozen native facade also exposes a cache-only optional read"
        )
    )]
    pub(crate) fn prepared_copy(
        &self,
        generation: PresentationGeneration,
    ) -> Option<SelectionCopy> {
        self.prepared_copy_result(generation).ok().flatten()
    }
    pub(crate) fn has_prepared_copy(&self) -> bool {
        self.native_permit().is_some()
            && self
                .channel
                .projection()
                .lock()
                .ok()
                .and_then(|projection| projection.prepared_copy())
                .is_some()
    }

    fn prepared_copy_result(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        let displayed = self.displayed.borrow();
        let Some((current, frame, _)) = displayed.as_ref() else {
            return Ok(None);
        };
        if *current != generation
            || !frame.permits_native_content_use()
            || self.closed.load(Ordering::Acquire)
            || !self.enabled.get()
        {
            return Ok(None);
        }
        let Some(lease) = self
            .channel
            .projection()
            .lock()
            .ok()
            .and_then(|projection| projection.prepared_copy())
        else {
            return Ok(None);
        };
        selection_copy_from_lease(&lease)
    }
    fn cell(
        &self,
        generation: PresentationGeneration,
        position: crate::terminal::SurfacePosition,
    ) -> Option<CellRef> {
        let displayed = self.displayed.borrow();
        let (current, frame, rows) = displayed.as_ref()?;
        if !self.enabled.get()
            || *current != generation
            || !frame.permits_native_content_use()
            || !position.x.is_finite()
            || !position.y.is_finite()
        {
            return None;
        }
        let geometry = self.geometry.get()?;
        let cell = geometry.logical_cell_size();
        let row = (position.y.max(0.0) / cell.height).floor() as usize;
        let column = (position.x.max(0.0) / cell.width).floor() as u16;
        let mut reference = *rows.get(row.min(rows.len().checked_sub(1)?))?;
        let columns = frame.frame()?.pages().first()?.page().parts().columns;
        reference.column = column.min(columns.saturating_sub(1));
        Some(reference)
    }
    fn pointer_input(&self, input: PointerInput) {
        if !self.enabled.get() {
            return;
        }
        let Some(cell) = self.cell(input.generation, input.position) else {
            return;
        };
        let Some(frame) = self.channel.current_frame() else {
            return;
        };
        let application = frame.frame().is_some_and(|frame| {
            frame.pages().iter().any(|page| {
                page.page().parts().modes.mouse_tracking != content::MouseTracking::None
            })
        }) && !(input.modifiers.shift
            && input.shift_selection == ShiftSelectionPolicy::OverrideApplicationMouse);
        let Ok(permit) = self.channel.permit(EffectKind::Read) else {
            return;
        };
        let Ok(stamp) = self.channel.query_stamp(&permit, RequestGeneration::new(0)) else {
            return;
        };
        if application {
            self.effect(
                EffectKind::Pointer,
                RuntimeEffect::Pointer(ApplicationPointerInput {
                    stamp,
                    cell,
                    phase: input.phase,
                    button: input.button,
                    modifiers: input.modifiers,
                }),
            );
            return;
        }
        if input.button != Some(PointerButton::Left) {
            return;
        }
        let start = match input.phase {
            PointerPhase::Press => cell,
            PointerPhase::Motion | PointerPhase::Release => {
                let drag = if input.phase == PointerPhase::Release {
                    self.selection_drag.take()
                } else {
                    self.selection_drag.get()
                };
                let Some((_, drag)) = drag.filter(|(started, _)| {
                    started.fence() == stamp.fence() && started.base() == stamp.base()
                }) else {
                    return;
                };
                drag.start()
            }
        };
        let projection = self.channel.projection();
        let Ok(mut projection) = projection.lock() else {
            return;
        };
        let Ok(selection) = ContentSelection::new(stamp, start, cell, input.modifiers.alt) else {
            return;
        };
        if projection.set_selection(Some(selection)).is_err() {
            return;
        }
        drop(projection);
        if input.phase == PointerPhase::Press {
            self.selection_drag.set(Some((stamp, selection)));
        }
        self.wake();
    }
    pub(crate) fn snapshot(
        &self,
        appearance: &TerminalAppearanceUpdate,
    ) -> Option<Arc<ScreenSnapshot>> {
        if !self.enabled.get() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        let frame = self.channel.current_frame()?;
        let semantic = frame.frame()?;
        let projection = self.channel.projection();
        let projection = projection.lock().ok()?;
        let selection = projection.selection();
        let mut screen = (*ScreenSnapshot::empty_with_appearance(
            spaceterm_terminal::local_path::LocalPathSemantics::Posix,
            appearance,
        ))
        .clone();
        if let Some(publication) = self.accessibility_publication.borrow_mut().take() {
            publication.revoke();
        }
        let generation = self.presentation_generation.get().next();
        self.presentation_generation.set(generation);
        let mut rows: Vec<crate::terminal::RowSnapshot> = Vec::new();
        let mut references = Vec::new();
        let mut soft_wraps = Vec::new();
        for page in semantic.pages() {
            let parts = page.page().parts();
            screen.cursor.password_input = parts.modes.hidden_input;
            apply_program_colors(&mut screen, &parts.colors, parts.modes.reverse_colors);
            screen.mouse_tracking = parts.modes.mouse_tracking != content::MouseTracking::None;
            for row in &parts.rows {
                let reference = CellRef {
                    session: semantic.base().session,
                    row: row.id(),
                    row_revision: row.revision(),
                    column: 0,
                };
                if row.start_column() == 0 {
                    references.push(reference);
                    soft_wraps.push(row.soft_wrapped());
                    rows.push(Arc::from([]));
                }
                let index = rows.len().checked_sub(1)?;
                let mut cells = rows[index].to_vec();
                cells.extend(row.cells().iter().enumerate().map(|(offset, cell)| {
                    snapshot_cell(
                        cell,
                        selection.is_some_and(|selection| {
                            selection_contains(
                                selection,
                                &references,
                                index,
                                usize::from(row.start_column()) + offset,
                            )
                        }),
                        cell.hyperlink()
                            .and_then(|id| {
                                parts
                                    .hyperlinks
                                    .iter()
                                    .find(|descriptor| descriptor.id() == id)
                            })
                            .and_then(|descriptor| {
                                crate::terminal::HyperlinkTarget::url(descriptor.target())
                            })
                            .map(Arc::new),
                    )
                }));
                rows[index] = Arc::from(cells);
            }
            screen.size.cols = parts.columns;
            if let Some(cursor) = parts.cursor
                && let Some(row) = references
                    .iter()
                    .position(|reference| reference.row == cursor.row)
            {
                screen.cursor.position = Some(crate::terminal::CursorPositionSnapshot {
                    row: row as u16,
                    column: cursor.column,
                    width_cells: 1,
                });
                screen.cursor.visible = cursor.visible;
                screen.cursor.blinking = cursor.blinking;
                screen.cursor.shape = match cursor.shape {
                    content::CursorShape::Block => crate::terminal::CursorShapeSnapshot::Block,
                    content::CursorShape::Bar => crate::terminal::CursorShapeSnapshot::Bar,
                    content::CursorShape::Underline => {
                        crate::terminal::CursorShapeSnapshot::Underline
                    }
                };
            }
        }
        screen.generation = generation;
        screen.rows = Arc::from(rows);
        screen.row_soft_wrapped = Arc::from(soft_wraps);
        screen.size.rows = screen.rows.len() as u16;
        if let Some(selection) = selection {
            for (index, row) in Arc::make_mut(&mut screen.rows).iter_mut().enumerate() {
                let mut cells = row.to_vec();
                for (column, cell) in cells.iter_mut().enumerate() {
                    cell.selected = selection_contains(selection, &references, index, column);
                }
                *row = Arc::from(cells);
            }
        }
        screen.selection_present = selection.is_some();
        screen.active_screen = match semantic.base().screen {
            content::Screen::Primary => crate::terminal::ActiveScreenSnapshot::Primary,
            content::Screen::Alternate => crate::terminal::ActiveScreenSnapshot::Alternate,
        };
        screen.scrollbar.visible_rows = screen.rows.len() as u64;
        screen.scrollbar.total_rows = screen.rows.len() as u64;
        if let Some(facts) = self
            .scrollbar_facts
            .lock()
            .ok()
            .and_then(|facts| *facts)
            .filter(|facts| {
                facts.base == semantic.base() && facts.viewport == projection.viewport()
            })
        {
            screen.scrollbar.total_rows = facts.total_rows.max(screen.scrollbar.visible_rows);
            screen.scrollbar.offset_rows = facts.first_row.map_or(0, |first| {
                facts
                    .total_rows
                    .saturating_sub(screen.scrollbar.visible_rows)
                    .saturating_sub(first)
            });
        }
        screen.damage.content = spaceterm_terminal::snapshot::ContentDamageSnapshot::Full;
        drop(projection);
        if !frame.is_paintable() {
            return None;
        }
        *self.accessibility_model.borrow_mut() = Some(
            crate::terminal::TerminalAccessibilityModel::from_screen(&screen),
        );
        *self.displayed.borrow_mut() = Some((generation, frame, references));
        Some(Arc::new(screen))
    }
}

/// A rendered element retains data, not permission to paint it or register its input handler.
/// Recheck at both prepaint and final paint, including renderer fallback frames.
pub(crate) struct FencedPaneContent<E> {
    child: E,
    frame: Option<FrameLease>,
    fallback: Option<FrameLease>,
    presentable: Rc<dyn Fn(&gpui::App) -> bool>,
}
impl<E> FencedPaneContent<E> {
    pub(crate) fn new(
        child: E,
        frame: Option<FrameLease>,
        fallback: Option<FrameLease>,
        presentable: impl Fn(&gpui::App) -> bool + 'static,
    ) -> Self {
        Self {
            child,
            frame,
            fallback,
            presentable: Rc::new(presentable),
        }
    }
    fn is_paintable(&self, cx: &gpui::App) -> bool {
        (self.presentable)(cx)
            && self.frame.as_ref().is_some_and(FrameLease::is_paintable)
            && self.fallback.as_ref().is_none_or(FrameLease::is_paintable)
    }
}
impl<E: gpui::Element> gpui::IntoElement for FencedPaneContent<E> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl<E: gpui::Element> gpui::Element for FencedPaneContent<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = Option<E::PrepaintState>;
    fn id(&self) -> Option<gpui::ElementId> {
        self.child.id()
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.child.source_location()
    }
    fn request_layout(
        &mut self,
        id: Option<&gpui::GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        self.child.request_layout(id, inspector, window, cx)
    }
    fn prepaint(
        &mut self,
        id: Option<&gpui::GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        self.is_paintable(cx).then(|| {
            self.child
                .prepaint(id, inspector, bounds, layout, window, cx)
        })
    }
    fn paint(
        &mut self,
        id: Option<&gpui::GlobalElementId>,
        inspector: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        if self.is_paintable(cx)
            && let Some(prepaint) = prepaint
        {
            self.child
                .paint(id, inspector, bounds, layout, prepaint, window, cx);
        }
    }
}

async fn wait_for_paste_invalidation(
    permit: &EffectPermit,
    state: &mut tokio::sync::watch::Receiver<spaceterm_client::pane_channel::PaneChannelState>,
    timer: impl std::future::Future<Output = ()>,
) {
    let mut timer = std::pin::pin!(timer);
    while permit.is_current() {
        let changed = state.changed();
        let mut changed = std::pin::pin!(changed);
        let invalidated = std::future::poll_fn(|cx| {
            if timer.as_mut().poll(cx).is_ready() { return std::task::Poll::Ready(true); }
            changed.as_mut().poll(cx).map(|result| result.is_err())
        }).await;
        if invalidated { return; }
    }
}

#[derive(Clone, Copy)]
enum ScrollTarget {
    Top,
    Bottom,
    Relative(i64),
    Offset(u64),
}

async fn read_frame(
    channel: &PaneChannel,
    permit: &EffectPermit,
    stamp: QueryStamp,
    viewport: Viewport,
    scrollbar: &Mutex<Option<ScrollbarFacts>>,
    updates: &async_channel::Sender<()>,
) -> Result<(), DesktopClientError> {
    let mut topology = None;
    let mut first_row = None;
    let range = if let Some(anchor) = viewport.anchor {
        let rows = topology_rows(channel, permit, stamp).await?;
        let first = rows
            .iter()
            .position(|row| row.row() == anchor)
            .ok_or(DesktopClientError::StaleContext)?;
        let last = (first + usize::from(viewport.rows).saturating_sub(1))
            .min(rows.len().saturating_sub(1));
        let range = ContentRange::History {
            first: rows.get(first).map(|row| row.row()),
            last: rows.get(last).map(|row| row.row()),
        };
        first_row = Some(first as u64);
        topology = Some(rows);
        range
    } else {
        ContentRange::ActiveGrid
    };
    read_pages(channel, permit, stamp, range).await?;
    // Paint need not wait for the history inventory. Its separately qualified totals replace
    // the conservative visible-row count once every topology page has arrived.
    let _ = updates.try_send(());
    let rows = match topology {
        Some(rows) => rows,
        None => match topology_rows(channel, permit, stamp).await {
            Ok(rows) => rows,
            Err(_) => return Ok(()),
        },
    };
    if permit.is_current() {
        *scrollbar
            .lock()
            .map_err(|_| DesktopClientError::StaleContext)? = Some(ScrollbarFacts {
            base: stamp.base(),
            viewport,
            total_rows: rows.len() as u64,
            first_row,
        });
    }
    Ok(())
}

async fn topology_rows(
    channel: &PaneChannel,
    permit: &EffectPermit,
    stamp: QueryStamp,
) -> Result<Vec<TopologyRow>, DesktopClientError> {
    let limits =
        PageLimits::new(256, 16_384, 1024 * 1024).map_err(|_| DesktopClientError::AdmissionFull)?;
    let mut rows = Vec::new();
    let mut continuation = None;
    loop {
        let request = TopologyPageRequest::new(stamp, continuation, limits)
            .map_err(|_| DesktopClientError::StaleContext)?;
        let mut stream = channel
            .read(permit, PaneReadRequest::Topology(request))
            .await
            .map_err(DesktopClientError::from)?;
        let Some(PaneReadItem::Topology(ContentReadResult::Ready(page))) =
            stream.next().await.map_err(DesktopClientError::from)?
        else {
            return Err(DesktopClientError::StaleContext);
        };
        if rows.len().saturating_add(page.rows().len())
            > SUPPORTED_SCROLLBACK_ROWS as usize + SUPPORTED_GRID_LIMITS.max_rows() as usize
        {
            return Err(DesktopClientError::AdmissionFull);
        }
        rows.extend_from_slice(page.rows());
        continuation = page.continuation();
        if continuation.is_none() {
            return Ok(rows);
        }
    }
}
async fn read_pages(
    channel: &PaneChannel,
    permit: &EffectPermit,
    stamp: QueryStamp,
    range: ContentRange,
) -> Result<(), DesktopClientError> {
    let limits =
        PageLimits::new(256, 16_384, 1024 * 1024).map_err(|_| DesktopClientError::AdmissionFull)?;
    let mut continuation = None;
    loop {
        let request = ContentPageRequest::new(stamp, range, continuation, limits)
            .map_err(|_| DesktopClientError::StaleContext)?;
        let mut stream = channel
            .read(permit, PaneReadRequest::Page(request))
            .await
            .map_err(DesktopClientError::from)?;
        let Some(PaneReadItem::Page(ContentReadResult::Ready(reply))) =
            stream.next().await.map_err(DesktopClientError::from)?
        else {
            return Err(DesktopClientError::StaleContext);
        };
        continuation = reply
            .continuation()
            .map_err(|_| DesktopClientError::StaleContext)?;
        let accepted = channel
            .projection()
            .lock()
            .map_err(|_| DesktopClientError::StaleContext)?
            .accept_page(stamp, reply.page().clone())
            .map_err(DesktopClientError::from)?;
        if accepted {
            return Ok(());
        }
        if continuation.is_none() {
            return Err(DesktopClientError::StaleContext);
        }
    }
}
fn terminal_palette(colors: &crate::appearance::TerminalColors) -> [crate::appearance::Color; 256] {
    let mut palette = [colors.foreground; 256];
    palette[..8].copy_from_slice(&colors.normal);
    palette[8..16].copy_from_slice(&colors.bright);
    let cube = [0, 95, 135, 175, 215, 255];
    for index in 0..216 {
        palette[index + 16] = crate::appearance::Color::from_rgb_components(
            cube[index / 36],
            cube[(index / 6) % 6],
            cube[index % 6],
        );
    }
    for index in 0..24 {
        let component = (8 + index * 10) as u8;
        palette[index + 232] =
            crate::appearance::Color::from_rgb_components(component, component, component);
    }
    palette
}
fn apply_program_colors(
    screen: &mut ScreenSnapshot,
    colors: &content::ProgramColorOverrides,
    reverse: bool,
) {
    let rgb =
        |rgb: content::Rgb| crate::appearance::Color::from_rgb_components(rgb.r, rgb.g, rgb.b);
    use spaceterm_terminal::snapshot::TerminalDefaultColorSource;
    if let Some(color) = colors.foreground {
        screen.colors.foreground = rgb(color);
        screen.colors.foreground_source = TerminalDefaultColorSource::ProgramOverride;
    }
    if let Some(color) = colors.background {
        screen.colors.background = rgb(color);
        screen.colors.background_source = TerminalDefaultColorSource::ProgramOverride;
    }
    if let Some(color) = colors.cursor {
        screen.cursor.color = rgb(color);
        screen.colors.cursor_source = TerminalDefaultColorSource::ProgramOverride;
    }
    let mut palette = terminal_palette(screen.configured_colors.as_ref());
    let mut overrides = [false; 256];
    for entry in &colors.palette {
        palette[usize::from(entry.index)] = rgb(entry.color);
        overrides[usize::from(entry.index)] = true;
    }
    screen.colors.palette = Arc::new(palette);
    screen.colors.palette_overrides = Arc::new(overrides);
    screen.colors.reversed = reverse;
    screen.background = screen.colors.effective_background();
}

pub(crate) fn selection_copy_from_lease(
    lease: &PreparedCopyLease,
) -> Result<Option<SelectionCopy>, SelectionCopyError> {
    fn owned_text(text: &str) -> Result<String, SelectionCopyError> {
        let mut result = String::new();
        result
            .try_reserve_exact(text.len())
            .map_err(|_| SelectionCopyError::Formatting)?;
        result.push_str(text);
        Ok(result)
    }
    let Some(plain) = lease.plain() else {
        return Ok(None);
    };
    let copy = SelectionCopy {
        plain_text: owned_text(plain)?,
        html: lease.html().map(owned_text).transpose()?,
    };
    Ok(lease.is_current().then_some(copy))
}

fn snapshot_cell(
    cell: &content::StyledCell,
    selected: bool,
    hyperlink: Option<Arc<crate::terminal::HyperlinkTarget>>,
) -> crate::terminal::CellSnapshot {
    let style = cell.style();
    crate::terminal::CellSnapshot {
        text: cell.content().text().to_owned(),
        foreground_source: snapshot_color(style.foreground),
        background_source: snapshot_color(style.background),
        inverse: style.inverse,
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        blinking: style.blinking,
        invisible: style.invisible,
        underline: match style.underline {
            content::Underline::None => crate::terminal::TerminalUnderlineSnapshot::None,
            content::Underline::Single => crate::terminal::TerminalUnderlineSnapshot::Single,
            content::Underline::Double => crate::terminal::TerminalUnderlineSnapshot::Double,
            content::Underline::Curly => crate::terminal::TerminalUnderlineSnapshot::Curly,
            content::Underline::Dotted => crate::terminal::TerminalUnderlineSnapshot::Dotted,
            content::Underline::Dashed => crate::terminal::TerminalUnderlineSnapshot::Dashed,
        },
        underline_source: snapshot_color(style.underline_color),
        strikethrough: style.strikethrough,
        overline: style.overline,
        selected,
        spacer_tail: cell.content().width() == content::CellWidth::SpacerTail,
        semantic_content: match style.semantic {
            content::SemanticContent::Output => crate::terminal::CellSemanticSnapshot::Output,
            content::SemanticContent::Input => crate::terminal::CellSemanticSnapshot::Input,
            content::SemanticContent::Prompt => crate::terminal::CellSemanticSnapshot::Prompt,
        },
        // A server hyperlink descriptor never carries client-local file authority.
        hyperlink,
    }
}
fn snapshot_color(color: content::TerminalColor) -> crate::terminal::TerminalColor {
    match color {
        content::TerminalColor::Default => crate::terminal::TerminalColor::Default,
        content::TerminalColor::Palette(index) => crate::terminal::TerminalColor::Palette(index),
        content::TerminalColor::Rgb(rgb) => crate::terminal::TerminalColor::Rgb(
            crate::appearance::Color::from_rgb_components(rgb.r, rgb.g, rgb.b),
        ),
    }
}
fn accessibility_cell(
    model: &crate::terminal::TerminalAccessibilityModel,
    references: &[CellRef],
    index: usize,
) -> Option<CellRef> {
    let line = model.line_for_index(index)?;
    let row = model.row(line)?;
    let local = index.checked_sub(row.range.start)?;
    let cell = row.cells().find(|cell| cell.utf16.contains(&local))?;
    let mut reference = *references.get(line)?;
    reference.column = cell.columns.start;
    Some(reference)
}

fn selection_contains(
    selection: ContentSelection,
    rows: &[CellRef],
    row: usize,
    column: usize,
) -> bool {
    let Some(start) = rows
        .iter()
        .position(|reference| reference.row == selection.start().row)
    else {
        return false;
    };
    let Some(end) = rows
        .iter()
        .position(|reference| reference.row == selection.end().row)
    else {
        return false;
    };
    let (start, end) = (
        (start, usize::from(selection.start().column)),
        (end, usize::from(selection.end().column)),
    );
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if selection.rectangle() {
        row >= start.0
            && row <= end.0
            && column >= start.1.min(end.1)
            && column <= start.1.max(end.1)
    } else {
        (row, column) >= start && (row, column) <= end
    }
}

impl TerminalSessionHandle for Rc<ServerPaneSession> {
    fn key(&self, input: KeyInput) {
        self.effect(EffectKind::Key, RuntimeEffect::Key(input));
    }
    fn clear_screen_and_scrollback(&self) {
        self.effect(EffectKind::Key, RuntimeEffect::ClearScreenAndScrollback);
    }
    fn focus(&self, focused: bool) {
        if focused { self.paste_confirmation_focused.set(false); }
        self.effect(EffectKind::FocusReport, RuntimeEffect::FocusReport(focused));
    }
    fn resize(&self, geometry: TerminalGeometry) {
        self.geometry.set(Some(geometry));
        if let Ok(mut projection) = self.channel.projection().lock() {
            let viewport = projection.viewport();
            let _ = projection.set_viewport(Viewport {
                rows: geometry.grid().rows,
                ..viewport
            });
        }
        let cell = geometry.backing_cell_size();
        let grid = geometry.grid();
        let measured = u16::try_from(cell.width)
            .ok()
            .zip(u16::try_from(cell.height).ok())
            .and_then(|(width, height)| CellPixels::new(width, height).ok())
            .and_then(|pixels| {
                MeasuredGrid::new(
                    u32::from(grid.rows),
                    u32::from(grid.cols),
                    pixels,
                    SUPPORTED_GRID_LIMITS,
                )
                .ok()
            });
        if let Some(grid) = measured {
            self.effect(EffectKind::Resize, RuntimeEffect::Resize(grid));
        }
    }
    fn pointer(&self, input: PointerInput) {
        self.pointer_input(input);
    }
    fn pointer_and_copy_selection(
        &self,
        input: PointerInput,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.pointer_input(input);
        Ok(None)
    }
    fn wheel(&self, input: WheelInput) {
        let Some(cell) = self.cell(input.generation, input.position) else {
            return;
        };
        let application = self
            .channel
            .current_frame()
            .and_then(|frame| {
                frame.frame().map(|frame| {
                    frame.pages().iter().any(|page| {
                        page.page().parts().modes.mouse_tracking != content::MouseTracking::None
                    })
                })
            })
            .unwrap_or(false)
            && !(input.modifiers.shift
                && input.shift_selection == ShiftSelectionPolicy::OverrideApplicationMouse);
        if application {
            let Ok(permit) = self.channel.permit(EffectKind::Read) else {
                return;
            };
            let Ok(stamp) = self.channel.query_stamp(&permit, RequestGeneration::new(0)) else {
                return;
            };
            self.effect(
                EffectKind::Pointer,
                RuntimeEffect::Wheel(ApplicationWheelInput {
                    stamp,
                    cell,
                    horizontal_steps: input.horizontal_steps,
                    vertical_steps: input.vertical_steps,
                    modifiers: input.modifiers,
                }),
            );
        } else {
            self.request_scroll(ScrollTarget::Relative(-i64::from(input.vertical_steps)));
        }
    }
    fn scroll_to(&self, offset_rows: u64, generation: PresentationGeneration) {
        if self
            .displayed
            .borrow()
            .as_ref()
            .is_none_or(|(current, frame, _)| {
                *current != generation || !frame.permits_native_content_use()
            })
        {
            return;
        }
        self.request_scroll(ScrollTarget::Offset(offset_rows));
    }
    fn scroll_scrollback(&self, movement: ScrollbackMovement) {
        let rows = self
            .geometry
            .get()
            .map_or(1, |geometry| geometry.grid().rows);
        self.request_scroll(match movement {
            ScrollbackMovement::Top => ScrollTarget::Top,
            ScrollbackMovement::Bottom => ScrollTarget::Bottom,
            ScrollbackMovement::PageUp => ScrollTarget::Relative(-i64::from(rows)),
            ScrollbackMovement::PageDown => ScrollTarget::Relative(i64::from(rows)),
        });
    }
    fn set_find_query(&self, generation: FindQueryGeneration, query: String) {
        self.find_task.borrow_mut().take();
        self.find_navigation_task.borrow_mut().take();
        self.find_generation.set(generation);
        let projection = self.channel.projection();
        let stamp_generation = {
            let Ok(mut projection) = projection.lock() else {
                return;
            };
            let Ok(find) = projection.find_mut() else {
                return;
            };
            if find.clear().is_err() {
                return;
            }
            find.generation()
        };
        if query.is_empty() {
            self.wake();
            return;
        }
        let Some(executor) = self.executor.borrow().clone() else {
            return;
        };
        let Ok(permit) = self.channel.permit(EffectKind::Read) else {
            return;
        };
        let Ok(stamp) = self.channel.query_stamp(&permit, stamp_generation) else {
            return;
        };
        let Ok(query) = BoundedFindQuery::new(query) else {
            return;
        };
        let Ok(request) = FindRequest::new(stamp, query, None) else {
            return;
        };
        if projection
            .lock()
            .ok()
            .and_then(|mut projection| projection.find_mut().ok()?.begin(request.clone()).ok())
            .is_none()
        {
            return;
        }
        let channel = self.channel.clone();
        let updates = self.updates.clone();
        let closed = Arc::clone(&self.closed);
        *self.find_task.borrow_mut() = Some(executor.spawn(async move {
            let Ok(mut stream) = channel.read(&permit, PaneReadRequest::Find(request)).await else {
                return;
            };
            while let Ok(Some(PaneReadItem::Find(item))) = stream.next().await {
                if closed.load(Ordering::Acquire) || !permit.is_current() {
                    return;
                }
                let accepted = channel
                    .projection()
                    .lock()
                    .ok()
                    .and_then(|mut projection| projection.find_mut().ok()?.accept(item).ok());
                if accepted.is_none() {
                    return;
                }
                let _ = updates.try_send(());
            }
        }));
    }
    fn navigate_find(&self, generation: FindQueryGeneration, direction: FindDirection) {
        if self.find_generation.get() != generation || !self.enabled.get() {
            return;
        }
        let forward = direction == FindDirection::Next;
        let projection = self.channel.projection();
        let navigation = projection
            .lock()
            .ok()
            .and_then(|mut projection| Some(projection.find_mut().ok()?.navigate(forward)));
        match navigation {
            Some(FindNavigation::Match(found)) => {
                if let Ok(mut projection) = projection.lock() {
                    let viewport = projection.viewport();
                    let _ = projection.set_viewport(Viewport {
                        anchor: Some(found.start().row),
                        ..viewport
                    });
                }
                let _ = self.request_frame();
            }
            Some(FindNavigation::LoadChunk { index }) => {
                let Some(executor) = self.executor.borrow().clone() else {
                    return;
                };
                let Some(request) = projection
                    .lock()
                    .ok()
                    .and_then(|projection| projection.find().request().cloned())
                else {
                    return;
                };
                let Ok(permit) = self.channel.permit(EffectKind::Read) else {
                    return;
                };
                let channel = self.channel.clone();
                let closed = Arc::clone(&self.closed);
                let updates = self.updates.clone();
                *self.find_navigation_task.borrow_mut() = Some(executor.spawn(async move {
                    let Ok(mut stream) =
                        channel.read(&permit, PaneReadRequest::Find(request)).await
                    else {
                        return;
                    };
                    while let Ok(Some(PaneReadItem::Find(item))) = stream.next().await {
                        if closed.load(Ordering::Acquire) || !permit.is_current() {
                            return;
                        }
                        let FindStreamItem::Chunk(chunk) = item else {
                            return;
                        };
                        if chunk.index() != index {
                            continue;
                        }
                        let projection = channel.projection();
                        let Ok(mut projection) = projection.lock() else {
                            return;
                        };
                        let Ok(find) = projection.find_mut() else {
                            return;
                        };
                        if find.load_window(chunk).is_err() {
                            return;
                        }
                        if let FindNavigation::Match(found) = find.navigate(forward) {
                            let viewport = projection.viewport();
                            let _ = projection.set_viewport(Viewport {
                                anchor: Some(found.start().row),
                                ..viewport
                            });
                            let _ = updates.try_send(());
                        }
                        return;
                    }
                }));
            }
            _ => {}
        }
    }
    fn end_find(&self, generation: FindQueryGeneration) {
        self.set_find_query(generation, String::new());
    }
    fn request_paste(
        &self,
        text: PastePayload,
    ) -> async_channel::Receiver<Result<PasteRequestOutcome, String>> {
        use crate::terminal::paste::PasteRejection;
        let (reply, receiver) = async_channel::bounded(1);
        self.expire_paste();
        let rejection = if text.text().len() > crate::terminal::MAX_PASTE_BYTES {
            Some(PasteRejection::TooLarge { limit: crate::terminal::MAX_PASTE_BYTES })
        } else if text.text().is_empty() {
            Some(PasteRejection::Empty)
        } else if self.pending_paste.borrow().is_some() {
            Some(PasteRejection::ConfirmationPending)
        } else if !self.accepts_input() || self.paste_confirmation_focused.get()
            || self.channel.permit(EffectKind::Paste).is_err() {
            Some(PasteRejection::TerminalUnfocused)
        } else { None };
        if let Some(rejection) = rejection {
            let _ = reply.try_send(Ok(PasteRequestOutcome::Rejected(rejection)));
            return receiver;
        }
        let admitted = (|| {
            let executor = self.executor.borrow().clone()
                .ok_or_else(|| ClientFailure::new(ClientFailureClass::MissingCapability))?;
            let permit = self.channel.permit(EffectKind::Paste)?;
            let text = NativeText::new(text.text().to_owned())
                .map_err(|_| ClientFailure::new(ClientFailureClass::ResourceLimit))?;
            let correlation = self.channel.try_effect(&permit, RuntimeEffect::RequestPaste(text))?
                .correlation().ok_or_else(|| ClientFailure::new(ClientFailureClass::MissingCapability))?;
            let expires = executor.now() + Duration::from_secs(30);
            let deadline = self.paste_deadline(&executor, &permit, expires, reply.clone(),
                PasteRequestOutcome::Rejected(PasteRejection::TerminalUnfocused));
            *self.pending_paste.borrow_mut() = Some(PendingPaste::Request {
                correlation, permit, reply: reply.clone(), expires, _deadline: deadline,
            });
            Ok::<_, ClientFailure>(())
        })();
        if let Err(error) = admitted { let _ = reply.try_send(Err(error.to_string())); }
        receiver
    }
    fn resolve_paste(
        &self,
        id: crate::terminal::paste::PasteConfirmationId,
        decision: PasteDecision,
    ) -> async_channel::Receiver<Result<PasteResolution, String>> {
        let (reply, receiver) = async_channel::bounded(1);
        self.expire_paste();
        let Some(pending) = self.pending_paste.borrow_mut().take() else {
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return receiver;
        };
        let PendingPaste::Approval { request, confirmation, expires, .. } = &pending else {
            *self.pending_paste.borrow_mut() = Some(pending);
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return receiver;
        };
        if confirmation.id != id {
            *self.pending_paste.borrow_mut() = Some(pending);
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return receiver;
        }
        let NativeRequestKind::UnsafePaste { approval, .. } = request.kind() else {
            let _ = reply.try_send(Ok(PasteResolution::Stale));
            return receiver;
        };
        let admitted = (|| {
            let executor = self.executor.borrow().clone()
                .ok_or_else(|| ClientFailure::new(ClientFailureClass::MissingCapability))?;
            let permit = self.channel.permit(EffectKind::Paste)?;
            if permit.fence() != request.fence() || !request.is_current() {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            let native_decision = match decision {
                PasteDecision::Confirm => spaceterm_core::native_requests::PasteDecision::Confirm,
                PasteDecision::Cancel => spaceterm_core::native_requests::PasteDecision::Cancel,
            };
            let correlation = self.channel.try_effect(&permit, RuntimeEffect::ResolvePaste(*approval, native_decision))?
                .correlation().ok_or_else(|| ClientFailure::new(ClientFailureClass::MissingCapability))?;
            let deadline = self.paste_deadline(&executor, &permit, *expires, reply.clone(), PasteResolution::Stale);
            *self.pending_paste.borrow_mut() = Some(PendingPaste::Resolution {
                correlation, permit, decision, reply: reply.clone(), expires: *expires, _deadline: deadline,
            });
            Ok::<_, ClientFailure>(())
        })();
        if let Err(error) = admitted {
            let result = if error.class() == ClientFailureClass::StaleAuthority {
                Ok(PasteResolution::Stale)
            } else { Err(error.to_string()) };
            let _ = reply.try_send(result);
        }
        receiver
    }
    fn focus_paste_confirmation(&self, id: crate::terminal::paste::PasteConfirmationId) {
        if self.paste_confirmation_is_current(id) {
            self.effect(EffectKind::FocusReport, RuntimeEffect::FocusReport(false));
            self.paste_confirmation_focused.set(true);
        }
    }
    fn copy_selection(&self) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        match self.displayed.borrow().as_ref() {
            Some((generation, _, _)) => self.prepared_copy_result(*generation),
            None => Ok(None),
        }
    }
    fn copy_selection_at(
        &self,
        generation: PresentationGeneration,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        self.prepared_copy_result(generation)
    }
    fn copy_or_forward(
        &self,
        modifiers: InputModifiers,
    ) -> Result<Option<SelectionCopy>, SelectionCopyError> {
        if self
            .channel
            .projection()
            .lock()
            .ok()
            .is_some_and(|projection| projection.selection().is_some())
        {
            return self.copy_selection();
        }
        if !self.closed.load(Ordering::Acquire) && self.enabled.get()
            && !self.paste_confirmation_focused.get()
            && let Ok(permit) = self.channel.permit(EffectKind::Key) {
            let _ = self.channel.try_effect(&permit, PaneRuntimeEffect::Copy(modifiers));
        }
        Ok(None)
    }
    fn accessibility_selection_sender(
        &self,
    ) -> Option<crate::terminal::AccessibilitySelectionSender> {
        let publication = self.accessibility_publication.borrow().clone()?;
        let displayed = self.displayed.borrow();
        let (generation, frame, references) = displayed.as_ref()?;
        if !publication.is_current() || !frame.permits_native_content_use() {
            return None;
        }
        // Bind the UTF-16 document and physical cell references together. A callback may never
        // reinterpret offsets against the document produced by a later snapshot.
        let permit = self.native_permit()?;
        let model = self.accessibility_model.borrow().clone()?;
        let generation = *generation;
        let frame = frame.clone();
        let references = references.clone();
        let weak = Rc::downgrade(self);
        Some(crate::terminal::AccessibilitySelectionSender::callback(
            move |request| {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                if !publication.is_current() || request.generation != generation {
                    return;
                }
                if model.selection_request(request.range.clone()).as_ref() != Some(&request) {
                    return;
                }
                let Ok(stamp) = session
                    .channel
                    .query_stamp(&permit, RequestGeneration::new(0))
                else {
                    return;
                };
                let selection = if request.range.is_empty() {
                    None
                } else {
                    let Some(start) = accessibility_cell(&model, &references, request.range.start)
                    else {
                        return;
                    };
                    let Some(end) = accessibility_cell(&model, &references, request.range.end - 1)
                    else {
                        return;
                    };
                    let Ok(selection) = ContentSelection::new(stamp, start, end, false) else {
                        return;
                    };
                    Some(selection)
                };
                if !publication.is_current() {
                    return;
                }
                // Publication holds channel state before projection. Never acquire channel state
                // while holding projection here. The captured frame checks projection eligibility;
                // the original permit is checked again after releasing the projection lock.
                let selected = session
                    .channel
                    .projection()
                    .lock()
                    .is_ok_and(|mut projection| {
                        frame.permits_native_content_use()
                            && projection.set_selection(selection).is_ok()
                    });
                if selected && publication.is_current() {
                    session.accessibility_copy_task.borrow_mut().take();
                    if let Ok(Some(request)) = session.copy_request()
                        && request.stamp().fence() == permit.fence()
                        && let Ok(preparation) = session.prepare_copy(request)
                    {
                        *session.accessibility_copy_task.borrow_mut() = Some(preparation);
                    }
                    session.wake();
                }
            },
        ))
    }
    fn accessibility_demand_sender(&self) -> Option<crate::terminal::AccessibilityDemandSender> {
        let publication = self.accessibility_publication.borrow().clone()?;
        let weak = Rc::downgrade(self);
        Some(crate::terminal::AccessibilityDemandSender::callback(
            move || {
                if publication.is_current()
                    && let Some(session) = weak.upgrade()
                {
                    let _ = session.request_frame();
                }
            },
        ))
    }
    fn set_presentable(&self, _presentable: bool) {}
    fn update_appearance(&self, update: TerminalAppearanceUpdate) {
        use spaceterm_terminal::runtime_contracts::RuntimeAppearance;
        let colors = &update.appearance.colors;
        let rgb = |color: crate::appearance::Color| content::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        };
        let palette = terminal_palette(colors).map(rgb);
        self.effect(
            EffectKind::Appearance,
            RuntimeEffect::Appearance(Box::new(RuntimeAppearance {
                generation: update.generation,
                foreground: rgb(colors.foreground),
                background: rgb(colors.background),
                palette,
                bold_as_bright: update.appearance.bold_as_bright,
                appearance: update.appearance.appearance,
            })),
        );
        self.wake();
    }
}

impl Drop for ServerPaneSession {
    fn drop(&mut self) {
        self.close_presentation();
    }
}

#[cfg(test)]
pub(crate) mod tests;
