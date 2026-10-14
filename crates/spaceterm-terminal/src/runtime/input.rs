//! Ordered, pre-reserved nonblocking PTY input. Every retry takes a final-effect guard.
use super::authority::AuthorityStamp;
use super::budget::BudgetLease;
use super::ingress::ReplySender;
use super::owner::Shared;
use super::ports::{PtyProcess, RuntimeFailure, RuntimeStage};
use crate::runtime_contracts::{
    AuthorityApplied, EffectDisposition, RuntimeAuthorityTransition, RuntimeCommand,
};
use spaceterm_core::authority::EffectFence;
use spaceterm_core::native_requests::NativeRequestId;
use spaceterm_core::observations::MonotonicTick;
use std::collections::VecDeque;
use std::io;

#[derive(Clone, Copy)]
pub(crate) enum Eligibility {
    Input,
    Ime,
    Native,
    Control,
}
#[derive(Clone, Copy)]
pub(crate) enum InputOrigin {
    Session,
    Client(AuthorityStamp, Eligibility),
    Native(AuthorityStamp, MonotonicTick),
    EnhancedCopy(AuthorityStamp),
    ApprovedPaste(AuthorityStamp, MonotonicTick),
}
impl InputOrigin {
    fn current(self, shared: &Shared, authority: &super::authority::Authority) -> bool {
        if authority.sealed {
            return false;
        }
        match self {
            Self::Session => true,
            Self::Native(fence, deadline) => {
                authority.permits_native(fence)
                    && shared.clock.now().is_ok_and(|now| now < deadline)
            }
            Self::ApprovedPaste(stamp, deadline) => {
                authority.permits_stamp(stamp, None, Some(shared.revision()))
                    && authority.current.parts().terminal_input_focus
                    && shared.clock.now().is_ok_and(|now| now < deadline)
            }
            Self::EnhancedCopy(stamp) => {
                authority.permits_stamp(stamp, None, Some(shared.revision()))
                    && authority.current.parts().terminal_input_focus
            }
            Self::Client(stamp, eligibility) => {
                let parts = authority.current.parts();
                authority.permits_stamp(stamp, None, Some(shared.revision()))
                    && match eligibility {
                        Eligibility::Input => parts.terminal_input_focus,
                        Eligibility::Ime => parts.focused_pane,
                        Eligibility::Native => parts.native_effects_eligible,
                        Eligibility::Control => true,
                    }
            }
        }
    }
}
pub(crate) enum InputCompletion {
    Effect(ReplySender<EffectDisposition>),
    Paste {
        origin: Option<ReplySender<EffectDisposition>>,
        resolution: ReplySender<EffectDisposition>,
    },
    Authority(
        ReplySender<AuthorityApplied>,
        Box<RuntimeAuthorityTransition>,
    ),
}
impl InputCompletion {
    pub(super) fn cancelled(&self) -> bool {
        match self {
            Self::Effect(reply) => reply.cancelled(),
            Self::Paste { origin, resolution } => {
                resolution.cancelled() || origin.as_ref().is_some_and(ReplySender::cancelled)
            }
            Self::Authority(..) => false,
        }
    }
    pub fn fail(self, error: RuntimeFailure) {
        match self {
            Self::Effect(reply) => reply.complete(Err(error)),
            Self::Paste { origin, resolution } => {
                if let Some(reply) = origin {
                    reply.complete(Err(error));
                }
                resolution.complete(Err(error));
            }
            Self::Authority(reply, _) => reply.complete(Err(error)),
        }
    }
    pub fn complete(self, disposition: EffectDisposition) {
        match self {
            Self::Effect(reply) => reply.complete(Ok(disposition)),
            Self::Paste { origin, resolution } => {
                if let Some(reply) = origin {
                    reply.complete(Ok(disposition));
                }
                resolution.complete(Ok(disposition));
            }
            Self::Authority(reply, transition) => {
                reply.complete(if disposition == EffectDisposition::Applied {
                    Ok(AuthorityApplied::new(*transition))
                } else {
                    Err(RuntimeFailure::stale())
                })
            }
        }
    }
}
struct Chunk {
    bytes: Vec<u8>,
    waiting: Option<NativeRequestId>,
    fallback: Option<Vec<u8>>,
    disposition: EffectDisposition,
    offset: usize,
    origin: InputOrigin,
    _lease: BudgetLease,
    completion: Option<InputCompletion>,
}
impl Chunk {
    fn reject(&mut self, disposition: EffectDisposition) -> bool {
        if disposition != EffectDisposition::Retired
            && let Some(fallback) = self.fallback.take()
        {
            self.bytes = fallback;
            self.offset = 0;
            self.origin = InputOrigin::Session;
            self.disposition = disposition;
            self._lease.shrink(self.bytes.len());
            return true;
        }
        if let Some(completion) = self.completion.take() {
            completion.complete(disposition);
        }
        false
    }
}
#[derive(Default)]
pub(crate) struct PtyInput {
    chunks: VecDeque<Chunk>,
}
impl PtyInput {
    pub fn enqueue(
        &mut self,
        bytes: Vec<u8>,
        origin: InputOrigin,
        mut lease: BudgetLease,
        completion: Option<InputCompletion>,
    ) -> Result<(), RuntimeFailure> {
        if bytes.len() > lease.bytes() {
            return Err(RuntimeFailure::capacity(RuntimeStage::Input));
        }
        lease.shrink(bytes.len());
        if bytes.is_empty() {
            if let Some(completion) = completion {
                completion.complete(EffectDisposition::Applied);
            }
            return Ok(());
        }
        self.chunks.push_back(Chunk {
            bytes,
            waiting: None,
            fallback: None,
            disposition: EffectDisposition::Applied,
            offset: 0,
            origin,
            _lease: lease,
            completion,
        });
        Ok(())
    }
    pub fn barrier(&mut self, id: NativeRequestId, lease: BudgetLease) {
        self.chunks.push_back(Chunk {
            bytes: Vec::new(),
            waiting: Some(id),
            fallback: None,
            disposition: EffectDisposition::Applied,
            offset: 0,
            origin: InputOrigin::Session,
            _lease: lease,
            completion: None,
        });
    }
    pub fn complete_barrier(
        &mut self,
        id: NativeRequestId,
        bytes: Vec<u8>,
        origin: InputOrigin,
        completion: Option<InputCompletion>,
    ) -> Result<(), RuntimeFailure> {
        let chunk = self
            .chunks
            .iter_mut()
            .find(|chunk| chunk.waiting == Some(id))
            .ok_or(RuntimeFailure::stale())?;
        if bytes.len() > chunk._lease.bytes() {
            return Err(RuntimeFailure::capacity(RuntimeStage::Input));
        }
        chunk.waiting = None;
        chunk.bytes = bytes;
        chunk.origin = origin;
        chunk.completion = completion;
        chunk
            ._lease
            .shrink(chunk.bytes.len() + chunk.fallback.as_ref().map_or(0, Vec::len));
        Ok(())
    }
    pub fn native_reply(
        &mut self,
        id: NativeRequestId,
        bytes: Vec<u8>,
        fallback: Vec<u8>,
        stamp: AuthorityStamp,
        deadline: MonotonicTick,
        completion: InputCompletion,
    ) -> Result<(), RuntimeFailure> {
        let chunk = self
            .chunks
            .iter_mut()
            .find(|chunk| chunk.waiting == Some(id))
            .ok_or(RuntimeFailure::stale())?;
        if bytes
            .len()
            .checked_add(fallback.len())
            .is_none_or(|bytes| bytes > chunk._lease.bytes())
        {
            return Err(RuntimeFailure::capacity(RuntimeStage::Input));
        }
        chunk.fallback = Some(fallback);
        self.complete_barrier(
            id,
            bytes,
            InputOrigin::Native(stamp, deadline),
            Some(completion),
        )
    }
    pub fn discard_ineligible(&mut self, shared: &Shared) -> Result<(), RuntimeFailure> {
        let authority = shared.authority.read()?;
        self.chunks.retain_mut(|chunk| {
            let cancelled = chunk
                .completion
                .as_ref()
                .is_some_and(InputCompletion::cancelled);
            let current = chunk.origin.current(shared, &authority);
            if !current || cancelled {
                return chunk.reject(if cancelled {
                    EffectDisposition::Cancelled
                } else {
                    EffectDisposition::StaleAuthority
                });
            }
            true
        });
        Ok(())
    }
    pub fn drain(
        &mut self,
        shared: &Shared,
        process: &mut dyn PtyProcess,
    ) -> Result<(), RuntimeFailure> {
        let mut budget = 64 * 1024;
        for _ in 0..16 {
            let Some(chunk) = self.chunks.front_mut() else {
                return Ok(());
            };
            let authority = shared.authority.read()?;
            let cancelled = chunk
                .completion
                .as_ref()
                .is_some_and(InputCompletion::cancelled);
            let hidden_native = matches!(
                chunk.origin,
                InputOrigin::Native(..) | InputOrigin::EnhancedCopy(_)
            ) && process.hidden_input()?;
            if !chunk.origin.current(shared, &authority) || cancelled || hidden_native {
                let disposition = if authority.sealed {
                    EffectDisposition::Retired
                } else if cancelled {
                    EffectDisposition::Cancelled
                } else {
                    EffectDisposition::StaleAuthority
                };
                if !chunk.reject(disposition) {
                    self.chunks.pop_front();
                }
                continue;
            }
            if chunk.waiting.is_some() {
                return Ok(());
            }
            if chunk.offset == chunk.bytes.len() {
                if let Some(completion) = chunk.completion.take() {
                    completion.complete(chunk.disposition);
                }
                self.chunks.pop_front();
                continue;
            }
            let end = chunk.bytes.len().min(chunk.offset + budget);
            match process.write(&chunk.bytes[chunk.offset..end]) {
                Ok(0) => {
                    return Err(RuntimeFailure::new(
                        spaceterm_core::organization::FailureClass::RuntimeFault,
                        RuntimeStage::Input,
                    ));
                }
                Ok(written) if written <= end - chunk.offset => {
                    chunk.offset += written;
                    budget -= written;
                    if chunk.offset == chunk.bytes.len() {
                        if let Some(completion) = chunk.completion.take() {
                            completion.complete(chunk.disposition);
                        }
                        self.chunks.pop_front();
                    }
                }
                Ok(_) => return Err(RuntimeFailure::internal(RuntimeStage::Input)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(_) => {
                    return Err(RuntimeFailure::new(
                        spaceterm_core::organization::FailureClass::RuntimeFault,
                        RuntimeStage::Input,
                    ));
                }
            }
            drop(authority);
            if budget == 0 {
                break;
            }
        }
        Ok(())
    }
    pub fn retire(&mut self) {
        for mut chunk in self.chunks.drain(..) {
            if let Some(completion) = chunk.completion.take() {
                completion.complete(EffectDisposition::Retired);
            }
        }
    }
}
pub(crate) fn eligibility(command: &RuntimeCommand) -> Eligibility {
    match command {
        RuntimeCommand::InputMethodCommit(_) => Eligibility::Ime,
        RuntimeCommand::Key(_)
        | RuntimeCommand::Copy(_)
        | RuntimeCommand::RequestPaste(_)
        | RuntimeCommand::RequestCorrelatedPaste(..)
        | RuntimeCommand::ResolvePaste(..)
        | RuntimeCommand::Pointer(_)
        | RuntimeCommand::Wheel(_) => Eligibility::Input,
        RuntimeCommand::CompleteNativeRequest(_) => Eligibility::Native,
        _ => Eligibility::Control,
    }
}
pub(crate) fn command_cost(command: &RuntimeCommand) -> Result<(usize, usize), RuntimeFailure> {
    let (payload, input) = match command {
        RuntimeCommand::Key(key) => {
            let len = key
                .logical_key
                .len()
                .checked_add(key.text.as_ref().map_or(0, String::len))
                .ok_or(RuntimeFailure::capacity(RuntimeStage::Admission))?;
            if len > 65_536 || key.native_key_code.is_some() {
                return Err(RuntimeFailure::new(
                    spaceterm_core::organization::FailureClass::InvalidRequest,
                    RuntimeStage::Admission,
                ));
            }
            (
                len,
                len.checked_mul(8)
                    .and_then(|value| value.checked_add(128))
                    .ok_or(RuntimeFailure::capacity(RuntimeStage::Input))?,
            )
        }
        RuntimeCommand::Copy(_) => (0, 256),
        RuntimeCommand::InputMethodCommit(text) => {
            (text.as_str().len(), text.as_str().len() * 8 + 128)
        }
        RuntimeCommand::RequestPaste(text) | RuntimeCommand::RequestCorrelatedPaste(_, text) => {
            (text.as_str().len(), text.as_str().len() + 12)
        }
        RuntimeCommand::CompleteNativeRequest(completion) => match completion.kind() {
            spaceterm_core::native_requests::NativeCompletionKind::ClipboardText(text) => {
                (text.as_str().len(), 0)
            }
            _ => (0, 0),
        },
        RuntimeCommand::Appearance(_) => (
            std::mem::size_of::<crate::runtime_contracts::RuntimeAppearance>(),
            0,
        ),
        RuntimeCommand::Pointer(_) => (0, 4096),
        // Two bounded 100-step axes, including long SGR pixel coordinates.
        RuntimeCommand::Wheel(_) => (0, 8192),
        RuntimeCommand::FocusReport(_) => (0, 16),
        _ => (0, 0),
    };
    Ok((
        payload
            .checked_add(std::mem::size_of::<RuntimeCommand>())
            .ok_or(RuntimeFailure::capacity(RuntimeStage::Admission))?,
        input,
    ))
}

pub(crate) fn validate_command_fence(
    fence: EffectFence,
    command: &RuntimeCommand,
) -> Result<(), RuntimeFailure> {
    let pointer = match command {
        RuntimeCommand::Pointer(pointer) => Some((pointer.stamp, pointer.cell)),
        RuntimeCommand::Wheel(wheel) => Some((wheel.stamp, wheel.cell)),
        _ => None,
    };
    if pointer.is_some_and(|(stamp, cell)| {
        stamp.fence() != fence
            || cell.session != fence.session()
            || !stamp.base().contains(cell.row)
    }) {
        return Err(RuntimeFailure::new(
            spaceterm_core::organization::FailureClass::InvalidRequest,
            RuntimeStage::Admission,
        ));
    }
    Ok(())
}
