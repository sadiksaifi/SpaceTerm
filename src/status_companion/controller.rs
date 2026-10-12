//! The companion's one loop: connect through the status-only facade, follow the single
//! replaceable status value, and handle the three menu commands. It has no path that
//! attaches to a Terminal Session, takes a control grant, starts a server or enrolls a device.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use spaceterm_client::management_cli::{
    ManagementFailure, ManagementFuture, ServerStatus, StatusManagement, StatusSubscription,
};
use spaceterm_client_native::cli::bindings::NativeObservation;
use tokio::sync::Notify;

use super::model::{CompanionModel, Notice};

/// Automatic reconnection waits. The last delay repeats.
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
];
/// A connected companion reads status on this interval to notice a lost connection. The
/// read proves reachability; it never ends a resync the subscription still requires.
const LIVENESS_INTERVAL: Duration = Duration::from_secs(30);
/// The local native observation is advisory and must not hold the status area.
const NATIVE_READ_DEADLINE: Duration = Duration::from_secs(5);

/// Everything the status area can ask for. None of them changes a server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompanionAction {
    OpenDesktop,
    RetryConnection,
    Quit,
}

/// Content-free reasons the companion cannot run or cannot complete a local action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CompanionFailure {
    #[error("the session has no status area")]
    StatusAreaUnavailable,
    #[error("the desktop application could not be opened")]
    DesktopUnavailable,
    #[error("another status companion is running in this session")]
    AlreadyRunning,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only AppKit requires the main thread")
    )]
    #[error("the status companion must start on the main thread")]
    MainThreadRequired,
    #[error("the status companion runtime is unavailable")]
    RuntimeUnavailable,
}

/// The platform status item and its menu.
pub(crate) trait StatusArea {
    /// Replaces what the status item shows. Hosts render only the newest model.
    fn present(&mut self, model: &CompanionModel) -> Result<(), CompanionFailure>;
    /// The oldest pending command, or a failure if the status area was lost.
    fn next_action(&mut self) -> Result<Option<CompanionAction>, CompanionFailure>;
}

/// Opens the desktop application from the local installation that contains this companion.
/// It takes no input, so no server value can choose what it opens.
pub(crate) trait DesktopActivation: Send + Sync {
    fn open(&self) -> Result<(), CompanionFailure>;
}

/// The local native observation, consulted only to explain an unreachable server.
pub(crate) trait NativeStatus: Send + Sync {
    fn inspect(&self) -> ManagementFuture<'_, NativeObservation>;
}

pub(crate) trait CompanionTimer: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Commands from the status area. Repeated commands coalesce, so the queue holds at most
/// one of each and never exceeds [`ActionIngress::CAPACITY`].
pub(crate) struct ActionIngress {
    queue: Mutex<VecDeque<CompanionAction>>,
    ready: Notify,
}

impl ActionIngress {
    pub(crate) const CAPACITY: usize = 16;

    pub(crate) fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(Self::CAPACITY)),
            ready: Notify::new(),
        }
    }

    /// Queues a command unless the same command is already pending. Returns false only
    /// when the bound would be exceeded.
    pub(crate) fn push(&self, action: CompanionAction) -> bool {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if !queue.contains(&action) {
            if queue.len() >= Self::CAPACITY {
                return false;
            }
            queue.push_back(action);
        }
        drop(queue);
        self.ready.notify_one();
        true
    }

    pub(crate) fn pop(&self) -> Option<CompanionAction> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "tests observe the bound"))]
    pub(crate) fn len(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn is_empty(&self) -> bool {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Resolves when a command is pending or the status area interrupted the wait.
    pub(crate) async fn wait(&self) {
        if !self.is_empty() {
            return;
        }
        self.ready.notified().await;
    }

    /// Wakes the controller without a command, so it asks the status area for its state.
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "the Linux area can be lost")
    )]
    pub(crate) fn interrupt(&self) {
        self.ready.notify_one();
    }
}

/// Where the companion reads status. Whether a Client binding exists is a fact of the
/// composition, not of any read, so an unbound companion never reads or polls.
pub(crate) enum StatusSource {
    /// The status-only facade over a backend bound to the companion's own Client.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "production has no companion Client binding until pairing supplies it"
        )
    )]
    Bound(Arc<dyn StatusManagement>),
    /// No companion Client binding is composed into this build. Pairing supplies it (C23).
    Unbound,
}

pub(crate) struct CompanionDependencies {
    pub(crate) status: StatusSource,
    pub(crate) native: Arc<dyn NativeStatus>,
    pub(crate) desktop: Arc<dyn DesktopActivation>,
    pub(crate) area: Box<dyn StatusArea + Send>,
    pub(crate) actions: Arc<ActionIngress>,
    pub(crate) timer: Arc<dyn CompanionTimer>,
}

enum Raced<T> {
    Done(T),
    Quit,
}

enum Followed {
    Lost(ManagementFailure),
    Quit,
}

enum Waited {
    Automatic,
    Manual,
    Quit,
}

pub(crate) struct CompanionController {
    source: StatusSource,
    native: Arc<dyn NativeStatus>,
    desktop: Arc<dyn DesktopActivation>,
    area: Box<dyn StatusArea + Send>,
    actions: Arc<ActionIngress>,
    timer: Arc<dyn CompanionTimer>,
    shown: Option<CompanionModel>,
    notice: Option<Notice>,
    /// The epoch and revision of the newest status shown on this connection.
    position: Option<(u64, u64)>,
}

impl CompanionController {
    pub(crate) fn new(dependencies: CompanionDependencies) -> Self {
        Self {
            source: dependencies.status,
            native: dependencies.native,
            desktop: dependencies.desktop,
            area: dependencies.area,
            actions: dependencies.actions,
            timer: dependencies.timer,
            shown: None,
            notice: None,
            position: None,
        }
    }

    /// Runs until the user quits or the status area is lost. Quitting drops this
    /// companion's own client handle and nothing else.
    pub(crate) async fn run(mut self) -> Result<(), CompanionFailure> {
        let status = match &self.source {
            StatusSource::Bound(status) => status.clone(),
            StatusSource::Unbound => {
                self.show(CompanionModel::unsupported())?;
                // Unsupported has no timer and offers no Retry, so only Quit ends the wait.
                loop {
                    if let Waited::Quit = self.wait(0).await? {
                        return Ok(());
                    }
                }
            }
        };
        self.show(CompanionModel::connecting())?;
        let mut attempt = 0;
        loop {
            let reader = status.clone();
            let connection = async move {
                let subscription = reader.subscribe_status().await?;
                let current = match subscription.latest()? {
                    Some(current) => current,
                    None => reader.status().await?,
                };
                Ok::<_, ManagementFailure>((subscription, current))
            };
            let failure = match self.race(connection).await? {
                Raced::Quit => return Ok(()),
                Raced::Done(Ok((subscription, current))) => {
                    attempt = 0;
                    match self.follow(&status, subscription, &current).await? {
                        Followed::Quit => return Ok(()),
                        Followed::Lost(failure) => failure,
                    }
                }
                Raced::Done(Err(failure)) => failure,
            };
            if self.disconnected(failure).await?.is_none() {
                return Ok(());
            }
            match self.wait(attempt).await? {
                Waited::Quit => return Ok(()),
                Waited::Automatic => attempt = (attempt + 1).min(RETRY_DELAYS.len() - 1),
                Waited::Manual => {
                    attempt = 0;
                    self.show(CompanionModel::connecting())?;
                }
            }
        }
    }

    async fn follow(
        &mut self,
        status: &Arc<dyn StatusManagement>,
        mut subscription: StatusSubscription,
        current: &ServerStatus,
    ) -> Result<Followed, CompanionFailure> {
        self.position = None;
        self.accept(current)?;
        let mut liveness = self.timer.sleep(LIVENESS_INTERVAL);
        loop {
            let actions = self.actions.clone();
            let event = tokio::select! {
                biased;
                () = actions.wait() => FollowEvent::Action,
                changed = subscription.changed() => FollowEvent::Changed(changed),
                () = &mut liveness => FollowEvent::Liveness,
            };
            match event {
                FollowEvent::Action => {
                    if self.handle_passive_actions()? {
                        return Ok(Followed::Quit);
                    }
                }
                FollowEvent::Changed(Err(failure)) => return Ok(Followed::Lost(failure)),
                FollowEvent::Changed(Ok(())) => {
                    if let Err(failure) = self.present_subscription(&subscription, None)? {
                        return Ok(Followed::Lost(failure));
                    }
                }
                FollowEvent::Liveness => {
                    let reader = status.clone();
                    let read = match self.race(async move { reader.status().await }).await? {
                        Raced::Quit => return Ok(Followed::Quit),
                        Raced::Done(Ok(read)) => read,
                        Raced::Done(Err(failure)) => return Ok(Followed::Lost(failure)),
                    };
                    if let Err(failure) = self.present_subscription(&subscription, Some(&read))? {
                        return Ok(Followed::Lost(failure));
                    }
                    liveness = self.timer.sleep(LIVENESS_INTERVAL);
                }
            }
        }
    }

    /// Shows a failure. Only an unreachable server consults the native observation, to
    /// tell a deliberately stopped server from a lost one. Returns `None` on Quit.
    async fn disconnected(
        &mut self,
        failure: ManagementFailure,
    ) -> Result<Option<()>, CompanionFailure> {
        self.position = None;
        let observation = if CompanionModel::consults_native(failure) {
            let native = self.native.clone();
            let inspection =
                async move { tokio::time::timeout(NATIVE_READ_DEADLINE, native.inspect()).await };
            match self.race(inspection).await? {
                Raced::Quit => return Ok(None),
                Raced::Done(Ok(Ok(observation))) => Some(observation),
                Raced::Done(_) => None,
            }
        } else {
            None
        };
        self.show(CompanionModel::disconnected(failure, observation.as_ref()))?;
        Ok(Some(()))
    }

    /// Waits for the next automatic attempt or an explicit Retry. A revoked binding and an
    /// unsupported installation have no automatic attempt.
    async fn wait(&mut self, attempt: usize) -> Result<Waited, CompanionFailure> {
        let automatic = self
            .shown
            .as_ref()
            .is_some_and(CompanionModel::retries_automatically);
        let mut timer = automatic.then(|| self.timer.sleep(RETRY_DELAYS[attempt]));
        loop {
            let actions = self.actions.clone();
            let elapsed = tokio::select! {
                biased;
                () = actions.wait() => false,
                () = async {
                    match timer.as_mut() {
                        Some(timer) => timer.await,
                        None => std::future::pending().await,
                    }
                } => true,
            };
            if elapsed {
                return Ok(Waited::Automatic);
            }
            while let Some(action) = self.area.next_action()? {
                match action {
                    CompanionAction::Quit => return Ok(Waited::Quit),
                    CompanionAction::OpenDesktop => self.open_desktop()?,
                    CompanionAction::RetryConnection
                        if self
                            .shown
                            .as_ref()
                            .is_some_and(CompanionModel::offers_retry) =>
                    {
                        return Ok(Waited::Manual);
                    }
                    CompanionAction::RetryConnection => {}
                }
            }
        }
    }

    /// Runs `future` while still answering the menu. A Retry during an attempt joins it.
    async fn race<T>(
        &mut self,
        future: impl Future<Output = T>,
    ) -> Result<Raced<T>, CompanionFailure> {
        tokio::pin!(future);
        loop {
            let actions = self.actions.clone();
            let output = tokio::select! {
                biased;
                () = actions.wait() => None,
                output = &mut future => Some(output),
            };
            match output {
                Some(output) => return Ok(Raced::Done(output)),
                None => {
                    if self.handle_passive_actions()? {
                        return Ok(Raced::Quit);
                    }
                }
            }
        }
    }

    /// Handles commands while no Retry applies. Returns true on Quit.
    fn handle_passive_actions(&mut self) -> Result<bool, CompanionFailure> {
        while let Some(action) = self.area.next_action()? {
            match action {
                CompanionAction::Quit => return Ok(true),
                CompanionAction::OpenDesktop => self.open_desktop()?,
                CompanionAction::RetryConnection => {}
            }
        }
        Ok(false)
    }

    /// Presents what the subscription holds. While it requires a resync, the shown status
    /// only gains the refreshing mark: a direct `read` proves the server answers but can be
    /// older than a publication the subscription already observed. Otherwise the newer of
    /// the subscription's value and `read` is shown. The inner error ends the connection.
    fn present_subscription(
        &mut self,
        subscription: &StatusSubscription,
        read: Option<&ServerStatus>,
    ) -> Result<Result<(), ManagementFailure>, CompanionFailure> {
        match subscription.needs_resync() {
            Err(failure) => return Ok(Err(failure)),
            Ok(true) => {
                if let Some(shown) = self.shown.clone() {
                    self.show(shown.with_refreshing(true))?;
                }
                return Ok(Ok(()));
            }
            Ok(false) => {}
        }
        let latest = match subscription.latest() {
            Err(failure) => return Ok(Err(failure)),
            Ok(latest) => latest,
        };
        for status in latest.iter().chain(read) {
            self.accept(status)?;
        }
        Ok(Ok(()))
    }

    /// Shows a status read unless a newer one is already shown on this connection.
    fn accept(&mut self, status: &ServerStatus) -> Result<(), CompanionFailure> {
        let parts = status.parts();
        let position = (parts.epoch.get(), parts.revision.get());
        if self.position.is_some_and(|shown| position < shown) {
            return Ok(());
        }
        self.position = Some(position);
        self.show(CompanionModel::from_status(status))
    }

    fn open_desktop(&mut self) -> Result<(), CompanionFailure> {
        self.notice = match self.desktop.open() {
            Ok(()) => None,
            Err(_) => Some(Notice::DesktopUnavailable),
        };
        match self.shown.clone() {
            Some(shown) => self.show(shown),
            None => Ok(()),
        }
    }

    /// Presents a model only when what it shows changed. A notice lasts until the state changes.
    fn show(&mut self, model: CompanionModel) -> Result<(), CompanionFailure> {
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.state() != model.state())
        {
            self.notice = None;
        }
        let model = model.with_notice(self.notice);
        if self.shown.as_ref() == Some(&model) {
            return Ok(());
        }
        self.area.present(&model)?;
        self.shown = Some(model);
        Ok(())
    }
}

enum FollowEvent {
    Action,
    Changed(Result<(), ManagementFailure>),
    Liveness,
}
