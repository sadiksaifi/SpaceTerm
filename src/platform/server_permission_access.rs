//! The desktop's view of this machine's System Permissions, which belong to its SpaceTerm Server.
//!
//! Programs in a Terminal Session inherit their server's grants, so the desktop reads and sets
//! them up by asking the server on this machine over its account-local service lane. It never asks
//! a paired server: those grants are set up only on their own machines. Reads return the latest
//! report at once and refresh it in the background, and a refreshed report that differs signals
//! every observer, so a Settings row converges on what the server reports.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use spaceterm_server::ServerError;
use spaceterm_server::permissions::{
    self as server, PermissionHolder, PermissionState, PermissionsReport, RequestOutcome,
};
use spaceterm_server::service::lane::{LaneReply, LaneRequest};

use super::microphone_access::{
    MicrophoneAccess, MicrophoneAccessError, MicrophoneAuthorization,
    MicrophoneAuthorizationCompletion,
};
use super::permission_access::{
    AccessibilityNaming, PermissionAccess, PermissionAccessError, PermissionAccessObservation,
    PermissionAccessSubscription, PermissionAuthorization, PermissionResetCompletion,
    PermissionSetupCompletion, PermissionSetupPreparation, PermissionSetupReadiness,
    SystemPermission,
};

/// A probe answers within three seconds, and the server's own tools within ten each.
const READ_LIMIT: Duration = Duration::from_secs(10);
const CHANGE_LIMIT: Duration = Duration::from_secs(30);
/// How long a microphone prompt may stay unanswered before the request reports the state then.
const PROMPT_LIMIT: Duration = Duration::from_secs(600);

/// One request to the server on this machine.
pub(crate) trait PermissionLane: Send + Sync {
    /// Sends `request` over a fresh lane connection and returns the server's reply, which may be
    /// a lane error.
    fn call(&self, request: &LaneRequest, limit: Duration) -> Result<LaneReply, ServerError>;
}

/// The lane of this identity's account-local server. Opening it never starts a server.
pub(crate) struct LocalPermissionLane {
    pub(crate) identity: &'static str,
}

impl PermissionLane for LocalPermissionLane {
    fn call(&self, request: &LaneRequest, limit: Duration) -> Result<LaneReply, ServerError> {
        spaceterm_server::ServerConnection::for_identity(self.identity)?
            .service_lane()?
            .session()?
            .request(request, Instant::now() + limit)
    }
}

/// Why the server did not answer with what was asked. Each names a classification only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    /// The server is unreachable, refused the caller or does not support the request.
    Unavailable,
    /// The system did not complete the operation.
    Rejected,
}

impl From<Failure> for PermissionAccessError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unavailable => Self::PlatformUnavailable,
            Failure::Rejected => Self::PlatformRejected,
        }
    }
}

impl From<Failure> for MicrophoneAccessError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unavailable => Self::PlatformUnavailable,
            Failure::Rejected => Self::PlatformRejected,
        }
    }
}

fn failure(reply: Result<LaneReply, ServerError>) -> Result<LaneReply, Failure> {
    match reply {
        Ok(LaneReply::Error { code }) if code == server::PERMISSION_FAILED => {
            Err(Failure::Rejected)
        }
        Ok(LaneReply::Error { .. }) | Err(_) => Err(Failure::Unavailable),
        Ok(reply) => Ok(reply),
    }
}

/// The server's System Permissions as the desktop presents them.
pub(crate) struct ServerPermissionAccess {
    shared: Arc<Shared>,
    /// The holder System Settings lists before the first report arrives.
    expected_holder: PermissionHolder,
    /// How often an unanswered microphone request reads again.
    poll: Duration,
}

struct Shared {
    lane: Box<dyn PermissionLane>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The latest read, or `None` before the first one finished.
    latest: Option<Result<PermissionsReport, Failure>>,
    reading: bool,
    read_again: bool,
    subscribers: Vec<async_channel::Sender<()>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn call(&self, request: &LaneRequest, limit: Duration) -> Result<LaneReply, Failure> {
        failure(self.lane.call(request, limit))
    }

    fn read(&self) -> Result<PermissionsReport, Failure> {
        match self.call(&LaneRequest::Permissions, READ_LIMIT)? {
            LaneReply::Permissions(report) => Ok(report),
            _ => Err(Failure::Unavailable),
        }
    }

    /// Keeps a read and signals every subscriber when it differs from the last one.
    fn record(&self, read: Result<PermissionsReport, Failure>) {
        let mut state = self.lock();
        if state.latest.as_ref() != Some(&read) {
            state.latest = Some(read);
            state
                .subscribers
                .retain(|subscriber| !subscriber.is_closed());
            for subscriber in &state.subscribers {
                let _ = subscriber.try_send(());
            }
        }
    }

    /// Reads again in the background. A request made while one runs starts one more
    /// afterward, so the last report always follows the last request.
    fn refresh(this: &Arc<Self>) {
        {
            let mut state = this.lock();
            if state.reading {
                state.read_again = true;
                return;
            }
            state.reading = true;
        }
        let shared = Arc::clone(this);
        let spawned = std::thread::Builder::new()
            .name("spaceterm-permission-read".to_owned())
            .spawn(move || {
                loop {
                    let read = shared.read();
                    shared.record(read);
                    let mut state = shared.lock();
                    if !std::mem::take(&mut state.read_again) {
                        state.reading = false;
                        break;
                    }
                }
            });
        if spawned.is_err() {
            this.lock().reading = false;
        }
    }

    fn latest(&self) -> Option<Result<PermissionsReport, Failure>> {
        self.lock().latest.clone()
    }
}

impl ServerPermissionAccess {
    /// The System Permissions of `identity`'s server, read once now so Settings opens on a
    /// report.
    pub(crate) fn new(identity: &'static str) -> Self {
        Self::with(
            Box::new(LocalPermissionLane { identity }),
            expected_holder(identity),
            Duration::from_millis(500),
        )
    }

    fn with(
        lane: Box<dyn PermissionLane>,
        expected_holder: PermissionHolder,
        poll: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            lane,
            state: Mutex::default(),
        });
        Shared::refresh(&shared);
        Self {
            shared,
            expected_holder,
            poll,
        }
    }

    /// The latest state of `permission`, starting a background read.
    fn state(
        &self,
        permission: server::SystemPermission,
    ) -> Option<Result<PermissionState, Failure>> {
        let latest = self.shared.latest();
        Shared::refresh(&self.shared);
        latest.map(|read| {
            read.map(|report| {
                report
                    .state(permission)
                    .unwrap_or(PermissionState::NotGranted)
            })
        })
    }

    /// The holder of the latest report, or the one System Settings is expected to list.
    fn current_holder(&self) -> PermissionHolder {
        match self.shared.latest() {
            Some(Ok(report)) => report.holder,
            _ => self.expected_holder.clone(),
        }
    }

    fn spawn(&self, name: &str, work: impl FnOnce(&Arc<Shared>) + Send + 'static) -> bool {
        let shared = Arc::clone(&self.shared);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || work(&shared))
            .is_ok()
    }
}

/// The name System Settings lists `identity`'s server under, when launchd runs it from its
/// bundle.
fn expected_holder(identity: &str) -> PermissionHolder {
    spaceterm_server::service::bundle_name(identity)
        .and_then(|name| name.strip_suffix(".app"))
        .map_or(PermissionHolder::Launcher, |name| {
            PermissionHolder::Server {
                name: name.to_owned(),
            }
        })
}

const fn server_permission(permission: SystemPermission) -> server::SystemPermission {
    match permission {
        SystemPermission::ScreenRecording => server::SystemPermission::ScreenRecording,
        SystemPermission::Accessibility => server::SystemPermission::Accessibility,
    }
}

const fn microphone_authorization(state: PermissionState) -> MicrophoneAuthorization {
    match state {
        PermissionState::NotRequested => MicrophoneAuthorization::NotDetermined,
        PermissionState::Granted => MicrophoneAuthorization::Authorized,
        PermissionState::NotGranted => MicrophoneAuthorization::Denied,
        PermissionState::Restricted => MicrophoneAuthorization::Restricted,
    }
}

/// Keeps nothing alive: a closed receiver is dropped from the subscribers on the next change.
struct ServerSubscription;

impl PermissionAccessSubscription for ServerSubscription {}

impl PermissionAccess for ServerPermissionAccess {
    fn authorization(
        &self,
        permission: SystemPermission,
    ) -> Result<PermissionAuthorization, PermissionAccessError> {
        match self.state(server_permission(permission)) {
            Some(Ok(PermissionState::Granted)) => Ok(PermissionAuthorization::Granted),
            Some(Ok(_)) | None => Ok(PermissionAuthorization::NotGranted),
            Some(Err(failure)) => Err(failure.into()),
        }
    }

    fn observe(&self) -> Option<PermissionAccessObservation> {
        let (sender, changed) = async_channel::bounded(1);
        let mut state = self.shared.lock();
        state
            .subscribers
            .retain(|subscriber| !subscriber.is_closed());
        state.subscribers.push(sender);
        Some(PermissionAccessObservation {
            changed,
            subscription: Box::new(ServerSubscription),
        })
    }

    fn prepare_setup(
        &self,
        permission: SystemPermission,
        completion: PermissionSetupCompletion,
    ) -> Result<PermissionSetupPreparation, PermissionAccessError> {
        let (preparation, cancellation) = PermissionSetupPreparation::new();
        let request = LaneRequest::PreparePermissionSetup {
            permission: server_permission(permission),
        };
        let spawned = self.spawn("spaceterm-permission-setup", move |shared| {
            if cancellation.is_cancelled() {
                return;
            }
            let readiness = shared
                .call(&request, CHANGE_LIMIT)
                .and_then(|reply| match reply {
                    LaneReply::PermissionSetup(server::SetupReadiness::AlreadyGranted) => {
                        Ok(PermissionSetupReadiness::AlreadyGranted)
                    }
                    LaneReply::PermissionSetup(server::SetupReadiness::Ready { cleared }) => {
                        Ok(PermissionSetupReadiness::Ready { cleared })
                    }
                    _ => Err(Failure::Unavailable),
                });
            shared.record(shared.read());
            completion(readiness.map_err(Into::into));
        });
        spawned
            .then_some(preparation)
            .ok_or(PermissionAccessError::PlatformUnavailable)
    }

    fn open_settings(&self, permission: SystemPermission) -> Result<(), PermissionAccessError> {
        let request = LaneRequest::OpenPermissionSettings {
            permission: server_permission(permission),
        };
        match self.shared.call(&request, CHANGE_LIMIT)? {
            LaneReply::PermissionSettingsOpened => Ok(()),
            _ => Err(PermissionAccessError::PlatformUnavailable),
        }
    }

    fn accessibility_naming(&self) -> AccessibilityNaming {
        match self.shared.latest() {
            Some(Ok(report)) => match report.accessibility_naming {
                server::AccessibilityNaming::Accessibility => AccessibilityNaming::Accessibility,
                server::AccessibilityNaming::DeviceControl => AccessibilityNaming::DeviceControl,
            },
            _ => AccessibilityNaming::default(),
        }
    }

    fn holder(&self) -> PermissionHolder {
        self.current_holder()
    }

    fn can_reset(&self) -> bool {
        matches!(self.shared.latest(), Some(Ok(report)) if report.resettable)
    }

    fn reset(
        &self,
        permission: SystemPermission,
        completion: PermissionResetCompletion,
    ) -> Result<(), PermissionAccessError> {
        let request = LaneRequest::ResetPermission {
            permission: server_permission(permission),
        };
        let spawned = self.spawn("spaceterm-permission-reset", move |shared| {
            let result = shared
                .call(&request, CHANGE_LIMIT)
                .and_then(|reply| match reply {
                    LaneReply::PermissionReset => Ok(()),
                    _ => Err(Failure::Unavailable),
                });
            // The completion follows a fresh read, so a read made on completion reflects the reset.
            shared.record(shared.read());
            completion(result.map_err(Into::into));
        });
        spawned
            .then_some(())
            .ok_or(PermissionAccessError::PlatformUnavailable)
    }
}

impl MicrophoneAccess for ServerPermissionAccess {
    fn authorization(&self) -> Result<MicrophoneAuthorization, MicrophoneAccessError> {
        match self.state(server::SystemPermission::Microphone) {
            Some(Ok(state)) => Ok(microphone_authorization(state)),
            None => Ok(MicrophoneAuthorization::NotDetermined),
            Some(Err(failure)) => Err(failure.into()),
        }
    }

    /// The server shows the prompt. Its answer reaches the server's next read, so the request
    /// reads again until the microphone is no longer undecided.
    fn request_authorization(
        &self,
        completion: MicrophoneAuthorizationCompletion,
    ) -> Result<(), MicrophoneAccessError> {
        let poll = self.poll;
        let spawned = self.spawn("spaceterm-microphone-request", move |shared| {
            let request = LaneRequest::RequestPermission {
                permission: server::SystemPermission::Microphone,
            };
            let state = match shared.call(&request, CHANGE_LIMIT) {
                Ok(LaneReply::PermissionRequested(RequestOutcome::Decided { state })) => state,
                Ok(LaneReply::PermissionRequested(RequestOutcome::Requested)) => {
                    let deadline = Instant::now() + PROMPT_LIMIT;
                    loop {
                        std::thread::sleep(poll);
                        let read = shared.read();
                        let state = read
                            .as_ref()
                            .ok()
                            .and_then(|report| report.state(server::SystemPermission::Microphone));
                        shared.record(read);
                        match state {
                            Some(PermissionState::NotRequested) | None
                                if Instant::now() < deadline => {}
                            Some(state) => break state,
                            None => break PermissionState::NotGranted,
                        }
                    }
                }
                _ => {
                    shared.record(shared.read());
                    PermissionState::NotGranted
                }
            };
            completion(microphone_authorization(state));
        });
        spawned
            .then_some(())
            .ok_or(MicrophoneAccessError::PlatformUnavailable)
    }

    fn open_settings(&self) -> Result<(), MicrophoneAccessError> {
        let request = LaneRequest::OpenPermissionSettings {
            permission: server::SystemPermission::Microphone,
        };
        match self.shared.call(&request, CHANGE_LIMIT)? {
            LaneReply::PermissionSettingsOpened => Ok(()),
            _ => Err(MicrophoneAccessError::PlatformUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::permissions::PermissionReading;

    /// A server whose report and replies the test scripts, recording every request.
    #[derive(Default)]
    struct ScriptedLane {
        report: Mutex<Option<PermissionsReport>>,
        replies: Mutex<Vec<LaneReply>>,
        requests: Mutex<Vec<LaneRequest>>,
    }

    impl PermissionLane for Arc<ScriptedLane> {
        fn call(&self, request: &LaneRequest, _: Duration) -> Result<LaneReply, ServerError> {
            self.requests.lock().unwrap().push(request.clone());
            if *request == LaneRequest::Permissions {
                return self
                    .report
                    .lock()
                    .unwrap()
                    .clone()
                    .map(LaneReply::Permissions)
                    .ok_or(ServerError::NotRunning);
            }
            let mut replies = self.replies.lock().unwrap();
            Ok(if replies.is_empty() {
                LaneReply::Error {
                    code: "unsupported".into(),
                }
            } else {
                replies.remove(0)
            })
        }
    }

    fn report(microphone: PermissionState, screen: PermissionState) -> PermissionsReport {
        PermissionsReport {
            permissions: vec![
                PermissionReading {
                    permission: server::SystemPermission::Microphone,
                    state: microphone,
                },
                PermissionReading {
                    permission: server::SystemPermission::ScreenRecording,
                    state: screen,
                },
                PermissionReading {
                    permission: server::SystemPermission::Accessibility,
                    state: PermissionState::NotGranted,
                },
            ],
            accessibility_naming: server::AccessibilityNaming::Accessibility,
            holder: PermissionHolder::Server {
                name: "SpaceTerm Server".into(),
            },
            resettable: true,
        }
    }

    fn access(lane: &Arc<ScriptedLane>) -> ServerPermissionAccess {
        ServerPermissionAccess::with(
            Box::new(Arc::clone(lane)),
            expected_holder("spaceterm"),
            Duration::from_millis(1),
        )
    }

    /// Waits for the background read the access started to land.
    fn settle(access: &ServerPermissionAccess) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while access.shared.lock().reading || access.shared.latest().is_none() {
            assert!(Instant::now() < deadline, "the read finished");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn reads_present_the_servers_report_and_its_holder() {
        let lane = Arc::new(ScriptedLane::default());
        *lane.report.lock().unwrap() = Some(report(
            PermissionState::Restricted,
            PermissionState::Granted,
        ));
        let access = access(&lane);
        settle(&access);

        assert_eq!(
            PermissionAccess::authorization(&access, SystemPermission::ScreenRecording),
            Ok(PermissionAuthorization::Granted)
        );
        assert_eq!(
            PermissionAccess::authorization(&access, SystemPermission::Accessibility),
            Ok(PermissionAuthorization::NotGranted)
        );
        assert_eq!(
            MicrophoneAccess::authorization(&access),
            Ok(MicrophoneAuthorization::Restricted)
        );
        assert_eq!(
            access.accessibility_naming(),
            AccessibilityNaming::Accessibility
        );
        assert!(access.can_reset());
        assert_eq!(
            PermissionAccess::holder(&access),
            PermissionHolder::Server {
                name: "SpaceTerm Server".into()
            }
        );
    }

    #[test]
    fn an_unreachable_server_reads_as_unavailable_and_resets_nothing() {
        let lane = Arc::new(ScriptedLane::default());
        let access = access(&lane);
        settle(&access);

        assert_eq!(
            PermissionAccess::authorization(&access, SystemPermission::ScreenRecording),
            Err(PermissionAccessError::PlatformUnavailable)
        );
        assert_eq!(
            MicrophoneAccess::authorization(&access),
            Err(MicrophoneAccessError::PlatformUnavailable)
        );
        assert!(!access.can_reset());
        assert_eq!(
            PermissionAccess::holder(&access),
            PermissionHolder::Server {
                name: "SpaceTerm Server".into()
            },
            "the holder System Settings is expected to list"
        );
    }

    #[test]
    fn a_changed_report_signals_every_observer() {
        let lane = Arc::new(ScriptedLane::default());
        *lane.report.lock().unwrap() = Some(report(
            PermissionState::NotRequested,
            PermissionState::NotGranted,
        ));
        let access = access(&lane);
        settle(&access);
        let observation = access.observe().unwrap();

        *lane.report.lock().unwrap() = Some(report(
            PermissionState::NotRequested,
            PermissionState::Granted,
        ));
        let _ = PermissionAccess::authorization(&access, SystemPermission::ScreenRecording);
        let deadline = Instant::now() + Duration::from_secs(5);
        while observation.changed.try_recv().is_err() {
            assert!(Instant::now() < deadline, "the change was signalled");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            PermissionAccess::authorization(&access, SystemPermission::ScreenRecording),
            Ok(PermissionAuthorization::Granted)
        );
    }

    #[test]
    fn a_setup_asks_the_server_and_reports_its_readiness() {
        let lane = Arc::new(ScriptedLane::default());
        *lane.report.lock().unwrap() = Some(report(
            PermissionState::NotRequested,
            PermissionState::NotGranted,
        ));
        lane.replies
            .lock()
            .unwrap()
            .push(LaneReply::PermissionSetup(server::SetupReadiness::Ready {
                cleared: true,
            }));
        let access = access(&lane);
        settle(&access);
        let (sender, ready) = std::sync::mpsc::channel();
        let preparation = access
            .prepare_setup(
                SystemPermission::Accessibility,
                Box::new(move |result| sender.send(result).unwrap()),
            )
            .unwrap();
        assert_eq!(
            ready.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok(PermissionSetupReadiness::Ready { cleared: true })
        );
        drop(preparation);
        assert!(
            lane.requests
                .lock()
                .unwrap()
                .contains(&LaneRequest::PreparePermissionSetup {
                    permission: server::SystemPermission::Accessibility
                })
        );
    }

    #[test]
    fn a_microphone_request_completes_once_the_prompt_is_answered() {
        let lane = Arc::new(ScriptedLane::default());
        *lane.report.lock().unwrap() = Some(report(
            PermissionState::NotRequested,
            PermissionState::NotGranted,
        ));
        lane.replies
            .lock()
            .unwrap()
            .push(LaneReply::PermissionRequested(RequestOutcome::Requested));
        let access = access(&lane);
        settle(&access);
        let (sender, decided) = std::sync::mpsc::channel();
        access
            .request_authorization(Box::new(move |decision| sender.send(decision).unwrap()))
            .unwrap();
        assert!(
            decided.recv_timeout(Duration::from_millis(50)).is_err(),
            "an unanswered prompt keeps the request open"
        );
        *lane.report.lock().unwrap() = Some(report(
            PermissionState::Granted,
            PermissionState::NotGranted,
        ));
        assert_eq!(
            decided.recv_timeout(Duration::from_secs(5)).unwrap(),
            MicrophoneAuthorization::Authorized
        );
    }

    #[test]
    fn a_failed_operation_is_reported_as_the_systems_refusal() {
        let lane = Arc::new(ScriptedLane::default());
        lane.replies.lock().unwrap().push(LaneReply::Error {
            code: server::PERMISSION_FAILED.into(),
        });
        let access = access(&lane);
        assert_eq!(
            PermissionAccess::open_settings(&access, SystemPermission::ScreenRecording),
            Err(PermissionAccessError::PlatformRejected)
        );
        assert_eq!(
            MicrophoneAccess::open_settings(&access),
            Err(MicrophoneAccessError::PlatformUnavailable),
            "an unsupported request is unavailable"
        );
    }
}
