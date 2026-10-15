//! Background persistence for the Development client's arrangements, outside Settings.
use spaceterm_server::client_view::ClientView;
use spaceterm_server::{LocalServer, ServerError};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    loaded: Option<Result<ClientView, ServerError>>,
    pending: Option<ClientView>,
    error: Option<ServerError>,
    closed: bool,
}

/// One worker owns the storage lock. Dropping the window flushes its latest snapshot.
pub(super) struct RetainedClientViews {
    state: Arc<Mutex<State>>,
    wake: std::sync::mpsc::SyncSender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl RetainedClientViews {
    pub(super) fn new(server: LocalServer) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let (wake, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("spaceterm-client-view".into())
            .spawn(move || {
                let opened = server.client_view_store();
                {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    state.loaded = Some(
                        opened
                            .as_ref()
                            .map(|store| store.view().clone())
                            .map_err(|error| *error),
                    );
                }
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
        }
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
