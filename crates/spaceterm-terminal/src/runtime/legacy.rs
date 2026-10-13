//! Local and SSH compatibility ownership without server identities or grants.
use super::budget::{Budget, BudgetLease, REPLY_INPUT_RESERVE, RuntimeLimits};
use super::ports::{
    PtyExit, PtyParts, PtyProcess, PtyReadEvent, PtyReaderPump, PtySize, PtyTermination,
    RuntimeFailure, RuntimeStage, cleanup_pty,
};
use spaceterm_core::organization::FailureClass;
use spaceterm_core::retirement::{CleanupResult, ConfirmedCleanupClass};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyRuntimeCompletion {
    pub result: Result<(), RuntimeFailure>,
    pub cleanup: CleanupResult,
}
struct CompletionState {
    value: Option<LegacyRuntimeCompletion>,
    joined: bool,
    process_started: bool,
    cleanup: Option<CleanupResult>,
}
struct Shared {
    retiring: AtomicBool,
    state: Mutex<CompletionState>,
    changed: Condvar,
}
impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            retiring: AtomicBool::new(false),
            state: Mutex::new(CompletionState {
                value: None,
                joined: false,
                process_started: false,
                cleanup: None,
            }),
            changed: Condvar::new(),
        })
    }
}
struct Factory {
    limits: RuntimeLimits,
    budget: Arc<Budget>,
    registry: mpsc::SyncSender<Registration>,
    ingress: Arc<Budget>,
    input: Arc<Budget>,
    client_input: Arc<Budget>,
    // Unconfirmed native cleanup never makes its capacity available to another launch.
    quarantine: Mutex<Vec<BudgetLease>>,
}
struct Registration {
    thread: JoinHandle<()>,
    shared: Arc<Shared>,
    lease: BudgetLease,
    factory: Arc<Factory>,
}
/// A bounded cleanup registry. It owns every worker join from startup, not just after close.
#[derive(Clone)]
pub struct LegacyRuntimeFactory(Arc<Factory>);
impl LegacyRuntimeFactory {
    pub fn new(limits: RuntimeLimits) -> Result<Self, RuntimeFailure> {
        let limits = limits.validate()?;
        let (registry, receiver) = mpsc::sync_channel::<Registration>(limits.owners);
        thread::Builder::new()
            .name("terminal-legacy-cleanup".into())
            .spawn(move || {
                let mut pending = Vec::<Registration>::new();
                let mut disconnected = false;
                loop {
                    if !disconnected {
                        match receiver.recv_timeout(Duration::from_millis(10)) {
                            Ok(registration) => pending.push(registration),
                            Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    } else {
                        thread::sleep(Duration::from_millis(10));
                    }
                    let mut index = 0;
                    while index < pending.len() {
                        if !pending[index].thread.is_finished() {
                            index += 1;
                            continue;
                        }
                        let registration = pending.swap_remove(index);
                        let joined = registration.thread.join().is_ok();
                        let mut state = registration
                            .shared
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if !joined {
                            state.value = Some(LegacyRuntimeCompletion {
                                result: Err(RuntimeFailure::new(
                                    FailureClass::RuntimeFault,
                                    RuntimeStage::Cleanup,
                                )),
                                cleanup: CleanupResult::Failed(FailureClass::RuntimeUnavailable),
                            });
                        }
                        if state.value.is_some_and(|completion| {
                            !matches!(completion.cleanup, CleanupResult::Confirmed(_))
                        }) {
                            registration
                                .factory
                                .quarantine
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(registration.lease);
                        } else {
                            drop(registration.lease);
                        }
                        state.joined = true;
                        registration.shared.changed.notify_all();
                    }
                    if disconnected && pending.is_empty() {
                        break;
                    }
                }
            })
            .map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Cleanup)
            })?;
        Ok(Self(Arc::new(Factory {
            limits,
            budget: Budget::new(limits.owners, limits.active_grid_slots),
            registry,
            ingress: Budget::new(0, limits.aggregate_command_bytes),
            input: Budget::new(0, limits.aggregate_pending_input_bytes),
            client_input: Budget::new(
                0,
                limits.aggregate_pending_input_bytes - limits.owners * REPLY_INPUT_RESERVE,
            ),
            quarantine: Mutex::new(Vec::new()),
        })))
    }
    /// The driver is the legacy projection only. Its process uses the same owning ports,
    /// reader pump and confirmed cleanup as server runtimes, without server authority DTOs.
    pub fn start(
        &self,
        size: PtySize,
        driver: impl FnOnce(LegacyRuntimeContext) -> Result<(), RuntimeFailure> + Send + 'static,
    ) -> Result<LegacyRuntimeOwner, RuntimeFailure> {
        let cells = self.cells(size)?;
        let lease = self.0.budget.reserve(1, 0, RuntimeStage::Admission)?;
        let grid = self
            .0
            .budget
            .reserve(0, cells * 4, RuntimeStage::Admission)?;
        let shared = Shared::new();
        let worker_shared = Arc::clone(&shared);
        let factory = Arc::clone(&self.0);
        let thread = thread::Builder::new()
            .name("terminal-legacy".into())
            .spawn(move || {
                let context = LegacyRuntimeContext {
                    shared: Arc::clone(&worker_shared),
                    factory: Arc::clone(&factory),
                    size,
                    cells,
                    grid: std::cell::RefCell::new(Some(grid)),
                    input: LegacyInputBudget {
                        local: Budget::new(0, factory.limits.pending_input_bytes),
                        aggregate: Arc::clone(&factory.input),
                        client: Budget::new(
                            0,
                            factory.limits.pending_input_bytes - REPLY_INPUT_RESERVE,
                        ),
                        client_aggregate: Arc::clone(&factory.client_input),
                    },
                    _worker: std::marker::PhantomData,
                };
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| driver(context)))
                        .unwrap_or_else(|_| {
                            Err(RuntimeFailure::new(
                                FailureClass::RuntimeFault,
                                RuntimeStage::Engine,
                            ))
                        });
                let mut state = worker_shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let cleanup = state.cleanup.unwrap_or_else(|| {
                    if state.process_started {
                        CleanupResult::Failed(FailureClass::RuntimeUnavailable)
                    } else {
                        CleanupResult::Confirmed(
                            ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                        )
                    }
                });
                state.value = Some(LegacyRuntimeCompletion { result, cleanup });
                worker_shared.changed.notify_all();
            })
            .map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Launch)
            })?;
        // At most owners workers can hold capacity. Registry capacity therefore always fits
        // all workers, including those whose owners close on the UI thread.
        if let Err(error) = self.0.registry.send(Registration {
            thread,
            shared: Arc::clone(&shared),
            lease,
            factory: Arc::clone(&self.0),
        }) {
            shared.retiring.store(true, Ordering::Release);
            let _ = error.0.thread.join();
            return Err(RuntimeFailure::internal(RuntimeStage::Cleanup));
        }
        Ok(LegacyRuntimeOwner {
            handle: LegacyRuntimeHandle(shared),
        })
    }
    fn cells(&self, size: PtySize) -> Result<usize, RuntimeFailure> {
        cells(size, self.0.limits)
    }
}
fn cells(size: PtySize, limits: RuntimeLimits) -> Result<usize, RuntimeFailure> {
    let count = usize::from(size.rows)
        .checked_mul(usize::from(size.columns))
        .ok_or(RuntimeFailure::capacity(RuntimeStage::Admission))?;
    if size.rows == 0
        || size.columns == 0
        || u32::from(size.rows) > limits.grid.max_rows()
        || u32::from(size.columns) > limits.grid.max_columns()
        || count > limits.grid.max_cells() as usize
    {
        return Err(RuntimeFailure::capacity(RuntimeStage::Admission));
    }
    Ok(count)
}
/// Non-cloneable legacy lifetime authority. Closing requests retirement, never joins the UI.
pub struct LegacyRuntimeOwner {
    handle: LegacyRuntimeHandle,
}
impl LegacyRuntimeOwner {
    pub fn handle(&self) -> LegacyRuntimeHandle {
        self.handle.clone()
    }
    pub fn retire(&self) {
        self.handle.retire();
    }
    /// Waits for actual worker joining. Use only on a blocking thread.
    pub fn join(&self) -> Result<LegacyRuntimeCompletion, RuntimeFailure> {
        self.handle.join()
    }
}
impl Drop for LegacyRuntimeOwner {
    fn drop(&mut self) {
        self.retire();
    }
}
/// Dropping this handle is inert. Explicit close belongs to the compatibility owner.
#[derive(Clone)]
pub struct LegacyRuntimeHandle(Arc<Shared>);
impl LegacyRuntimeHandle {
    pub fn retire(&self) {
        self.0.retiring.store(true, Ordering::Release);
    }
    pub fn retirement_requested(&self) -> bool {
        self.0.retiring.load(Ordering::Acquire)
    }
    pub fn completion(&self) -> Option<LegacyRuntimeCompletion> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.joined.then_some(state.value).flatten()
    }
    pub fn join(&self) -> Result<LegacyRuntimeCompletion, RuntimeFailure> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        while !state.joined {
            state = self
                .0
                .changed
                .wait(state)
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Cleanup))?;
        }
        state
            .value
            .ok_or(RuntimeFailure::internal(RuntimeStage::Cleanup))
    }
}
pub struct LegacyRuntimeContext {
    shared: Arc<Shared>,
    factory: Arc<Factory>,
    size: PtySize,
    cells: usize,
    grid: std::cell::RefCell<Option<BudgetLease>>,
    input: LegacyInputBudget,
    _worker: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl LegacyRuntimeContext {
    pub fn handle(&self) -> LegacyRuntimeHandle {
        LegacyRuntimeHandle(Arc::clone(&self.shared))
    }
    pub fn input_budget(&self) -> LegacyInputBudget {
        self.input.clone()
    }
    pub fn retirement_requested(&self) -> bool {
        self.handle().retirement_requested()
    }
    pub fn launch<T: Send + 'static>(
        &self,
        launch: impl FnOnce(PtySize) -> Result<PtyParts, RuntimeFailure>,
        capture: impl Fn() -> Result<T, RuntimeFailure> + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Result<LegacyPty<T>, RuntimeFailure> {
        if self.retirement_requested() {
            return Err(RuntimeFailure::stale());
        }
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| RuntimeFailure::internal(RuntimeStage::Launch))?;
            if state.process_started {
                return Err(RuntimeFailure::internal(RuntimeStage::Launch));
            }
            state.process_started = true;
        }
        let parts = match launch(self.size) {
            Ok(parts) => parts,
            Err(error) => {
                if error.stage != RuntimeStage::Cleanup {
                    self.shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cleanup = Some(CleanupResult::Confirmed(
                        ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                    ));
                }
                return Err(error);
            }
        };
        // Install all resources before any reader operation can fail or unwind.
        let mut pty = LegacyPty {
            process: Some(parts.process),
            termination: Some(parts.termination),
            reader: None,
            shared: Arc::clone(&self.shared),
            factory: Arc::clone(&self.factory),
            grid: self.grid.borrow_mut().take(),
            cells: self.cells,
            size: self.size,
            cleaned: None,
            _worker: std::marker::PhantomData,
        };
        let reader = pty
            .process
            .as_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))?
            .take_reader()?;
        pty.reader = Some(PtyReaderPump::spawn(reader, capture, notify)?);
        Ok(pty)
    }
}
/// Owned process and ordered output, used only by the legacy worker thread.
pub struct LegacyPty<T: Send + 'static> {
    process: Option<Box<dyn PtyProcess>>,
    termination: Option<Arc<dyn PtyTermination>>,
    reader: Option<PtyReaderPump<T>>,
    shared: Arc<Shared>,
    factory: Arc<Factory>,
    grid: Option<BudgetLease>,
    cells: usize,
    size: PtySize,
    cleaned: Option<CleanupResult>,
    _worker: std::marker::PhantomData<std::rc::Rc<()>>,
}
pub enum LegacyPtyOutput<T> {
    Bytes(Vec<u8>, T),
    Stopped(Result<(), RuntimeFailure>),
}
impl<T: Send + 'static> LegacyPty<T> {
    pub fn try_output(&mut self) -> Result<Option<LegacyPtyOutput<T>>, RuntimeFailure> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        match reader.try_next()? {
            Some(PtyReadEvent::Bytes { bytes, receipt, .. }) => {
                Ok(Some(LegacyPtyOutput::Bytes(bytes, receipt)))
            }
            Some(PtyReadEvent::Finished) => Ok(Some(LegacyPtyOutput::Stopped(reader.join()?))),
            None => Ok(None),
        }
    }
    pub fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
        self.process
            .as_ref()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Input))?
            .hidden_input()
    }
    pub fn resize(&mut self, size: PtySize) -> Result<(), RuntimeFailure> {
        self.resize_with(size, || Ok(()))
    }
    /// Keeps old grids and reflow capacity charged through both native and engine resize.
    pub fn resize_with(
        &mut self,
        size: PtySize,
        resize_engine: impl FnOnce() -> Result<(), RuntimeFailure>,
    ) -> Result<(), RuntimeFailure> {
        let count = cells(size, self.factory.limits)?;
        if self.shared.retiring.load(Ordering::Acquire) {
            return Err(RuntimeFailure::stale());
        }
        let reservation = if size.rows != self.size.rows || size.columns != self.size.columns {
            Some(self.factory.budget.reserve(
                0,
                count * 4 + self.cells * 2,
                RuntimeStage::Resize,
            )?)
        } else {
            None
        };
        self.process
            .as_ref()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Resize))?
            .resize(size)?;
        resize_engine()?;
        if let Some(mut reservation) = reservation {
            reservation.shrink(count * 4);
            self.grid = Some(reservation);
            self.cells = count;
        }
        self.size = size;
        Ok(())
    }
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<PtyExit, RuntimeFailure> {
        self.process
            .as_mut()
            .ok_or(RuntimeFailure::internal(RuntimeStage::Cleanup))?
            .wait_for_exit(timeout)
    }
    pub fn cleanup(&mut self) -> CleanupResult {
        if let Some(cleaned) = self.cleaned {
            return cleaned;
        }
        let cleaned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cleanup_pty(&mut self.process, &mut self.termination, &mut self.reader)
        }))
        .unwrap_or(CleanupResult::Failed(FailureClass::RuntimeUnavailable));
        if !matches!(cleaned, CleanupResult::Confirmed(_))
            && let Some(grid) = self.grid.take()
        {
            self.factory
                .quarantine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(grid);
        }
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cleanup = Some(cleaned);
        self.cleaned = Some(cleaned);
        cleaned
    }
}
impl<T: Send + 'static> Write for LegacyPty<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.shared.retiring.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.process
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.process
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .flush()
    }
}
impl<T: Send + 'static> Drop for LegacyPty<T> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Payload accounting for the legacy-only projection command lane.
pub trait LegacyCommand {
    fn payload_bytes(&self) -> usize;
    /// Conservative encoded bytes reserved before admitting an input command.
    fn input_bytes(&self) -> usize {
        0
    }
    fn internal(&self) -> bool {
        false
    }
    /// Only zero-payload projection wakeups may coalesce. Input and results never do.
    fn coalescing_key(&self) -> Option<u8> {
        None
    }
}
struct CommandEnvelope<C> {
    command: C,
    _lease: BudgetLease,
    key: Option<u8>,
    input: Option<LegacyInputReservation>,
    sequence: u64,
}
pub struct LegacyCommandSender<C> {
    sender: mpsc::SyncSender<CommandEnvelope<C>>,
    local: Arc<Budget>,
    ordinary: Arc<Budget>,
    aggregate: Arc<Budget>,
    input: LegacyInputBudget,
    sequence: Arc<AtomicU64>,
    wakeups: Arc<Mutex<std::collections::HashSet<u8>>>,
}
impl<C> std::fmt::Debug for LegacyCommandSender<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LegacyCommandSender(..)")
    }
}
impl<C> Clone for LegacyCommandSender<C> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            local: Arc::clone(&self.local),
            ordinary: Arc::clone(&self.ordinary),
            aggregate: Arc::clone(&self.aggregate),
            input: self.input.clone(),
            sequence: Arc::clone(&self.sequence),
            wakeups: Arc::clone(&self.wakeups),
        }
    }
}
impl<C: LegacyCommand> LegacyCommandSender<C> {
    /// Never blocks a UI/native caller. A rejection owns and returns the original command.
    pub fn accepted_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }
    pub fn send(&self, command: C) -> Result<(), mpsc::SendError<C>> {
        self.try_send(command)
            .map_err(|(command, _)| mpsc::SendError(command))
    }
    pub fn try_send(&self, command: C) -> Result<(), (C, RuntimeFailure)> {
        let input = if command.input_bytes() == 0 {
            None
        } else {
            match self.input.reserve(command.input_bytes()) {
                Ok(lease) => Some(LegacyInputReservation::new(lease)),
                Err(error) => return Err((command, error)),
            }
        };
        let bytes = command.payload_bytes();
        let key = command.coalescing_key().filter(|_| bytes == 0);
        let mut wakeups = self.wakeups.lock().unwrap_or_else(|e| e.into_inner());
        if key.is_some_and(|key| wakeups.contains(&key)) {
            return Ok(());
        }
        let lease: Result<BudgetLease, RuntimeFailure> = (|| {
            let local = self.local.reserve(1, bytes, RuntimeStage::Admission)?;
            let aggregate = self.aggregate.reserve(0, bytes, RuntimeStage::Admission)?;
            let lease = local.combine(aggregate);
            if command.internal() {
                Ok(lease)
            } else {
                Ok(lease.combine(self.ordinary.reserve(1, 0, RuntimeStage::Admission)?))
            }
        })();
        let lease: BudgetLease = match lease {
            Ok(lease) => lease,
            Err(error) => return Err((command, error)),
        };
        if let Some(key) = key {
            wakeups.insert(key);
        }
        let Some(sequence) = self.accepted_sequence().checked_add(1) else {
            if let Some(key) = key {
                wakeups.remove(&key);
            }
            return Err((command, RuntimeFailure::internal(RuntimeStage::Admission)));
        };
        self.sender
            .try_send(CommandEnvelope {
                command,
                _lease: lease,
                key,
                input,
                sequence,
            })
            .map(|()| self.sequence.store(sequence, Ordering::Release))
            .map_err(|error| {
                if let Some(key) = key {
                    wakeups.remove(&key);
                }
                match error {
                    mpsc::TrySendError::Full(envelope) => (
                        envelope.command,
                        RuntimeFailure::capacity(RuntimeStage::Admission),
                    ),
                    mpsc::TrySendError::Disconnected(envelope) => (
                        envelope.command,
                        RuntimeFailure::internal(RuntimeStage::Admission),
                    ),
                }
            })
    }
}
pub struct LegacyCommandReceiver<C> {
    // A legacy driver may park one command behind a presentation barrier. Keep its
    // admission charged until the driver begins receiving its next command.
    current: std::cell::RefCell<Option<BudgetLease>>,
    current_input: std::cell::RefCell<Option<LegacyInputReservation>>,
    sequence: std::cell::Cell<u64>,
    input: LegacyInputBudget,
    receiver: mpsc::Receiver<CommandEnvelope<C>>,
    wakeups: Arc<Mutex<std::collections::HashSet<u8>>>,
}
impl<C> LegacyCommandReceiver<C> {
    fn unwrap(&self, envelope: CommandEnvelope<C>) -> C {
        if let Some(key) = envelope.key {
            self.wakeups
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
        self.sequence.set(envelope.sequence);
        self.current.replace(Some(envelope._lease));
        self.current_input.replace(envelope.input);
        envelope.command
    }
    pub fn received_sequence(&self) -> u64 {
        self.sequence.get()
    }
    pub fn input_budget(&self) -> LegacyInputBudget {
        self.input.clone()
    }
    pub fn take_input_reservation(&self) -> Option<LegacyInputReservation> {
        self.current_input.borrow_mut().take()
    }
    pub fn try_iter(&self) -> impl Iterator<Item = C> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
    pub fn try_recv(&self) -> Result<C, mpsc::TryRecvError> {
        self.current.replace(None);
        self.current_input.replace(None);
        self.receiver.try_recv().map(|e| self.unwrap(e))
    }
    pub fn recv(&self) -> Result<C, mpsc::RecvError> {
        self.current.replace(None);
        self.current_input.replace(None);
        self.receiver.recv().map(|e| self.unwrap(e))
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<C, mpsc::RecvTimeoutError> {
        self.current.replace(None);
        self.current_input.replace(None);
        self.receiver.recv_timeout(timeout).map(|e| self.unwrap(e))
    }
}
impl<C> Drop for LegacyCommandReceiver<C> {
    fn drop(&mut self) {
        self.wakeups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
impl LegacyRuntimeFactory {
    pub fn command_channel<C: LegacyCommand>(
        &self,
    ) -> (LegacyCommandSender<C>, LegacyCommandReceiver<C>) {
        let (sender, receiver) = mpsc::sync_channel(self.0.limits.commands);
        let wakeups = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let input = LegacyInputBudget {
            local: Budget::new(0, self.0.limits.pending_input_bytes),
            aggregate: Arc::clone(&self.0.input),
            client: Budget::new(0, self.0.limits.pending_input_bytes - REPLY_INPUT_RESERVE),
            client_aggregate: Arc::clone(&self.0.client_input),
        };
        (
            LegacyCommandSender {
                sender,
                local: Budget::new(self.0.limits.commands, self.0.limits.command_bytes),
                ordinary: Budget::new(self.0.limits.commands.saturating_sub(16).max(1), 0),
                aggregate: Arc::clone(&self.0.ingress),
                input: input.clone(),
                sequence: Arc::new(AtomicU64::new(0)),
                wakeups: Arc::clone(&wakeups),
            },
            LegacyCommandReceiver {
                receiver,
                wakeups,
                current: std::cell::RefCell::new(None),
                current_input: std::cell::RefCell::new(None),
                sequence: std::cell::Cell::new(0),
                input,
            },
        )
    }
}
/// One command's input claim, retained by all chunks until its last byte is released.
pub struct LegacyInputReservation {
    lease: Arc<Mutex<BudgetLease>>,
    capacity: usize,
    used: usize,
}
impl LegacyInputReservation {
    fn new(lease: BudgetLease) -> Self {
        Self {
            capacity: lease.bytes(),
            lease: Arc::new(Mutex::new(lease)),
            used: 0,
        }
    }
    pub fn retain(&mut self, bytes: usize) -> Result<Arc<Mutex<BudgetLease>>, RuntimeFailure> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|used| *used <= self.capacity)
            .ok_or(RuntimeFailure::internal(RuntimeStage::Input))?;
        Ok(Arc::clone(&self.lease))
    }
}
impl Drop for LegacyInputReservation {
    fn drop(&mut self) {
        self.lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shrink(self.used);
    }
}
#[derive(Clone)]
pub struct LegacyInputBudget {
    local: Arc<Budget>,
    aggregate: Arc<Budget>,
    client: Arc<Budget>,
    client_aggregate: Arc<Budget>,
}
impl LegacyInputBudget {
    pub fn reserve(&self, bytes: usize) -> Result<BudgetLease, RuntimeFailure> {
        Ok(self
            .reserve_reply(bytes)?
            .combine(self.client.reserve(0, bytes, RuntimeStage::Input)?)
            .combine(
                self.client_aggregate
                    .reserve(0, bytes, RuntimeStage::Input)?,
            ))
    }
    /// Only terminal-generated replies may use the capacity excluded from client input.
    pub fn reserve_reply(&self, bytes: usize) -> Result<BudgetLease, RuntimeFailure> {
        Ok(self
            .local
            .reserve(0, bytes, RuntimeStage::Input)?
            .combine(self.aggregate.reserve(0, bytes, RuntimeStage::Input)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn size() -> PtySize {
        PtySize {
            rows: 24,
            columns: 80,
            pixel_width: 800,
            pixel_height: 480,
        }
    }

    #[test]
    fn dropping_legacy_handles_does_not_retire_the_owner() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        let (entered, entry) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let owner = factory
            .start(size(), move |context| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                assert!(!context.retirement_requested());
                Ok(())
            })
            .unwrap();
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(owner.handle());
        release.send(()).unwrap();
        let completion = owner.join().unwrap();
        completion.result.unwrap();
        assert!(matches!(completion.cleanup, CleanupResult::Confirmed(_)));
    }

    #[test]
    fn legacy_close_is_nonblocking_and_retains_capacity_until_worker_cleanup() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            owners: 1,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let (entered, entry) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let owner = factory
            .start(size(), move |context| {
                entered.send(context.handle()).unwrap();
                released.recv().unwrap();
                assert!(context.retirement_requested());
                Ok(())
            })
            .unwrap();
        let handle = entry.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(owner);
        assert!(handle.retirement_requested());
        assert!(factory.start(size(), |_| Ok(())).is_err());
        release.send(()).unwrap();
        handle.join().unwrap().result.unwrap();
        factory
            .start(size(), |_| Ok(()))
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
    }

    #[test]
    fn invalid_legacy_geometry_never_invokes_the_worker() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        let invalid = PtySize { rows: 0, ..size() };
        assert!(
            factory
                .start(invalid, |_| panic!("invalid launch ran"))
                .is_err()
        );
    }
    struct TestCommand {
        bytes: usize,
        internal: bool,
    }
    impl LegacyCommand for TestCommand {
        fn payload_bytes(&self) -> usize {
            self.bytes
        }
        fn internal(&self) -> bool {
            self.internal
        }
    }
    #[test]
    fn legacy_ingress_bounds_pending_payload_and_preserves_native_capacity() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        let (sender, receiver) = factory.command_channel();
        for _ in 0..240 {
            assert!(
                sender
                    .send(TestCommand {
                        bytes: 0,
                        internal: false
                    })
                    .is_ok()
            );
        }
        assert!(
            sender
                .send(TestCommand {
                    bytes: 0,
                    internal: false
                })
                .is_err()
        );
        for _ in 0..16 {
            assert!(
                sender
                    .send(TestCommand {
                        bytes: 0,
                        internal: true
                    })
                    .is_ok()
            );
        }
        assert!(
            sender
                .send(TestCommand {
                    bytes: 0,
                    internal: true
                })
                .is_err()
        );
        assert_eq!(receiver.try_iter().count(), 256);
        assert!(
            sender
                .send(TestCommand {
                    bytes: 4 * 1024 * 1024,
                    internal: false
                })
                .is_ok()
        );
        assert!(
            sender
                .send(TestCommand {
                    bytes: 1,
                    internal: false
                })
                .is_err()
        );
        receiver.recv().unwrap();
        assert!(
            sender
                .send(TestCommand {
                    bytes: 1,
                    internal: false
                })
                .is_err()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(
            sender
                .send(TestCommand {
                    bytes: 1,
                    internal: false
                })
                .is_ok()
        );
    }
    struct TestReader {
        steps: mpsc::Receiver<Vec<u8>>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl std::io::Read for TestReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let next = self.steps.recv().unwrap_or_default();
            assert!(next.len() <= bytes.len());
            bytes[..next.len()].copy_from_slice(&next);
            Ok(next.len())
        }
    }
    impl Drop for TestReader {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct TestProcess {
        reader: Option<Box<dyn std::io::Read + Send>>,
        reader_failure: bool,
        exited: Arc<AtomicBool>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
        resizes: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Write for TestProcess {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl PtyProcess for TestProcess {
        fn take_reader(&mut self) -> Result<Box<dyn std::io::Read + Send>, RuntimeFailure> {
            if self.reader_failure {
                return Err(RuntimeFailure::new(
                    FailureClass::RuntimeFault,
                    RuntimeStage::Reader,
                ));
            }
            self.reader
                .take()
                .ok_or(RuntimeFailure::internal(RuntimeStage::Reader))
        }
        fn hidden_input(&self) -> Result<bool, RuntimeFailure> {
            Ok(false)
        }
        fn resize(&self, _: PtySize) -> Result<(), RuntimeFailure> {
            self.resizes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        fn wait_for_exit(&mut self, _: Duration) -> Result<PtyExit, RuntimeFailure> {
            Ok(if self.exited.load(Ordering::Acquire) {
                PtyExit::Exited { status: Some(0) }
            } else {
                PtyExit::StillRunning
            })
        }
    }
    impl Drop for TestProcess {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct TestTermination {
        steps: mpsc::Sender<Vec<u8>>,
        exited: Arc<AtomicBool>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        failure: bool,
    }
    impl PtyTermination for TestTermination {
        fn request_termination(&self) -> Result<(), RuntimeFailure> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.exited.store(true, Ordering::Release);
            let _ = self.steps.send(Vec::new());
            if self.failure {
                Err(RuntimeFailure::new(
                    FailureClass::RuntimeFault,
                    RuntimeStage::Cleanup,
                ))
            } else {
                Ok(())
            }
        }
    }
    #[derive(Default, Clone)]
    struct Records {
        reader_drops: Arc<std::sync::atomic::AtomicUsize>,
        process_drops: Arc<std::sync::atomic::AtomicUsize>,
        terminations: Arc<std::sync::atomic::AtomicUsize>,
        resizes: Arc<std::sync::atomic::AtomicUsize>,
    }
    fn parts(records: &Records, reader_failure: bool, cleanup_failure: bool) -> PtyParts {
        let (steps, reader) = mpsc::channel();
        let exited = Arc::new(AtomicBool::new(false));
        PtyParts {
            process: Box::new(TestProcess {
                reader: Some(Box::new(TestReader {
                    steps: reader,
                    drops: Arc::clone(&records.reader_drops),
                })),
                reader_failure,
                exited: Arc::clone(&exited),
                drops: Arc::clone(&records.process_drops),
                resizes: Arc::clone(&records.resizes),
            }),
            termination: Arc::new(TestTermination {
                steps,
                exited,
                calls: Arc::clone(&records.terminations),
                failure: cleanup_failure,
            }),
        }
    }
    #[test]
    fn legacy_owner_retirement_joins_process_and_blocked_reader_once() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        let records = Records::default();
        let process = parts(&records, false, false);
        let (entered, entry) = mpsc::sync_channel(1);
        let owner = factory
            .start(size(), move |context| {
                let _pty = context.launch(|_| Ok(process), || Ok(()), || {})?;
                entered.send(()).unwrap();
                while !context.retirement_requested() {
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })
            .unwrap();
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.retire();
        owner.retire();
        let completion = owner.join().unwrap();
        completion.result.unwrap();
        assert_eq!(
            completion.cleanup,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased)
        );
        assert_eq!(records.terminations.load(Ordering::Acquire), 1);
        assert_eq!(records.reader_drops.load(Ordering::Acquire), 1);
        assert_eq!(records.process_drops.load(Ordering::Acquire), 1);
    }
    #[test]
    fn legacy_reader_start_failure_cleans_installed_process_before_completion() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        let records = Records::default();
        let process = parts(&records, true, false);
        let completion = factory
            .start(size(), move |context| {
                context
                    .launch(|_| Ok(process), || Ok(()), || {})
                    .map(|_| ())
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(completion.result.unwrap_err().stage, RuntimeStage::Reader);
        assert!(matches!(completion.cleanup, CleanupResult::Confirmed(_)));
        assert_eq!(records.terminations.load(Ordering::Acquire), 1);
        assert_eq!(records.process_drops.load(Ordering::Acquire), 1);
    }
    #[test]
    fn unconfirmed_legacy_cleanup_does_not_release_launch_capacity() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            owners: 1,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let process = parts(&Records::default(), false, true);
        let completion = factory
            .start(size(), move |context| {
                let _pty = context.launch(|_| Ok(process), || Ok(()), || {})?;
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap();
        completion.result.unwrap();
        assert!(matches!(completion.cleanup, CleanupResult::Failed(_)));
        assert!(factory.start(size(), |_| Ok(())).is_err());
    }
    #[test]
    fn legacy_resize_reserves_grid_before_native_effect_and_allows_pixel_only_changes() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            active_grid_slots: 9000,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let records = Records::default();
        let process = parts(&records, false, false);
        factory
            .start(size(), move |context| {
                let mut pty = context.launch(|_| Ok(process), || Ok(()), || {})?;
                assert_eq!(
                    pty.resize(PtySize { rows: 25, ..size() })
                        .unwrap_err()
                        .class,
                    FailureClass::ResourceLimit
                );
                pty.resize(PtySize {
                    pixel_width: 960,
                    ..size()
                })?;
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(records.resizes.load(Ordering::Acquire), 1);
    }
    #[test]
    fn legacy_same_area_resize_still_reserves_reflow_capacity() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            active_grid_slots: 9000,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let records = Records::default();
        let process = parts(&records, false, false);
        factory
            .start(size(), move |context| {
                let mut pty = context.launch(|_| Ok(process), || Ok(()), || {})?;
                assert_eq!(
                    pty.resize_with(
                        PtySize {
                            rows: 48,
                            columns: 40,
                            ..size()
                        },
                        || panic!("rejected resize reached the engine"),
                    )
                    .unwrap_err()
                    .class,
                    FailureClass::ResourceLimit
                );
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(records.resizes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn legacy_resize_keeps_old_grid_and_scratch_charged_until_engine_resize_finishes() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            owners: 2,
            active_grid_slots: 21_000,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let other = factory.clone();
        let records = Records::default();
        let process = parts(&records, false, false);
        factory
            .start(size(), move |context| {
                let mut pty = context.launch(|_| Ok(process), || Ok(()), || {})?;
                let mut engine_resized = false;
                pty.resize_with(PtySize { rows: 25, ..size() }, || {
                    assert!(other.start(size(), |_| Ok(())).is_err());
                    engine_resized = true;
                    Ok(())
                })?;
                assert!(engine_resized);
                other.start(size(), |_| Ok(()))?.join()?.result?;
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(records.resizes.load(Ordering::Acquire), 1);
    }

    #[test]
    fn pending_legacy_input_budgets_are_shared_and_cannot_be_reset_by_cloning_context() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            owners: 2,
            pending_input_bytes: 65_537,
            aggregate_pending_input_bytes: 131_073,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let (sender, receiver) = mpsc::sync_channel(2);
        let other = sender.clone();
        let first = factory
            .start(size(), move |context| {
                let budget = context.input_budget();
                let lease = budget.reserve_reply(65_537)?;
                assert!(context.input_budget().reserve(1).is_err());
                assert!(sender.send((budget, lease)).is_ok());
                Ok(())
            })
            .unwrap();
        first.join().unwrap().result.unwrap();
        factory
            .start(size(), move |context| {
                assert!(
                    other
                        .send((context.input_budget(), BudgetLease::default()))
                        .is_ok()
                );
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
        let (_first, lease) = receiver.recv().unwrap();
        let (second, _empty) = receiver.recv().unwrap();
        assert!(second.reserve_reply(65_537).is_err());
        drop(lease);
        assert!(second.reserve_reply(65_537).is_ok());
    }
    #[test]
    fn legacy_client_input_cannot_consume_terminal_reply_capacity() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits::default()).unwrap();
        factory
            .start(size(), |context| {
                let budget = context.input_budget();
                let _client = budget.reserve(4 * 1024 * 1024 - 64 * 1024)?;
                assert!(budget.reserve(1).is_err());
                let _reply = budget.reserve_reply(64 * 1024)?;
                assert!(budget.reserve_reply(1).is_err());
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap()
            .result
            .unwrap();
    }
    #[derive(Debug)]
    struct InputCommand(usize);
    impl LegacyCommand for InputCommand {
        fn payload_bytes(&self) -> usize {
            0
        }
        fn input_bytes(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn legacy_input_admission_transfers_capacity_and_releases_unused_encoding() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            pending_input_bytes: REPLY_INPUT_RESERVE + 128,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let (sender, receiver) = factory.command_channel();
        sender.send(InputCommand(100)).unwrap();
        assert!(matches!(
            sender.try_send(InputCommand(100)),
            Err((
                _,
                RuntimeFailure {
                    class: FailureClass::ResourceLimit,
                    ..
                }
            ))
        ));
        receiver.recv().unwrap();
        let mut reservation = receiver.take_input_reservation().unwrap();
        let chunk = reservation.retain(8).unwrap();
        drop(reservation);
        assert_eq!(chunk.lock().unwrap().bytes(), 8);
        assert!(sender.send(InputCommand(121)).is_err());
        sender.send(InputCommand(120)).unwrap();
        let reply = receiver
            .input_budget()
            .reserve_reply(REPLY_INPUT_RESERVE)
            .unwrap();
        assert!(receiver.input_budget().reserve_reply(1).is_err());
        drop(reply);
        drop(chunk);
        sender.send(InputCommand(8)).unwrap();
        assert!(sender.send(InputCommand(1)).is_err());
    }

    #[test]
    fn queued_legacy_input_is_aggregate_bounded_and_released_on_cancellation() {
        let factory = LegacyRuntimeFactory::new(RuntimeLimits {
            owners: 2,
            pending_input_bytes: REPLY_INPUT_RESERVE + 128,
            aggregate_pending_input_bytes: 2 * REPLY_INPUT_RESERVE + 128,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let (first, first_receiver) = factory.command_channel();
        let (second, second_receiver) = factory.command_channel();
        first.send(InputCommand(100)).unwrap();
        assert!(second.send(InputCommand(29)).is_err());
        second.send(InputCommand(28)).unwrap();
        let _first_reply = first_receiver
            .input_budget()
            .reserve_reply(REPLY_INPUT_RESERVE)
            .unwrap();
        let _second_reply = second_receiver
            .input_budget()
            .reserve_reply(REPLY_INPUT_RESERVE)
            .unwrap();
        assert!(first_receiver.input_budget().reserve_reply(1).is_err());
        drop(first_receiver);
        second.send(InputCommand(100)).unwrap();
        assert!(second.send(InputCommand(1)).is_err());
    }
}
