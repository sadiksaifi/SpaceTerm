//! Background persistence for this client's arrangements of shared Workspaces, outside Settings.
use spaceterm_server::client_view::ClientView;
use spaceterm_server::{ServerConnection, ServerError};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    loaded: Option<Result<ClientView, ServerError>>,
    attachments: Option<Result<spaceterm_server::attachments::Attachments, ServerError>>,
    pending: Option<ClientView>,
    error: Option<ServerError>,
    closed: bool,
}

/// One worker owns the storage lock. Dropping the window flushes its latest snapshot.
pub(super) struct RetainedClientViews {
    state: Arc<Mutex<State>>,
    wake: std::sync::mpsc::SyncSender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Closes once the saved arrangement and attachments are read, or reading failed.
    read: async_channel::Receiver<()>,
}
impl RetainedClientViews {
    pub(super) fn new(server: ServerConnection) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let (wake, receiver) = std::sync::mpsc::sync_channel(1);
        let (read_done, read) = async_channel::bounded::<()>(1);
        let worker = std::thread::Builder::new()
            .name("spaceterm-client-view".into())
            .spawn(move || {
                let attachments = server.attachments();
                let opened = server.client_view_store();
                {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    state.attachments = Some(attachments);
                    state.loaded = Some(
                        opened
                            .as_ref()
                            .map(|store| store.view().clone())
                            .map_err(|error| *error),
                    );
                }
                // The window waits on this before its first inventory, so it hears at once.
                drop(read_done);
                let Ok(mut store) = opened else {
                    return;
                };
                loop {
                    let (pending, closed) = {
                        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                        (state.pending.take(), state.closed)
                    };
                    if let Some(view) = pending
                        && let Err(error) = store.save(view)
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
    /// Completes once the saved arrangement and attachments can be read.
    pub(super) fn read(&self) -> impl std::future::Future<Output = ()> + 'static {
        let read = self.read.clone();
        async move {
            // Only the worker sends nothing and then closes, so any result means reading ended.
            let _ = read.recv().await;
        }
    }
    pub(super) fn attachments(
        &self,
    ) -> Option<Result<spaceterm_server::attachments::Attachments, ServerError>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attachments
            .clone()
    }
    pub(super) fn loaded(&self) -> Option<Result<ClientView, ServerError>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .clone()
    }
    pub(super) fn save(&self, view: ClientView) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.error.is_some() {
            return;
        }
        state.pending = Some(view);
        let _ = self.wake.try_send(());
    }
    pub(super) fn read_error(&self) -> Option<ServerError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.as_ref().err().copied())
    }
    pub(super) fn error(&self) -> Option<ServerError> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).error
    }
}
impl Drop for RetainedClientViews {
    fn drop(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
