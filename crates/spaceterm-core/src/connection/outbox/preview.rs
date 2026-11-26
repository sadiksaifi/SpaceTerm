//! A Pane preview stays revocable while the connection can send more of it.

use super::*;
use crate::connection::carrier::{Carrier, CarrierError, Flush, Offer};

/// Settles a specific preview read. End All Sessions can retire it before the worker answers.
#[derive(Clone)]
pub struct PreviewReply {
    outbox: Outbox,
    id: RequestId,
    token: Arc<()>,
}

impl PreviewReply {
    pub fn send(self, result: Result<ReplyBody, Refusal>) {
        let sent = self
            .outbox
            .reply_with_preview(self.id, result, Some(&self.token));
        if let Err(OutboxError::Encode(error)) = sent {
            let refusal = match error {
                EncodeError::TooLarge => Refusal::Capacity,
                EncodeError::Unencodable => Refusal::Failed,
            };
            let _ = self
                .outbox
                .reply_with_preview(self.id, Err(refusal), Some(&self.token));
        }
    }
}

impl Outbox {
    pub fn preview_reply(&self, id: RequestId) -> Result<PreviewReply, OutboxError> {
        let mut state = self.lock();
        state.open()?;
        if state.requests.get(&id) != Some(&false) || state.previews.contains_key(&id) {
            return Err(OutboxError::Misuse);
        }
        let token = Arc::new(());
        state.previews.insert(id, Arc::clone(&token));
        Ok(PreviewReply {
            outbox: self.clone(),
            id,
            token,
        })
    }

    /// End All Sessions clears every preview that was still in flight when it took effect.
    pub fn discard_previews(&self) {
        let _ = self.with(|state| {
            let previews = std::mem::take(&mut state.previews);
            if state.writing_preview || state.buffered_preview {
                // A partial bulk message must never be completed after the clear. There is no
                // frame that can replace its suffix, so the connection ends without draining.
                state.aborted = true;
                state.closing = true;
                state.l2_rest.clear();
            }
            let mut discarded = 0;
            state.l2.retain(|(id, frames)| {
                if previews.contains_key(id) {
                    discarded += size(frames);
                    false
                } else {
                    true
                }
            });
            state.l2_bytes -= discarded;
            if !state.closing {
                for id in previews.into_keys() {
                    let frames = encode(&CoreMessage::Reply(Box::new(Reply {
                        id,
                        result: Err(Refusal::Stale),
                    })))?;
                    state.push_l1(Entry::Message(frames, Some(id)))?;
                    state.requests.insert(id, true);
                }
            }
            Ok(())
        });
    }

    pub(crate) fn aborted(&self) -> bool {
        self.lock().aborted
    }

    /// The clear and an L2 offer share the queue lock, including retries of a held frame.
    pub(crate) fn offer(
        &self,
        carrier: &mut dyn Carrier,
        frame: &[u8],
        level: Priority,
    ) -> Result<Offer, CarrierError> {
        let mut state = self.lock();
        if state.aborted {
            return Err(CarrierError::Closed);
        }
        if level != Priority::L2 || !state.writing_preview {
            drop(state);
            return carrier.offer(frame, level);
        }
        let offered = carrier.offer(frame, level)?;
        if offered == Offer::Accepted {
            state.buffered_preview = true;
            if state.l2_rest.is_empty() {
                state.writing_preview = false;
            }
        }
        Ok(offered)
    }

    pub(crate) fn flush(&self, carrier: &mut dyn Carrier) -> Result<Flush, CarrierError> {
        let mut state = self.lock();
        if state.aborted {
            return Err(CarrierError::Closed);
        }
        if !state.buffered_preview {
            drop(state);
            return carrier.flush();
        }
        let flushed = carrier.flush()?;
        state.buffered_preview = flushed.pending || carrier.bulk_may_retransmit();
        Ok(flushed)
    }
}
