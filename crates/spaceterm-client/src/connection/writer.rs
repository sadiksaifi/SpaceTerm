use super::{DispatchRequest, stale};
use crate::{
    api::{ConnectedInfo, ConnectionState},
    failure::{ClientFailure, invalid_peer, resource_limit},
    limits::DIAL_DEADLINE,
    ports::{ConnectionDelegate, MessageSender, OutboundClass},
    protocol_adapter::unavailable,
    registry::{EgressBudget, EgressReservation, cancelled, same_connection},
};
use spaceterm_protocol::{
    codec::Payload, connection::AuthenticatedConnection, limits::NegotiatedLimits,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, watch};

struct WriterCompletion(watch::Sender<bool>);
impl Drop for WriterCompletion {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

type DelegateBinding = (Arc<dyn ConnectionDelegate>, ConnectedInfo);
struct Write {
    message: Payload,
    delegate: Option<DelegateBinding>,
    state: Option<watch::Receiver<ConnectionState>>,
    query: Option<(
        Arc<super::queries::QueryHub>,
        crate::api::PairedServerBinding,
    )>,
    completion: Option<oneshot::Sender<Result<(), ClientFailure>>>,
    _reservation: EgressReservation,
}

/// Every authenticated message crosses this bounded writer. Sequence allocation
/// happens at final dequeue, and only this task waits for native send completion.
pub(crate) struct Writer {
    queue: mpsc::Sender<Write>,
    task: tokio::task::JoinHandle<()>,
    pub failure: watch::Receiver<Option<ClientFailure>>,
    pub done: watch::Receiver<bool>,
    budget: Arc<EgressBudget>,
    limits: NegotiatedLimits,
}
impl Writer {
    pub fn new(
        codec: Arc<Mutex<AuthenticatedConnection>>,
        sender: Arc<dyn MessageSender>,
        limits: NegotiatedLimits,
        budget: Arc<EgressBudget>,
    ) -> Self {
        let (queue, mut incoming) =
            mpsc::channel::<Write>(spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES);
        let (failed, failure) = watch::channel(None);
        let (finished, done) = watch::channel(false);
        let completion = WriterCompletion(finished);
        let task = tokio::spawn(async move {
            let _completion = completion;
            while let Some(write) = incoming.recv().await {
                let admission = (|| {
                    if let Some(state) = &write.state {
                        let (_, captured) = write.delegate.as_ref().ok_or_else(invalid_peer)?;
                        if !matches!(&*state.borrow(), ConnectionState::Connected { info } if same_connection(*info, *captured))
                        {
                            return Err(stale());
                        }
                    }
                    if let Some((delegate, info)) = &write.delegate {
                        if let Some((queries, binding)) = &write.query {
                            queries.admit(*info, &write.message, binding)?;
                        } else {
                            delegate.admit(*info, &write.message)?;
                        }
                    }
                    Ok(())
                })();
                let encoded = admission.and_then(|()| {
                    codec
                        .lock()
                        .map_err(|_| invalid_peer())?
                        .encode(&write.message)
                        .map_err(Into::into)
                });
                let (result, transport_failed) = match encoded {
                    Ok(bytes) => {
                        let result = tokio::time::timeout(DIAL_DEADLINE, sender.send(&bytes))
                            .await
                            .unwrap_or_else(|_| Err(unavailable()));
                        let transport_failed = result.is_err();
                        (result, transport_failed)
                    }
                    Err(error) => (Err(error), false),
                };
                // Callback acknowledgements have already returned queue admission.
                // A final rejection must still fail the connection, not disappear.
                let fatal = transport_failed || (write.completion.is_none() && result.is_err());
                let error = result.as_ref().err().copied();
                if let Some(completion) = write.completion {
                    let _ = completion.send(result);
                }
                if fatal {
                    failed.send_replace(error);
                    break;
                }
            }
        });
        Self {
            queue,
            task,
            failure,
            done,
            budget,
            limits,
        }
    }
    pub fn enqueue(
        &self,
        message: &Payload,
        delegate: Option<DelegateBinding>,
    ) -> Result<(), ClientFailure> {
        if let Some(error) = *self.failure.borrow() {
            return Err(error);
        }
        let class = if let Some((delegate, info)) = &delegate {
            delegate.admit(*info, message)?
        } else {
            OutboundClass::Lifecycle
        };
        let bytes = message
            .encode_with_limits(self.limits)?
            .len()
            .checked_add(512)
            .ok_or_else(resource_limit)?;
        let reservation = self.budget.reserve(bytes, class)?;
        self.push(Write {
            message: message.clone(),
            delegate,
            state: None,
            query: None,
            completion: None,
            _reservation: reservation,
        })
    }
    pub fn dispatch(
        &self,
        request: DispatchRequest,
        delegate: Arc<dyn ConnectionDelegate>,
        state: watch::Receiver<ConnectionState>,
        queries: Arc<super::queries::QueryHub>,
        binding: crate::api::PairedServerBinding,
    ) -> Result<(), ClientFailure> {
        self.push(Write {
            message: request.message,
            delegate: Some((delegate, request.captured)),
            state: Some(state),
            query: request.query.then_some((queries, binding)),
            completion: Some(request.completion),
            _reservation: request._reservation,
        })
    }
    fn push(&self, write: Write) -> Result<(), ClientFailure> {
        self.queue.try_send(write).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => resource_limit(),
            mpsc::error::TrySendError::Closed(_) => cancelled(),
        })
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
