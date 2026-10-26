//! Background persistence for this client's one Workspace order across servers, outside
//! Settings.
use spaceterm_server::workspace_order::WorkspaceOrder;
use spaceterm_server::{ServerConnection, ServerError};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    loaded: Option<Result<WorkspaceOrder, ServerError>>,
    pending: Option<WorkspaceOrder>,
    error: Option<ServerError>,
    closed: bool,
}

/// One worker owns the storage lock. Dropping the window flushes its latest order.
pub(super) struct RetainedWorkspaceOrder {
    state: Arc<Mutex<State>>,
    wake: std::sync::mpsc::SyncSender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Closes once the saved order is read, or reading failed.
    read: async_channel::Receiver<()>,
}

impl RetainedWorkspaceOrder {
    /// Opens the order `server`, the account-local connection, retains. Opening needs no
    /// running server.
    pub(super) fn new(server: ServerConnection) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let (wake, receiver) = std::sync::mpsc::sync_channel(1);
        let (read_done, read) = async_channel::bounded::<()>(1);
        let worker = std::thread::Builder::new()
            .name("spaceterm-workspace-order".into())
            .spawn(move || {
                let opened = server.workspace_order_store();
                shared.lock().unwrap_or_else(|e| e.into_inner()).loaded = Some(
                    opened
                        .as_ref()
                        .map(|store| store.order().clone())
                        .map_err(|error| *error),
                );
                drop(read_done);
                let Ok(mut store) = opened else {
                    return;
                };
                loop {
                    let (pending, closed) = {
                        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                        (state.pending.take(), state.closed)
                    };
                    if let Some(order) = pending
                        && let Err(error) = store.save(order)
                    {
                        shared.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error);
                        return;
                    }
                    if closed {
                        return;
                    }
                    if receiver.recv().is_err() {
                        return;
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(_) => {
                state.lock().unwrap_or_else(|e| e.into_inner()).loaded =
                    Some(Err(ServerError::StorageUnavailable));
                None
            }
        };
        Self {
            state,
            wake,
            worker,
            read,
        }
    }

    /// Completes once the saved order can be read.
    pub(super) fn read(&self) -> impl std::future::Future<Output = ()> + 'static {
        let read = self.read.clone();
        async move {
            // Only the worker sends nothing and then closes, so any result means reading ended.
            let _ = read.recv().await;
        }
    }

    pub(super) fn loaded(&self) -> Option<Result<WorkspaceOrder, ServerError>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .clone()
    }

    /// Why the saved order couldn't be read, once reading ended.
    pub(super) fn read_error(&self) -> Option<ServerError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.as_ref().err().copied())
    }

    /// Why the worker stopped saving, after which nothing more is written.
    pub(super) fn error(&self) -> Option<ServerError> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).error
    }

    /// Queues `order` for the worker, which writes only the latest one.
    pub(super) fn save(&self, order: WorkspaceOrder) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.error.is_some() {
            return;
        }
        state.pending = Some(order);
        let _ = self.wake.try_send(());
    }
}

impl Drop for RetainedWorkspaceOrder {
    fn drop(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
