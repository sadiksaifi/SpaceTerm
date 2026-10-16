//! Converts authenticated requests into the existing desktop native-service flows.
use super::*;
use crate::terminal::osc52::{Osc52Operation, Osc52Target, Osc52Terminator};
use pane::effects::{ClipboardTarget, Completion, Effect};

pub(super) struct ClientEffects {
    clipboard: async_channel::Sender<ClipboardRequest>,
    completed: mpsc::Receiver<(u64, ClipboardCompletion)>,
    complete: mpsc::SyncSender<(u64, ClipboardCompletion)>,
    pending: Option<(u64, u64, u64)>,
    hidden_input: Option<(u64, bool)>,
}

impl ClientEffects {
    pub(super) fn new() -> (Self, async_channel::Receiver<ClipboardRequest>) {
        let (clipboard, requests) = async_channel::bounded(1);
        let (complete, completed) = mpsc::sync_channel(1);
        (
            Self {
                clipboard,
                complete,
                completed,
                pending: None,
                hidden_input: None,
            },
            requests,
        )
    }

    pub(super) fn poll(
        &mut self,
        state: &DevelopmentPaneState,
        lease: Lease,
        events: &async_channel::Sender<TerminalSessionEvent>,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<(), ServerError> {
        let generation = state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wire_focus()
            .0;
        while let Ok((id, completion)) = self.completed.try_recv() {
            if let Some((pending, old_generation, epoch)) = self.pending
                && pending == id
                && old_generation == generation
                && state.effect_authority.native_permits(epoch)
            {
                self.pending = None;
                let completion = match completion {
                    ClipboardCompletion::Denied => Completion::Denied,
                    ClipboardCompletion::Written => Completion::Written,
                    ClipboardCompletion::Text(text) => Completion::Text(text),
                };
                expect_unchanged(exchange(&Request::CompleteClipboard {
                    lease,
                    generation,
                    id,
                    completion,
                })?)?;
            }
        }
        let local_epoch = state.effect_authority.output_epoch();
        let started = std::time::Instant::now();
        let Reply::NativeEffects {
            request,
            hidden_input,
        } = exchange(&Request::NativeEffects { lease, generation })?
        else {
            return Err(ServerError::Protocol);
        };
        state.hidden_input.store(hidden_input, Ordering::Release);
        let hidden_state = (state.epoch.load(Ordering::Acquire), hidden_input);
        if self.hidden_input != Some(hidden_state) {
            events
                .force_send(TerminalSessionEvent::HiddenInputChanged(hidden_input))
                .map_err(|_| ServerError::NotRunning)?;
            self.hidden_input = Some(hidden_state);
        }
        let Some(request) = request else {
            return Ok(());
        };
        let current_generation = state
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wire_focus()
            .0;
        let Some(epoch) = local_epoch.filter(|epoch| state.effect_authority.native_permits(*epoch))
        else {
            return Ok(());
        };
        if current_generation != generation
            || request.generation != generation
            || request.remaining_ms == 0
            || request.remaining_ms > 1000
        {
            return Ok(());
        }
        let Some(remaining) = Duration::from_millis(u64::from(request.remaining_ms))
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
        else {
            return Ok(());
        };
        let operation = match request.effect {
            Effect::ClipboardRead { target } => Some(Osc52Operation::Read {
                target: decode_target(target),
                terminator: Osc52Terminator::StringTerminator,
            }),
            Effect::ClipboardWrite { target, text }
                if text.len() <= pane::effects::MAX_CLIPBOARD_BYTES =>
            {
                Some(Osc52Operation::Write {
                    target: decode_target(target),
                    text,
                })
            }
            Effect::ClipboardWrite { .. } => return Err(ServerError::Protocol),
            effect => {
                let request = ClientEffectRequest::new(
                    effect,
                    Arc::clone(&state.effect_authority),
                    epoch,
                    remaining,
                );
                let _ = events.try_send(TerminalSessionEvent::ClientEffect(request));
                None
            }
        };
        if let Some(operation) = operation {
            self.pending = Some((request.id, generation, epoch));
            let request = ClipboardRequest::relayed(
                request.id,
                operation,
                Arc::clone(&state.effect_authority),
                epoch,
                remaining,
                self.complete.clone(),
            );
            let _ = self.clipboard.try_send(request);
        }
        Ok(())
    }
}

fn decode_target(target: ClipboardTarget) -> Osc52Target {
    match target {
        ClipboardTarget::Default => Osc52Target::Default,
        ClipboardTarget::Standard => Osc52Target::Standard,
        ClipboardTarget::Selection => Osc52Target::Selection,
        ClipboardTarget::Primary => Osc52Target::Primary,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Adapter;
    use super::*;
    use crate::terminal::native_services::{clipboard::ClipboardPreferences, testing::adapters};

    #[gpui::test]
    fn relayed_clipboard_uses_client_preferences_and_rejects_delayed_local_authority(
        cx: &mut gpui::TestAppContext,
    ) {
        for revoke in 0..4 {
            let adapter = Adapter::new();
            let (mut effects, requests) = ClientEffects::new();
            let (events, _receiver) = async_channel::bounded(8);
            let state = &adapter.handle.state;
            let lease = adapter.active.lease;
            let generation = state.interaction.lock().unwrap().focus_generation;
            effects
                .poll(state, lease, &events, &mut |request| {
                    assert!(matches!(request, Request::NativeEffects { .. }));
                    Ok(Reply::NativeEffects {
                        hidden_input: false,
                        request: Some(pane::effects::Request {
                            id: 1,
                            generation,
                            remaining_ms: 1000,
                            effect: Effect::ClipboardWrite {
                                target: ClipboardTarget::Standard,
                                text: "native fixture".into(),
                            },
                        }),
                    })
                })
                .unwrap();
            let request = requests.try_recv().unwrap();
            if revoke == 1 {
                adapter.handle.interactive_focus(false, false);
                adapter.handle.interactive_focus(true, true);
            }
            if revoke == 2 {
                state.invalidate();
            }
            cx.update(|cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into()));
                request
                    .perform(
                        ClipboardPreferences {
                            allow_write: revoke != 3,
                            allow_read: false,
                        },
                        adapters().text_clipboard.as_ref(),
                        true,
                        cx,
                    )
                    .detach();
            });
            cx.run_until_parked();
            let text = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
            assert_eq!(
                text.as_deref(),
                Some(if revoke == 0 {
                    "native fixture"
                } else {
                    "untouched"
                })
            );
        }
    }

    #[test]
    fn a_reply_crossing_local_focus_loss_never_reaches_the_native_effect_queue() {
        let adapter = Adapter::new();
        let (mut effects, requests) = ClientEffects::new();
        let (events, _receiver) = async_channel::bounded(8);
        let generation = adapter
            .handle
            .state
            .interaction
            .lock()
            .unwrap()
            .focus_generation;
        effects
            .poll(
                &adapter.handle.state,
                adapter.active.lease,
                &events,
                &mut |_| {
                    adapter.handle.interactive_focus(false, false);
                    adapter.handle.interactive_focus(true, true);
                    Ok(Reply::NativeEffects {
                        hidden_input: true,
                        request: Some(pane::effects::Request {
                            id: 1,
                            generation,
                            remaining_ms: 1000,
                            effect: Effect::ClipboardRead {
                                target: ClipboardTarget::Standard,
                            },
                        }),
                    })
                },
            )
            .unwrap();
        assert!(requests.try_recv().is_err());
        assert_eq!(adapter.handle.hidden_input(), Some(true));
    }
}
