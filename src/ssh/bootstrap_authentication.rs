//! Bootstrap consumes the existing OpenSSH process owner and obscured Authentication Prompt.

#![expect(
    dead_code,
    reason = "Desktop C3 supplies foreground composition after native SSH authority is available."
)]

use super::{
    askpass::{AskPassPresenter, AskPassProtocolReply, prompt::AskPassRequest},
    cancellation::SshCancellationToken,
    command::{SshCommandContext, SshCommandSpec},
    live_connection::LiveConnectionCapability,
    process::{
        CancelOnDrop, CapturedProcessError, SshProcessAdapter, SshProcessEnvironment,
        run_captured_process,
    },
};
use spaceterm_client::pairing::{bootstrap::*, credentials::PairingFuture};
use spaceterm_core::pairing::PairingFailure;
use spaceterm_host::bootstrap::{
    process::{
        BootstrapProbeFailure, BootstrapProbeLease, BootstrapProbeTransport,
    },
};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use zeroize::Zeroizing;

struct OwnedControl {
    route: BootstrapRoute,
    live: LiveConnectionCapability,
    retired: AtomicBool,
    retire: Box<dyn Fn() + Send + Sync>,
}
impl OwnedControl {
    fn revalidate(&self, route: BootstrapRoute) -> Result<(), BootstrapFailure> {
        if route != self.route
            || self.retired.load(Ordering::Acquire)
            || self.live.authorize().is_err()
        {
            return Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::OpenSshAuthenticated,
            ));
        }
        Ok(())
    }
}
impl Drop for OwnedControl {
    fn drop(&mut self) {
        if !self.retired.swap(true, Ordering::AcqRel) {
            (self.retire)();
        }
    }
}
struct ControlLease(Arc<OwnedControl>);
impl BootstrapSshLease for ControlLease {
    fn revalidate(&self, route: BootstrapRoute) -> Result<(), BootstrapFailure> {
        self.0.revalidate(route)
    }
}
impl Drop for ControlLease {
    fn drop(&mut self) {
        if !self.0.retired.swap(true, Ordering::AcqRel) {
            (self.0.retire)();
        }
    }
}
/// `retire` enqueues the existing owner's exact process-group cleanup. It is not a process selector.
/// The foreground consumer provides a dedicated bootstrap control owner, not an ordinary reconnect.
pub(crate) struct AuthenticatedBootstrapSsh<A: SshProcessAdapter> {
    adapter: A,
    command: Arc<SshCommandSpec>,
    environment: SshProcessEnvironment,
    control: Arc<OwnedControl>,
}
impl<A: SshProcessAdapter> AuthenticatedBootstrapSsh<A> {
    pub(crate) fn new(
        adapter: A,
        context: &SshCommandContext,
        environment: SshProcessEnvironment,
        route: BootstrapRoute,
        live: LiveConnectionCapability,
        retire: Box<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            adapter,
            command: Arc::new(context.bootstrap_read_only_probe()),
            environment,
            control: Arc::new(OwnedControl {
                route,
                live,
                retired: AtomicBool::new(false),
                retire,
            }),
        }
    }
}
impl<A: SshProcessAdapter> BootstrapOpenSsh for AuthenticatedBootstrapSsh<A> {
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
        Box::pin(async move {
            let lease = ControlLease(self.control.clone());
            attempt.validate(BootstrapPhase::ExplicitIntent)?;
            self.control.revalidate(*route)?;
            OpenSshAuthenticated::retain(*route, Box::new(lease))
        })
    }
}
impl<A: SshProcessAdapter> BootstrapProbeTransport for AuthenticatedBootstrapSsh<A> {
    fn run_read_only<'a>(
        &'a self,
        ssh: &'a dyn BootstrapProbeLease,
        script: &'static [u8],
        maximum: usize,
    ) -> PairingFuture<'a, Result<Vec<u8>, BootstrapProbeFailure>> {
        Box::pin(async move {
            ssh.revalidate()?;
            if script != spaceterm_host::bootstrap::command::read_only_probe_script()
                || maximum != spaceterm_host::bootstrap::command::MAX_PROBE_BYTES
            {
                return Err(BootstrapProbeFailure::InvalidProbe);
            }
            let cancellation = SshCancellationToken::linked(
                &self.control.live.cancellation(),
                &SshCancellationToken::default(),
            );
            let _cancel_on_drop = CancelOnDrop::new(cancellation.clone());
            let adapter = self.adapter.clone();
            let command = self.command.clone();
            let environment = self.environment.clone();
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("spaceterm-bootstrap-probe".into())
                .spawn(move || {
                    let result = run_captured_process(
                        &adapter,
                        &command,
                        &environment,
                        script.to_vec(),
                        maximum,
                        &cancellation,
                        Instant::now() + spaceterm_host::bootstrap::command::PROBE_DEADLINE,
                    );
                    let result = match result {
                        Ok(output) if output.exit.is_success() => Ok(output.stdout),
                        Ok(_) => Err(BootstrapProbeFailure::Offline),
                        Err(error) => Err(match error {
                            CapturedProcessError::Cancelled => BootstrapProbeFailure::Cancelled,
                            CapturedProcessError::TimedOut => BootstrapProbeFailure::TimedOut,
                            CapturedProcessError::OutputTooLarge => {
                                BootstrapProbeFailure::LimitExceeded
                            }
                            CapturedProcessError::Operation(_) => BootstrapProbeFailure::Offline,
                        }),
                    };
                    let _ = send.send_blocking(result);
                })
                .map_err(|_| BootstrapProbeFailure::Offline)?;
            let result = receive
                .recv()
                .await
                .map_err(|_| BootstrapProbeFailure::Offline)?;
            ssh.revalidate()?;
            result
        })
    }
}

pub(crate) struct BootstrapAuthenticationPrompt {
    route: BootstrapRoute,
    attempt: BootstrapAttempt,
    live: LiveConnectionCapability,
    presenter: Arc<dyn AskPassPresenter>,
}
impl BootstrapAuthenticationPrompt {
    pub(crate) fn new(
        route: BootstrapRoute,
        attempt: BootstrapAttempt,
        live: LiveConnectionCapability,
        presenter: Arc<dyn AskPassPresenter>,
    ) -> Self {
        Self {
            route,
            attempt,
            live,
            presenter,
        }
    }
}
struct PromptWait(Arc<AtomicBool>);
impl Drop for PromptWait {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl ScopedAuthenticationPrompt for BootstrapAuthenticationPrompt {
    fn revalidate(&self, plan: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        self.attempt.validate(BootstrapPhase::Authorized)?;
        if plan.route() != self.route
            || !plan.belongs_to_attempt(&self.attempt)
            || self.live.authorize().is_err()
        {
            return Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::Authorized,
            ));
        }
        Ok(())
    }
    fn respond<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate(plan)?;
            if !plan.requires_linger_authorization() {
                return Err(BootstrapFailure::new(
                    PairingFailure::AuthorizationDenied,
                    BootstrapPhase::Authorized,
                ));
            }
            let request = AskPassRequest::scoped_sudo(plan.route());
            let stop = Arc::new(AtomicBool::new(false));
            let _wait = PromptWait(stop.clone());
            let presenter = self.presenter.clone();
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("spaceterm-bootstrap-authorization".into())
                .spawn(move || {
                    let result = match presenter.present(request, &stop) {
                        Ok(AskPassProtocolReply::Secret(secret)) => Ok(secret.into_zeroizing()),
                        Ok(AskPassProtocolReply::Cancelled) => Err(BootstrapFailure::new(
                            PairingFailure::Cancelled,
                            BootstrapPhase::Authorized,
                        )),
                        _ => Err(BootstrapFailure::new(
                            PairingFailure::AuthorizationUnavailable,
                            BootstrapPhase::Authorized,
                        )),
                    };
                    let _ = send.send_blocking(result);
                })
                .map_err(|_| {
                    BootstrapFailure::new(
                        PairingFailure::AuthorizationUnavailable,
                        BootstrapPhase::Authorized,
                    )
                })?;
            let response = receive.recv().await.map_err(|_| {
                BootstrapFailure::new(
                    PairingFailure::AuthorizationUnavailable,
                    BootstrapPhase::Authorized,
                )
            })??;
            self.revalidate(plan)?;
            if response.is_empty() {
                return Err(BootstrapFailure::new(
                    PairingFailure::AuthorizationRequired,
                    BootstrapPhase::Authorized,
                ));
            }
            Ok(response)
        })
    }
}

// The portable setup composition is shared with headless native clients.
use spaceterm_client_native::bootstrap::BootstrapOpenSsh;
#[cfg(test)]
use spaceterm_client_native::bootstrap::{BootstrapNativePreflight, SshNativeServerSetup};

#[cfg(test)]
mod tests {
    use spaceterm_host::bootstrap::probe::ReadOnlyHostProbe;
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const PROBE: &[u8] = b"STBP2\nuid=1000\nos=Other\narch=x86_64\nversion=22.04\nlibc=unknown\nkernel=5.15.0\nmanager=missing\nmanager_version=0\nmanager_reachable=no\nlinger=unavailable\nsudo=available\n";
    fn route() -> BootstrapRoute {
        BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 1000, BuildChannel::Release).unwrap()
    }
    struct Lease(Arc<AtomicBool>);
    impl BootstrapSshLease for Lease {
        fn revalidate(&self, _: BootstrapRoute) -> Result<(), BootstrapFailure> {
            if self.0.load(Ordering::Acquire) {
                Err(BootstrapFailure::new(
                    PairingFailure::RouteChanged,
                    BootstrapPhase::OpenSshAuthenticated,
                ))
            } else {
                Ok(())
            }
        }
    }
    struct Ssh {
        changed: Arc<AtomicBool>,
        bytes: Vec<u8>,
    }
    impl BootstrapProbeTransport for Ssh {
        fn run_read_only<'a>(
            &'a self,
            ssh: &'a dyn BootstrapProbeLease,
            script: &'static [u8],
            maximum: usize,
        ) -> PairingFuture<'a, Result<Vec<u8>, BootstrapProbeFailure>> {
            Box::pin(async move {
                ssh.revalidate()?;
                assert_eq!(
                    script,
                    spaceterm_host::bootstrap::command::read_only_probe_script()
                );
                assert_eq!(maximum, spaceterm_host::bootstrap::command::MAX_PROBE_BYTES);
                Ok(self.bytes.clone())
            })
        }
    }
    impl BootstrapOpenSsh for Ssh {
        fn authenticate<'a>(
            &'a self,
            route: &'a BootstrapRoute,
            attempt: &'a BootstrapAttempt,
        ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
            Box::pin(async move {
                attempt.validate(BootstrapPhase::ExplicitIntent)?;
                OpenSshAuthenticated::retain(*route, Box::new(Lease(self.changed.clone())))
            })
        }
    }
    struct Native {
        changed: Arc<AtomicBool>,
        change_after_inspection: bool,
        wrong_observation: bool,
        inspections: Arc<AtomicUsize>,
    }
    impl BootstrapAuthorizationBoundary for Native {
        fn authorize<'a>(
            &'a self,
            _: &'a BootstrapPlan,
            _: &'a dyn ScopedAuthenticationPrompt,
        ) -> PairingFuture<'a, Result<Box<dyn BootstrapAuthorizedOperation>, BootstrapFailure>>
        {
            Box::pin(async { panic!("read-only preflight cannot authorize native effects") })
        }
    }
    impl BootstrapNativePreflight for Native {
        fn inspect<'a>(
            &'a self,
            route: BootstrapRoute,
            host: &'a ReadOnlyHostProbe,
        ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>> {
            Box::pin(async move {
                self.inspections.fetch_add(1, Ordering::SeqCst);
                let mut facts = BootstrapHostFacts {
                    route,
                    account_uid: host.account_uid,
                    os: host.os,
                    architecture: host.architecture,
                    os_version: host.os_version,
                    libc: host.libc,
                    kernel: host.kernel,
                    supervision: host.supervision,
                    registration: BootstrapRegistration::Missing,
                    storage: BootstrapPrerequisite::Available,
                    installation: BootstrapPrerequisite::Available,
                    linger: host.linger,
                    sudo: BootstrapSudo::ResponseRequired,
                    authorization_agent: true,
                    background_items_allowed: true,
                };
                if self.wrong_observation {
                    facts.kernel = HostVersion::new(6, 0, 0);
                }
                if self.change_after_inspection {
                    self.changed.store(true, Ordering::Release);
                }
                Ok(facts)
            })
        }
        fn resolve(
            &self,
            _: [u8; 16],
            _: [u8; 32],
        ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>> {
            Box::pin(async { panic!("read-only preflight cannot resolve a native mutation") })
        }
    }
    fn setup(
        bytes: Vec<u8>,
        wrong_observation: bool,
        change_after_inspection: bool,
    ) -> (SshNativeServerSetup<Ssh, Native>, Arc<AtomicUsize>) {
        let changed = Arc::new(AtomicBool::new(false));
        let inspections = Arc::new(AtomicUsize::new(0));
        (
            SshNativeServerSetup::new(
                Ssh {
                    changed: changed.clone(),
                    bytes,
                },
                Native {
                    changed,
                    change_after_inspection,
                    wrong_observation,
                    inspections: inspections.clone(),
                },
            ),
            inspections,
        )
    }
    #[tokio::test]
    async fn bootstrap_native_preflight_merges_only_matching_independent_observations() {
        let (setup, inspections) = setup(PROBE.to_vec(), false, false);
        let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
        let attempt = intent.cancellation();
        let route = route();
        let ssh = setup.authenticate(&route, &attempt).await.unwrap();
        let facts = setup.discover(&ssh).await.unwrap();
        assert_eq!(facts.os_version, HostVersion::new(22, 4, 0));
        assert_eq!(facts.registration, BootstrapRegistration::Missing);
        assert_eq!(inspections.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn bootstrap_native_preflight_rejects_disagreement_with_the_read_only_probe() {
        let (setup, _) = setup(PROBE.to_vec(), true, false);
        let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
        let attempt = intent.cancellation();
        let route = route();
        let ssh = setup.authenticate(&route, &attempt).await.unwrap();
        assert_eq!(
            setup.discover(&ssh).await.err().unwrap().class,
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        );
    }
    #[tokio::test]
    async fn bootstrap_route_change_during_native_inspection_discards_its_completion() {
        let (setup, _) = setup(PROBE.to_vec(), false, true);
        let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
        let attempt = intent.cancellation();
        let route = route();
        let ssh = setup.authenticate(&route, &attempt).await.unwrap();
        assert_eq!(
            setup.discover(&ssh).await.err().unwrap().class,
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        );
    }
    #[tokio::test]
    async fn malformed_probe_is_content_free_and_prevents_native_inspection() {
        let (setup, inspections) = setup(b"untrusted-secret-and-path".to_vec(), false, false);
        let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
        let attempt = intent.cancellation();
        let route = route();
        let ssh = setup.authenticate(&route, &attempt).await.unwrap();
        let failure = setup.discover(&ssh).await.err().unwrap();
        assert_eq!(failure.class, BootstrapFailureClass::InvalidProbe);
        assert!(!failure.to_string().contains("untrusted-secret-and-path"));
        assert_eq!(inspections.load(Ordering::SeqCst), 0);
    }
}
