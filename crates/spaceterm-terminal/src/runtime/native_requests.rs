//! Portable native-effect ordering, captured authority, paste approvals and deadlines.
use super::authority::AuthorityStamp;
use super::budget::BudgetLease;
use super::ingress::ReplySender;
use super::input::{InputCompletion, InputOrigin, PtyInput};
use super::owner::Shared;
use super::ports::{RuntimeFailure, RuntimeStage};
use crate::runtime_contracts::{EffectDisposition, RuntimeCommand};
use spaceterm_core::native_requests::*;
use spaceterm_core::observations::MonotonicTick;
use std::collections::VecDeque;

/// Non-replaceable bounded native requests. Stale requests are discarded without
/// closing the server's route to a later controlling connection.
pub(crate) struct RoutedNativeRequest {
    pub request: NativeRequest,
    pub stamp: AuthorityStamp,
}
pub struct NativeRequestSubscription {
    pub(crate) receiver: async_channel::Receiver<Result<RoutedNativeRequest, RuntimeFailure>>,
    pub(crate) shared: std::sync::Arc<Shared>,
}
impl NativeRequestSubscription {
    fn eligible(&self, routed: &RoutedNativeRequest) -> Result<bool, RuntimeFailure> {
        let now = self
            .shared
            .clock
            .now()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
        let request = &routed.request;
        Ok(self.shared.authority.read()?.permits_native(routed.stamp)
            && request.deadline().is_none_or(|deadline| now < deadline))
    }
    pub async fn next(&mut self) -> Result<Option<NativeRequest>, RuntimeFailure> {
        while let Ok(request) = self.receiver.recv().await {
            let request = request?;
            if self.eligible(&request)? {
                return Ok(Some(request.request));
            }
        }
        Ok(None)
    }
    pub fn next_blocking(&mut self) -> Result<Option<NativeRequest>, RuntimeFailure> {
        while let Ok(request) = self.receiver.recv_blocking() {
            let request = request?;
            if self.eligible(&request)? {
                return Ok(Some(request.request));
            }
        }
        Ok(None)
    }
    pub fn try_next(&mut self) -> Result<Option<NativeRequest>, RuntimeFailure> {
        while let Ok(request) = self.receiver.try_recv() {
            let request = request?;
            if self.eligible(&request)? {
                return Ok(Some(request.request));
            }
        }
        Ok(None)
    }
}
impl Drop for NativeRequestSubscription {
    fn drop(&mut self) {
        self.receiver.close();
    }
}

pub(crate) enum OutputEffect {
    Terminal(Vec<u8>, Option<AuthorityStamp>),
    Clipboard(osc::Osc52Operation, Option<AuthorityStamp>),
    Permission(PermissionNotice, Option<AuthorityStamp>),
}
#[derive(Clone, Copy)]
enum StringKind {
    Clipboard,
    Permission,
}
enum FilterState {
    Ground,
    Prefix,
    Native(StringKind, bool),
    Discard(bool),
    Other(bool, bool),
}
pub(crate) struct OutputFilter {
    state: FilterState,
    candidate: Vec<u8>,
    fence: Option<AuthorityStamp>,
    escape_fence: Option<AuthorityStamp>,
}
impl Default for OutputFilter {
    fn default() -> Self {
        Self {
            state: FilterState::Ground,
            candidate: Vec::new(),
            fence: None,
            escape_fence: None,
        }
    }
}
impl OutputFilter {
    pub fn feed(&mut self, bytes: &[u8], fence: Option<AuthorityStamp>) -> VecDeque<OutputEffect> {
        const CLIP: &[u8] = b"\x1b]52;";
        const PERMISSION: &[u8] = b"\x1b]7701;";
        let mut effects = VecDeque::new();
        let mut terminal = Vec::with_capacity(bytes.len());
        let mut terminal_fence = fence;
        for &byte in bytes {
            if matches!(
                self.state,
                FilterState::Native(_, true) | FilterState::Discard(true)
            ) && byte != b'\\'
            {
                // A non-ST escape abandons the notice, including oversized ones.
                // Reprocess that ESC with its own captured authority, even when
                // its following byte arrives in another reader chunk.
                self.candidate.clear();
                self.candidate.push(0x1b);
                self.fence = self.escape_fence;
                self.state = FilterState::Prefix;
            }
            if byte == 0x1b {
                self.escape_fence = fence;
            }
            match self.state {
                FilterState::Ground => {
                    if byte == 0x1b {
                        if !terminal.is_empty() {
                            effects.push_back(OutputEffect::Terminal(
                                std::mem::take(&mut terminal),
                                terminal_fence,
                            ));
                        }
                        terminal_fence = fence;
                        self.candidate.push(byte);
                        self.fence = fence;
                        self.state = FilterState::Prefix;
                    } else {
                        terminal.push(byte);
                    }
                }
                FilterState::Prefix => {
                    self.candidate.push(byte);
                    if self.candidate == CLIP {
                        self.state = FilterState::Native(StringKind::Clipboard, false);
                    } else if self.candidate == PERMISSION {
                        self.state = FilterState::Native(StringKind::Permission, false);
                    } else if !CLIP.starts_with(&self.candidate)
                        && !PERMISSION.starts_with(&self.candidate)
                    {
                        if self.fence != fence {
                            terminal_fence = None;
                        }
                        let candidate = std::mem::take(&mut self.candidate);
                        self.state = if matches!(byte, 0x18 | 0x1a)
                            || candidate.starts_with(b"\x1b]") && byte == 7
                        {
                            FilterState::Ground
                        } else if candidate.starts_with(b"\x1b]") {
                            FilterState::Other(true, byte == 0x1b)
                        } else if candidate
                            .get(1)
                            .is_some_and(|byte| matches!(byte, b'P' | b'_' | b'^' | b'X'))
                        {
                            FilterState::Other(false, byte == 0x1b)
                        } else {
                            FilterState::Ground
                        };
                        if matches!(self.state, FilterState::Ground) && byte == 0x1b {
                            terminal.extend_from_slice(&candidate[..candidate.len() - 1]);
                            self.candidate.push(byte);
                            self.fence = fence;
                            self.state = FilterState::Prefix;
                        } else {
                            terminal.extend(candidate);
                        }
                    }
                }
                FilterState::Other(bell, escape) => {
                    if self.fence != fence {
                        terminal_fence = None;
                    }
                    terminal.push(byte);
                    self.state = if bell && byte == 7
                        || escape && byte == b'\\'
                        || matches!(byte, 0x18 | 0x1a)
                    {
                        FilterState::Ground
                    } else {
                        FilterState::Other(bell, byte == 0x1b)
                    };
                }
                FilterState::Native(kind, escape) => {
                    self.candidate.push(byte);
                    if byte == 7 || escape && byte == b'\\' {
                        let raw = std::mem::take(&mut self.candidate);
                        match kind {
                            StringKind::Clipboard => {
                                if let Ok(operation) = osc::parse_osc52(&raw) {
                                    effects
                                        .push_back(OutputEffect::Clipboard(operation, self.fence));
                                }
                            }
                            StringKind::Permission => {
                                let end = raw.len() - if byte == 7 { 1 } else { 2 };
                                if let Some(body) = raw
                                    .get(PERMISSION.len()..end)
                                    .and_then(|body| body.strip_prefix(b"permissions="))
                                {
                                    let mut screen = false;
                                    let mut access = false;
                                    let mut valid = true;
                                    for name in body.split(|byte| *byte == b',') {
                                        match name {
                                            b"screen-recording" => screen = true,
                                            b"accessibility" => access = true,
                                            _ => valid = false,
                                        }
                                    }
                                    if valid
                                        && let Ok(notice) = PermissionNotice::new(screen, access)
                                    {
                                        effects.push_back(OutputEffect::Permission(
                                            notice, self.fence,
                                        ));
                                    }
                                }
                            }
                        }
                        terminal.extend_from_slice(b"\x1b\\");
                        self.state = FilterState::Ground;
                    } else if matches!(byte, 0x18 | 0x1a) {
                        self.candidate.clear();
                        self.state = FilterState::Ground;
                        terminal.push(byte);
                    } else if self.candidate.len()
                        > match kind {
                            StringKind::Clipboard => 1_398_116,
                            StringKind::Permission => 266,
                        }
                    {
                        self.candidate.clear();
                        self.state = FilterState::Discard(byte == 0x1b);
                    } else {
                        self.state = FilterState::Native(kind, byte == 0x1b);
                    }
                }
                FilterState::Discard(escape) => {
                    if byte == 7 || escape && byte == b'\\' || matches!(byte, 0x18 | 0x1a) {
                        self.state = FilterState::Ground;
                        terminal.extend_from_slice(b"\x1b\\");
                    } else {
                        self.state = FilterState::Discard(byte == 0x1b);
                    }
                }
            }
        }
        if !terminal.is_empty() {
            effects.push_back(OutputEffect::Terminal(terminal, terminal_fence));
        }
        effects
    }
}
struct ClipboardPending {
    request: NativeRequest,
    stamp: AuthorityStamp,
    read: Option<(osc::Osc52Target, osc::Osc52Terminator)>,
}
struct PastePending {
    request: NativeRequest,
    stamp: AuthorityStamp,
    text: NativeText,
    input: BudgetLease,
}
#[derive(Default)]
pub(crate) struct NativeRequests {
    clipboard: Option<ClipboardPending>,
    paste: Option<PastePending>,
    permission: Option<RoutedNativeRequest>,
    notification: Option<RoutedNativeRequest>,
    next_request: u64,
    next_paste: u64,
}
impl NativeRequests {
    fn id(&mut self) -> Result<NativeRequestId, RuntimeFailure> {
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
        NativeRequestId::new(self.next_request)
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))
    }
    pub fn clipboard_pending(&self) -> bool {
        self.clipboard.is_some()
    }
    pub fn clipboard(
        &mut self,
        operation: osc::Osc52Operation,
        captured: Option<AuthorityStamp>,
        now: MonotonicTick,
        shared: &Shared,
        input: &mut PtyInput,
        hidden: bool,
    ) -> Result<(), RuntimeFailure> {
        let read = match &operation {
            osc::Osc52Operation::Read { target, terminator } => Some((*target, *terminator)),
            _ => None,
        };
        // The frozen native DTO has no clipboard-target selector. Deny selection
        // targets rather than executing them against a different client clipboard.
        let supported_target = match &operation {
            osc::Osc52Operation::Read { target, .. } => matches!(
                target,
                osc::Osc52Target::Default | osc::Osc52Target::Standard
            ),
            osc::Osc52Operation::Write { _target, .. } => matches!(
                _target,
                osc::Osc52Target::Default | osc::Osc52Target::Standard
            ),
        };
        let eligible = captured
            .filter(|_| supported_target)
            .filter(|fence| {
                shared
                    .authority
                    .read()
                    .is_ok_and(|authority| authority.permits_native(*fence))
            })
            .filter(|_| {
                !hidden
                    && shared
                        .native_routed
                        .load(std::sync::atomic::Ordering::Acquire)
                    && !shared.native_sender.is_closed()
            });
        let Some(fence) = eligible else {
            if let Some((target, terminator)) = read {
                let bytes = osc::read_response(target, terminator, "");
                let lease = shared.reserve_input(bytes.len(), false)?;
                input.enqueue(bytes, InputOrigin::Session, lease, None)?;
            }
            return Ok(());
        };
        let kind = match operation {
            osc::Osc52Operation::Read { .. } => NativeRequestKind::ClipboardRead,
            osc::Osc52Operation::Write { text, .. } => NativeRequestKind::ClipboardWrite(
                NativeText::new(text)
                    .map_err(|_| RuntimeFailure::capacity(RuntimeStage::NativeRequest))?,
            ),
        };
        let deadline = now
            .get()
            .checked_add(OSC52_READ_DEADLINE_MILLIS)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
        let request = NativeRequest::new(
            fence.fence,
            self.id()?,
            now,
            now,
            Some(MonotonicTick::new(deadline)),
            kind,
        )
        .map_err(|_| RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
        if read.is_some() {
            // Reserve the complete possible read reply before issuing native work.
            match shared.reserve_input(MAX_NATIVE_TEXT_BYTES.div_ceil(3) * 4 + 24, false) {
                Ok(lease) => input.barrier(request.id(), lease),
                Err(_) => {
                    let (target, terminator) =
                        read.ok_or(RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
                    let bytes = osc::read_response(target, terminator, "");
                    let lease = shared.reserve_input(bytes.len(), false)?;
                    input.enqueue(bytes, InputOrigin::Session, lease, None)?;
                    return Ok(());
                }
            }
        }
        if shared
            .native_sender
            .try_send(Ok(RoutedNativeRequest {
                request: request.clone(),
                stamp: fence,
            }))
            .is_err()
        {
            if let Some((target, terminator)) = read {
                input.complete_barrier(
                    request.id(),
                    osc::read_response(target, terminator, ""),
                    InputOrigin::Session,
                    None,
                )?;
            }
            return Ok(());
        }
        self.clipboard = Some(ClipboardPending {
            request,
            stamp: fence,
            read,
        });
        Ok(())
    }
    pub fn paste(
        &mut self,
        text: NativeText,
        fence: AuthorityStamp,
        now: MonotonicTick,
        bracketed: bool,
        shared: &Shared,
        completion: (BudgetLease, ReplySender<EffectDisposition>),
    ) -> Result<Option<(RuntimeCommand, BudgetLease, ReplySender<EffectDisposition>)>, RuntimeFailure>
    {
        let (reservation, reply) = completion;
        if self.paste.is_some() {
            reply.complete(Err(RuntimeFailure::capacity(RuntimeStage::NativeRequest)));
            return Ok(None);
        }
        let text = text.into_string().replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            reply.complete(Ok(EffectDisposition::Applied));
            return Ok(None);
        }
        let unsafe_paste = text.contains("\x1b[201~") || text.contains('\n') && !bracketed;
        let text: String = text
            .chars()
            .filter(|character| {
                !matches!(
                    *character,
                    '\0' | '\x08'
                        | '\x05'
                        | '\x04'
                        | '\x1b'
                        | '\x7f'
                        | '\x03'
                        | '\x1c'
                        | '\x15'
                        | '\x1a'
                        | '\x11'
                        | '\x13'
                        | '\x17'
                        | '\x16'
                        | '\x12'
                        | '\x0f'
                )
            })
            .collect();
        let text = NativeText::new(text)
            .map_err(|_| RuntimeFailure::capacity(RuntimeStage::NativeRequest))?;
        if !unsafe_paste {
            return Ok(Some((
                RuntimeCommand::RequestPaste(text),
                reservation,
                reply,
            )));
        }
        self.next_paste = self
            .next_paste
            .checked_add(1)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
        let approval = PasteApprovalId::new(self.next_paste)
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Counter))?;
        let deadline = now
            .get()
            .checked_add(PASTE_APPROVAL_DEADLINE_MILLIS)
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?;
        let request = NativeRequest::new(
            fence.fence,
            self.id()?,
            now,
            now,
            Some(MonotonicTick::new(deadline)),
            NativeRequestKind::UnsafePaste {
                approval,
                text: text.clone(),
            },
        )
        .map_err(|_| RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
        if !shared
            .native_routed
            .load(std::sync::atomic::Ordering::Acquire)
            || shared
                .native_sender
                .try_send(Ok(RoutedNativeRequest {
                    request: request.clone(),
                    stamp: fence,
                }))
                .is_err()
        {
            reply.complete(Ok(EffectDisposition::Cancelled));
            return Ok(None);
        }
        self.paste = Some(PastePending {
            request,
            stamp: fence,
            text,
            input: reservation,
        });
        reply.complete(Ok(EffectDisposition::Applied));
        Ok(None)
    }
    pub fn resolve_paste(
        &mut self,
        approval: PasteApprovalId,
        decision: PasteDecision,
        fence: AuthorityStamp,
        now: MonotonicTick,
    ) -> Option<(NativeText, BudgetLease)> {
        let pending = self.paste.as_ref()?;
        if pending.stamp != fence
            || !matches!(pending.request.kind(), NativeRequestKind::UnsafePaste { approval: expected, .. } if *expected == approval)
        {
            return None;
        }
        let pending = self.paste.take()?;
        if decision != PasteDecision::Confirm
            || pending
                .request
                .deadline()
                .is_some_and(|deadline| now >= deadline)
        {
            return None;
        }
        Some((pending.text, pending.input))
    }
    pub fn resolve_completion_paste(
        &mut self,
        completion: &NativeCompletion,
        stamp: AuthorityStamp,
        approval: PasteApprovalId,
        decision: PasteDecision,
        now: MonotonicTick,
    ) -> Option<(NativeText, BudgetLease)> {
        if self.paste.as_ref()?.request.id() != completion.id() {
            return None;
        }
        self.resolve_paste(approval, decision, stamp, now)
    }
    pub fn permission(
        &mut self,
        notice: PermissionNotice,
        captured: Option<AuthorityStamp>,
        now: MonotonicTick,
        shared: &Shared,
    ) -> Result<(), RuntimeFailure> {
        if self.permission.is_some()
            || !shared
                .native_routed
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(fence) = captured.filter(|fence| {
            shared
                .authority
                .read()
                .is_ok_and(|authority| authority.permits_native(*fence))
        }) else {
            return Ok(());
        };
        let not_before = MonotonicTick::new(
            now.get()
                .checked_add(500)
                .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?,
        );
        let request = NativeRequest::new(
            fence.fence,
            self.id()?,
            now,
            not_before,
            None,
            NativeRequestKind::Permission(notice),
        )
        .map_err(|_| RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
        if shared
            .native_sender
            .try_send(Ok(RoutedNativeRequest {
                request: request.clone(),
                stamp: fence,
            }))
            .is_ok()
        {
            self.permission = Some(RoutedNativeRequest {
                request,
                stamp: fence,
            });
        }
        Ok(())
    }
    pub fn notify(
        &mut self,
        notice: NotificationNotice,
        captured: Option<AuthorityStamp>,
        now: MonotonicTick,
        shared: &Shared,
    ) -> Result<(), RuntimeFailure> {
        if self.notification.is_some()
            || !shared
                .native_routed
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(stamp) = captured.filter(|stamp| {
            shared
                .authority
                .read()
                .is_ok_and(|authority| authority.permits_native(*stamp))
        }) else {
            return Ok(());
        };
        let deadline = MonotonicTick::new(
            now.get()
                .checked_add(5000)
                .ok_or(RuntimeFailure::capacity(RuntimeStage::Counter))?,
        );
        let request = NativeRequest::new(
            stamp.fence,
            self.id()?,
            now,
            now,
            Some(deadline),
            NativeRequestKind::Notification(notice),
        )
        .map_err(|_| RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
        if shared
            .native_sender
            .try_send(Ok(RoutedNativeRequest {
                request: request.clone(),
                stamp,
            }))
            .is_ok()
        {
            self.notification = Some(RoutedNativeRequest { request, stamp });
        }
        Ok(())
    }
    pub fn complete(
        &mut self,
        completion: &NativeCompletion,
        now: MonotonicTick,
        shared: &Shared,
        input: &mut PtyInput,
        reply: ReplySender<EffectDisposition>,
        hidden: bool,
    ) -> Result<(), RuntimeFailure> {
        if let Some(pending) = &self.clipboard
            && pending.request.id() == completion.id()
            && pending.request.fence() == completion.fence()
        {
            if NativeCompletion::new(&pending.request, completion.kind().clone()).is_err() {
                reply.complete(Ok(EffectDisposition::StaleAuthority));
                return Ok(());
            }
            let pending = self
                .clipboard
                .take()
                .ok_or(RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
            let eligible = shared.authority.read()?.permits_native(pending.stamp)
                && now >= pending.request.not_before()
                && pending
                    .request
                    .deadline()
                    .is_none_or(|deadline| now < deadline)
                && !hidden;
            if let Some((target, terminator)) = pending.read {
                let text = if eligible {
                    match completion.kind() {
                        NativeCompletionKind::ClipboardText(text) => text.as_str(),
                        _ => "",
                    }
                } else {
                    ""
                };
                let bytes = osc::read_response(target, terminator, text);
                if text.is_empty() {
                    input.complete_barrier(
                        completion.id(),
                        bytes,
                        InputOrigin::Session,
                        Some(InputCompletion::Effect(reply)),
                    )?;
                } else {
                    let deadline = pending
                        .request
                        .deadline()
                        .ok_or(RuntimeFailure::internal(RuntimeStage::NativeRequest))?;
                    input.native_reply(
                        completion.id(),
                        bytes,
                        osc::read_response(target, terminator, ""),
                        pending.stamp,
                        deadline,
                        InputCompletion::Effect(reply),
                    )?;
                }
            } else {
                reply.complete(Ok(if eligible {
                    EffectDisposition::Applied
                } else {
                    EffectDisposition::StaleAuthority
                }));
            }
            return Ok(());
        }
        if self.paste.as_ref().is_some_and(|pending| {
            pending.request.id() == completion.id()
                && pending.request.fence() == completion.fence()
                && matches!(
                    completion.kind(),
                    NativeCompletionKind::Denied
                        | NativeCompletionKind::Cancelled
                        | NativeCompletionKind::Expired
                )
        }) {
            self.paste = None;
            reply.complete(Ok(EffectDisposition::Cancelled));
            return Ok(());
        }
        if self.notification.as_ref().is_some_and(|routed| {
            let request = &routed.request;
            shared
                .authority
                .read()
                .is_ok_and(|authority| authority.permits_native(routed.stamp))
                && request.id() == completion.id()
                && request.fence() == completion.fence()
                && request.deadline().is_some_and(|deadline| now < deadline)
                && NativeCompletion::new(request, completion.kind().clone()).is_ok()
        }) {
            self.notification = None;
            reply.complete(Ok(EffectDisposition::Applied));
            return Ok(());
        }
        if self.permission.as_ref().is_some_and(|routed| {
            let request = &routed.request;
            shared
                .authority
                .read()
                .is_ok_and(|authority| authority.permits_native(routed.stamp))
                && request.id() == completion.id()
                && request.fence() == completion.fence()
                && now >= request.not_before()
                && NativeCompletion::new(request, completion.kind().clone()).is_ok()
        }) {
            self.permission = None;
            reply.complete(Ok(EffectDisposition::Applied));
            return Ok(());
        }
        reply.complete(Ok(EffectDisposition::StaleAuthority));
        Ok(())
    }
    pub fn expire(
        &mut self,
        now: MonotonicTick,
        shared: &Shared,
        input: &mut PtyInput,
    ) -> Result<(), RuntimeFailure> {
        if self.clipboard.as_ref().is_some_and(|pending| {
            pending
                .request
                .deadline()
                .is_some_and(|deadline| now >= deadline)
                || shared.native_sender.is_closed()
                || shared
                    .authority
                    .read()
                    .is_ok_and(|authority| !authority.permits_native(pending.stamp))
        }) && let Some(pending) = self.clipboard.take()
            && let Some((target, terminator)) = pending.read
        {
            input.complete_barrier(
                pending.request.id(),
                osc::read_response(target, terminator, ""),
                InputOrigin::Session,
                None,
            )?;
        }
        if self.paste.as_ref().is_some_and(|pending| {
            pending
                .request
                .deadline()
                .is_some_and(|deadline| now >= deadline)
                || shared
                    .authority
                    .read()
                    .is_ok_and(|authority| !authority.permits_native(pending.stamp))
        }) {
            self.paste = None;
        }
        if self.notification.as_ref().is_some_and(|routed| {
            routed
                .request
                .deadline()
                .is_some_and(|deadline| now >= deadline)
                || shared
                    .authority
                    .read()
                    .is_ok_and(|authority| !authority.permits_native(routed.stamp))
        }) {
            self.notification = None;
        }
        if self.permission.as_ref().is_some_and(|routed| {
            shared
                .authority
                .read()
                .is_ok_and(|authority| !authority.permits_native(routed.stamp))
        }) {
            self.permission = None;
        }
        Ok(())
    }
    pub fn clear(&mut self) {
        self.clipboard = None;
        self.paste = None;
        self.permission = None;
        self.notification = None;
    }
}

mod osc {
    const MAX_OSC52_CONTENT_BYTES: usize = 1024 * 1024;
    const MAX_OSC52_ENCODED_BYTES: usize = MAX_OSC52_CONTENT_BYTES.div_ceil(3) * 4;
    const OSC52_PREFIX: &[u8] = b"\x1b]52;";
    #[derive(Clone, Copy)]
    pub(crate) enum Osc52Target {
        Default,
        Standard,
        Selection,
        Primary,
    }
    #[derive(Clone, Copy)]
    pub(crate) enum Osc52Terminator {
        Bell,
        StringTerminator,
    }
    pub(crate) enum Osc52Operation {
        Read {
            target: Osc52Target,
            terminator: Osc52Terminator,
        },
        Write {
            _target: Osc52Target,
            text: String,
        },
    }
    pub(super) enum Osc52Rejection {
        Malformed,
        Oversized,
        UnsupportedTarget,
        InvalidBase64,
        InvalidUtf8,
    }
    pub(super) fn parse_osc52(raw: &[u8]) -> Result<Osc52Operation, Osc52Rejection> {
        let (terminator, end) = if raw.ends_with(b"\x1b\\") {
            (Osc52Terminator::StringTerminator, raw.len() - 2)
        } else if raw.last() == Some(&0x07) {
            (Osc52Terminator::Bell, raw.len() - 1)
        } else {
            return Err(Osc52Rejection::Malformed);
        };
        let body = raw
            .get(OSC52_PREFIX.len()..end)
            .ok_or(Osc52Rejection::Malformed)?;
        let separator = body
            .iter()
            .position(|byte| *byte == b';')
            .ok_or(Osc52Rejection::Malformed)?;
        let target = match &body[..separator] {
            b"" => Osc52Target::Default,
            b"c" => Osc52Target::Standard,
            b"s" => Osc52Target::Selection,
            b"p" => Osc52Target::Primary,
            _ => return Err(Osc52Rejection::UnsupportedTarget),
        };
        let payload = &body[separator + 1..];
        if payload == b"?" {
            return Ok(Osc52Operation::Read { target, terminator });
        }
        if payload.len() > MAX_OSC52_ENCODED_BYTES {
            return Err(Osc52Rejection::Oversized);
        }
        let decoded = decode_base64(payload)?;
        if decoded.len() > MAX_OSC52_CONTENT_BYTES {
            return Err(Osc52Rejection::Oversized);
        }
        let text = String::from_utf8(decoded).map_err(|_| Osc52Rejection::InvalidUtf8)?;
        Ok(Osc52Operation::Write {
            _target: target,
            text,
        })
    }

    pub(super) fn read_response(
        target: Osc52Target,
        terminator: Osc52Terminator,
        text: &str,
    ) -> Vec<u8> {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let selector: &[u8] = match target {
            Osc52Target::Default => b"",
            Osc52Target::Standard => b"c",
            Osc52Target::Selection => b"s",
            Osc52Target::Primary => b"p",
        };
        let mut response = Vec::with_capacity(text.len().div_ceil(3) * 4 + 10);
        response.extend_from_slice(b"\x1b]52;");
        response.extend_from_slice(selector);
        response.push(b';');
        for chunk in text.as_bytes().chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            response.push(ALPHABET[usize::from(a >> 2)]);
            response.push(ALPHABET[usize::from(((a & 3) << 4) | (b >> 4))]);
            response.push(if chunk.len() > 1 {
                ALPHABET[usize::from(((b & 15) << 2) | (c >> 6))]
            } else {
                b'='
            });
            response.push(if chunk.len() > 2 {
                ALPHABET[usize::from(c & 63)]
            } else {
                b'='
            });
        }
        response.extend_from_slice(match terminator {
            Osc52Terminator::Bell => b"\x07",
            Osc52Terminator::StringTerminator => b"\x1b\\",
        });
        response
    }

    fn decode_base64(input: &[u8]) -> Result<Vec<u8>, Osc52Rejection> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        if !input.len().is_multiple_of(4) {
            return Err(Osc52Rejection::InvalidBase64);
        }
        let mut output = Vec::with_capacity(input.len() / 4 * 3);
        for (index, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
            let last = index + 1 == input.len() / 4;
            let padding = match (chunk[2] == b'=', chunk[3] == b'=') {
                (true, true) => 2,
                (false, true) => 1,
                (false, false) => 0,
                (true, false) => return Err(Osc52Rejection::InvalidBase64),
            };
            if padding != 0 && !last {
                return Err(Osc52Rejection::InvalidBase64);
            }
            let a = base64_value(chunk[0])?;
            let b = base64_value(chunk[1])?;
            let c = if padding == 2 {
                0
            } else {
                base64_value(chunk[2])?
            };
            let d = if padding == 0 {
                base64_value(chunk[3])?
            } else {
                0
            };
            if (padding == 2 && b & 0x0f != 0) || (padding == 1 && c & 0x03 != 0) {
                return Err(Osc52Rejection::InvalidBase64);
            }
            output.push((a << 2) | (b >> 4));
            if padding < 2 {
                output.push((b << 4) | (c >> 2));
            }
            if padding == 0 {
                output.push((c << 6) | d);
            }
        }
        Ok(output)
    }

    fn base64_value(byte: u8) -> Result<u8, Osc52Rejection> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(Osc52Rejection::InvalidBase64),
        }
    }
}
