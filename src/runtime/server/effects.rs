use super::*;
use crate::terminal::session::ClientEffectRequest;
use pane::effects::{ClipboardTarget, Completion, Effect};
use std::collections::VecDeque;

/// How long an effect the server itself sends waits for its client, like a terminal's own.
const SERVER_EFFECT_LIFETIME: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Default)]
pub(super) struct NativeEffects {
    next_id: u64,
    clipboard: Option<(u64, ClipboardRequest, bool)>,
    events: VecDeque<ClientEffectRequest>,
    /// Effects the server sends, such as a Reveal, each with its deadline.
    server: VecDeque<(Effect, std::time::Instant)>,
}

impl DevelopmentPaneHost {
    /// Queues `effect` for this Pane's channel when `controller` controls its running Session.
    /// No other client receives it.
    pub(super) fn send_server_effect(&mut self, controller: [u8; 32], effect: Effect) {
        if let Some(pane) = &mut self.pane
            && pane.device == Some(controller)
        {
            pane.effects.push_server(effect);
        }
    }
}

impl NativeEffects {
    pub(super) fn clear(&mut self) {
        self.clipboard = None;
        self.events.clear();
        self.server.clear();
    }
    fn push_server(&mut self, effect: Effect) {
        let now = std::time::Instant::now();
        self.server.retain(|(_, deadline)| *deadline > now);
        if self.server.len() < 8 {
            self.server
                .push_back((effect, now + SERVER_EFFECT_LIFETIME));
        }
    }
    pub(super) fn push(&mut self, request: ClientEffectRequest) {
        if request.grant.current() && self.events.len() < 8 {
            self.events.push_back(request);
        }
    }
    pub(super) fn drain_clipboard(&mut self, requests: &async_channel::Receiver<ClipboardRequest>) {
        if self
            .clipboard
            .as_ref()
            .is_some_and(|(_, request, _)| request.remaining().is_none())
        {
            self.clipboard = None;
        }
        if self.clipboard.is_none()
            && let Ok(request) = requests.try_recv()
        {
            self.next_id = self.next_id.saturating_add(1);
            self.clipboard = Some((self.next_id, request, false));
        }
    }
    pub(super) fn next(&mut self, generation: u64) -> Option<pane::effects::Request> {
        if let Some((id, request, sent)) = &mut self.clipboard
            && !*sent
            && let Some(remaining) = request.remaining()
        {
            *sent = true;
            let effect = match request.operation() {
                Osc52Operation::Read { target, .. } => Effect::ClipboardRead {
                    target: encode_target(*target),
                },
                Osc52Operation::Write { target, text } => Effect::ClipboardWrite {
                    target: encode_target(*target),
                    text: text.clone(),
                },
            };
            return Some(pane::effects::Request {
                id: *id,
                generation,
                remaining_ms: remaining.as_millis().min(1000) as u32,
                effect,
            });
        }
        while let Some(request) = self.events.pop_front() {
            if let Some(remaining) = request.grant.remaining() {
                self.next_id = self.next_id.saturating_add(1);
                return Some(pane::effects::Request {
                    id: self.next_id,
                    generation,
                    remaining_ms: remaining.as_millis().min(1000) as u32,
                    effect: request.effect,
                });
            }
        }
        while let Some((effect, deadline)) = self.server.pop_front() {
            if let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                self.next_id = self.next_id.saturating_add(1);
                return Some(pane::effects::Request {
                    id: self.next_id,
                    generation,
                    remaining_ms: remaining.as_millis().min(1000) as u32,
                    effect,
                });
            }
        }
        None
    }
    pub(super) fn complete(&mut self, id: u64, completion: Completion) {
        let Some((pending, request, sent)) = &mut self.clipboard else {
            return;
        };
        if *pending != id || !*sent || request.remaining().is_none() {
            return;
        }
        let completion = match completion {
            Completion::Denied => ClipboardCompletion::Denied,
            Completion::Written => ClipboardCompletion::Written,
            Completion::Text(text) if text.len() <= pane::effects::MAX_CLIPBOARD_BYTES => {
                ClipboardCompletion::Text(text)
            }
            Completion::Text(_) => ClipboardCompletion::Denied,
        };
        request.complete(completion);
        self.clipboard = None;
    }
}

fn encode_target(target: Osc52Target) -> ClipboardTarget {
    match target {
        Osc52Target::Default => ClipboardTarget::Default,
        Osc52Target::Standard => ClipboardTarget::Standard,
        Osc52Target::Selection => ClipboardTarget::Selection,
        Osc52Target::Primary => ClipboardTarget::Primary,
    }
}
