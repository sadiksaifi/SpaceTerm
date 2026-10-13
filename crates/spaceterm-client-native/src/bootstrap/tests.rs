use super::*;
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    pairing::{EnrollmentProposalParts, PairingKeys, PairingKeysParts},
};
use spaceterm_distribution::BuildVersion;
use spaceterm_host::{
    account_service::SupervisorState,
    bootstrap::linger::{LingerInvocation, ScopedLingerAuthentication},
    startup::AccountBuildKey,
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct State {
    native: Mutex<BootstrapAccountFacts>,
    live: AtomicBool,
    reservations: AtomicUsize,
    installs: AtomicUsize,
    owners: AtomicUsize,
    sudo: AtomicUsize,
}
#[derive(Clone)]
struct Transport(Arc<State>);
fn route() -> BootstrapRoute {
    BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 501, BuildChannel::Development).unwrap()
}
impl BootstrapProbeLease for Transport {
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure> {
        if self.0.live.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(BootstrapProbeFailure::RouteChanged)
        }
    }
    fn account_uid(&self) -> u32 {
        501
    }
}
struct Lease(Transport);
impl BootstrapSshLease for Lease {
    fn revalidate(&self, captured: BootstrapRoute) -> Result<(), BootstrapFailure> {
        recheck(&self.0, captured)
    }
}
impl BootstrapOpenSsh for Transport {
    fn authenticate<'a>(
        &'a self,
        captured: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
        Box::pin(async move {
            attempt.validate(BootstrapPhase::ExplicitIntent)?;
            OpenSshAuthenticated::retain(*captured, Box::new(Lease(self.clone())))
        })
    }
}
impl BootstrapProbeTransport for Transport {
    fn run_read_only<'a>(
        &'a self,
        lease: &'a dyn BootstrapProbeLease,
        script: &'static [u8],
        maximum: usize,
    ) -> PairingFuture<'a, Result<Vec<u8>, BootstrapProbeFailure>> {
        Box::pin(async move {
            lease.revalidate()?;
            assert_eq!(
                script,
                spaceterm_host::bootstrap::command::read_only_probe_script()
            );
            assert_eq!(maximum, spaceterm_host::bootstrap::command::MAX_PROBE_BYTES);
            let linger = match self.0.native.lock().unwrap().linger {
                Some(LingerState::Enabled) => "yes",
                Some(LingerState::Disabled) => "no",
                _ => "unavailable",
            };
            Ok(format!("STBP2\nuid=501\nos=Linux\narch=x86_64\nversion=22.04\nlibc=glibc 2.35\nkernel=5.15.0\nmanager=systemd-user\nmanager_version=249\nmanager_reachable=yes\nlinger={linger}\nsudo=available\n").into_bytes())
        })
    }
}
impl BootstrapLingerTransport for Transport {
    type Exchange = u32;
    fn retain<'a>(
        &'a self,
        lease: &'a dyn BootstrapProbeLease,
        invocation: &'a LingerInvocation,
    ) -> LingerFuture<'a, Self::Exchange> {
        Box::pin(async move {
            lease
                .revalidate()
                .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
            assert_eq!(invocation.executable(), "/usr/bin/sudo");
            assert_eq!(
                invocation.arguments(),
                [
                    "-k",
                    "-p",
                    "[SpaceTerm linger password]",
                    "--",
                    "/usr/bin/loginctl",
                    "enable-linger",
                    "--",
                    "501"
                ]
                .map(str::to_owned)
            );
            Ok(invocation.account_uid())
        })
    }
    fn revalidate(&self, exchange: &Self::Exchange) -> Result<(), BootstrapLingerFailure> {
        if *exchange == 501 && self.0.live.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(BootstrapLingerFailure::RouteChanged)
        }
    }
    fn invoke<'a>(
        &'a self,
        exchange: &'a mut Self::Exchange,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            BootstrapLingerTransport::revalidate(self, exchange)?;
            let response = authentication.respond(invocation.account_uid()).await?;
            assert_eq!(response.as_slice(), b"fixture response");
            self.0.sudo.fetch_add(1, Ordering::AcqRel);
            self.0.native.lock().unwrap().linger = Some(LingerState::Enabled);
            Ok(())
        })
    }
    fn observe<'a>(
        &'a self,
        exchange: &'a Self::Exchange,
        uid: u32,
    ) -> LingerFuture<'a, BootstrapLinger> {
        Box::pin(async move {
            BootstrapLingerTransport::revalidate(self, exchange)?;
            assert_eq!(uid, 501);
            Ok(BootstrapLinger::Enabled)
        })
    }
}
impl BootstrapAccountTransport for Transport {
    fn route(&self) -> BootstrapRoute {
        route()
    }
    fn inspect(&self) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>> {
        Box::pin(async { Ok(self.0.native.lock().unwrap().clone()) })
    }
    fn reserve_request(&self) -> PairingFuture<'_, Result<[u8; 16], BootstrapFailure>> {
        Box::pin(async {
            self.0.reservations.fetch_add(1, Ordering::AcqRel);
            Ok([8; 16])
        })
    }
    fn install(
        &self,
        _: [u8; 16],
        _: VerifiedServerArtifact,
    ) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>> {
        Box::pin(async {
            self.0.installs.fetch_add(1, Ordering::AcqRel);
            Err(failure(
                PairingFailure::InvalidRequest,
                BootstrapPhase::Applied,
            ))
        })
    }
    fn authorize_owner<'a>(
        &'a self,
        request: [u8; 16],
        expected: &'a BootstrapAccountRegistration,
        proposal: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapAccountFacts, BootstrapFailure>> {
        Box::pin(async move {
            assert_eq!(request, [8; 16]);
            assert_eq!(proposal.parts().server.bytes(), [4; 16]);
            let native = self.0.native.lock().unwrap().clone();
            assert_eq!(&native.registration, expected);
            self.0.owners.fetch_add(1, Ordering::AcqRel);
            Ok(native)
        })
    }
    fn resolve(
        &self,
        _: [u8; 16],
        _: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>> {
        Box::pin(async { self.inspect().await })
    }
}
struct Enrollment;
impl BootstrapEnrollment for Enrollment {
    fn preflight<'a>(
        &'a self,
        _: BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move { attempt.validate(BootstrapPhase::ReadOnlyPreflight) })
    }
    fn prepare_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<EnrollmentProposal, BootstrapFailure>> {
        Box::pin(async move {
            attempt.validate(BootstrapPhase::Planned)?;
            Ok(EnrollmentProposal::new(EnrollmentProposalParts {
                server: ServerId::new(server.pin.identity),
                device: DeviceId::new([7; 16]),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [9; 32],
                    pairing_authentication_public_key: [10; 32],
                })
                .unwrap(),
                claim_nonce: [11; 32],
            })
            .unwrap())
        })
    }
    fn enroll_owner<'a>(
        &'a self,
        _: &'a BootstrapServer,
        request: [u8; 16],
        _: &'a EnrollmentProposal,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            assert_eq!(request, [8; 16]);
            attempt.validate(BootstrapPhase::Enrolled)
        })
    }
    fn connect<'a>(
        &'a self,
        _: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move { attempt.validate(BootstrapPhase::Connected) })
    }
}
struct Prompt(Arc<AtomicUsize>);
impl ScopedAuthenticationPrompt for Prompt {
    fn revalidate(&self, _: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        Ok(())
    }
    fn respond<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            assert!(plan.requires_linger_authorization());
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(Zeroizing::new(b"fixture response".to_vec()))
        })
    }
}
fn transport(linger: LingerState) -> Transport {
    Transport(Arc::new(State {
        native: Mutex::new(BootstrapAccountFacts {
            key: AccountBuildKey {
                account_uid: 501,
                build_identity: BuildIdentity::Development,
            },
            registration: BootstrapAccountRegistration::Running {
                pin: ServerPin {
                    identity: [4; 16],
                    public_key: [5; 32],
                },
                generation: 1,
                artifact_identity: [6; 32],
                version: BuildVersion::parse("1.2.3").unwrap(),
                protocol_majors: vec![1, 2],
            },
            supervisor: SupervisorState::Running,
            linger: Some(linger),
        }),
        live: AtomicBool::new(true),
        reservations: AtomicUsize::new(0),
        installs: AtomicUsize::new(0),
        owners: AtomicUsize::new(0),
        sudo: AtomicUsize::new(0),
    }))
}

#[tokio::test]
async fn bootstrap_native_running_reuse_skips_installation_and_linger_prompt() {
    let ssh = transport(LingerState::Enabled);
    let responses = Arc::new(AtomicUsize::new(0));
    let native = NativeBootstrapAccount::new(Arc::new(ssh.clone())).unwrap();
    let coordinator = BootstrapCoordinator::new(
        Arc::new(SshNativeServerSetup::new(ssh.clone(), native)),
        Enrollment,
        Prompt(Arc::clone(&responses)),
    );
    let outcome = coordinator
        .bootstrap(ForegroundBootstrap::confirm(route(), 1).unwrap(), None)
        .await
        .unwrap();
    assert!(outcome.reused);
    assert_eq!(outcome.phase, BootstrapPhase::Connected);
    assert_eq!(ssh.0.reservations.load(Ordering::Acquire), 1);
    assert_eq!(ssh.0.owners.load(Ordering::Acquire), 1);
    assert_eq!(ssh.0.installs.load(Ordering::Acquire), 0);
    assert_eq!(ssh.0.sudo.load(Ordering::Acquire), 0);
    assert_eq!(responses.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_native_disabled_linger_uses_only_scoped_sudo_before_owner_request() {
    let ssh = transport(LingerState::Disabled);
    let responses = Arc::new(AtomicUsize::new(0));
    let native = NativeBootstrapAccount::new(Arc::new(ssh.clone())).unwrap();
    let coordinator = BootstrapCoordinator::new(
        Arc::new(SshNativeServerSetup::new(ssh.clone(), native)),
        Enrollment,
        Prompt(Arc::clone(&responses)),
    );
    let outcome = coordinator
        .bootstrap(ForegroundBootstrap::confirm(route(), 1).unwrap(), None)
        .await
        .unwrap();
    assert!(outcome.reused);
    assert_eq!(responses.load(Ordering::Acquire), 1);
    assert_eq!(ssh.0.sudo.load(Ordering::Acquire), 1);
    assert_eq!(ssh.0.installs.load(Ordering::Acquire), 0);
    assert_eq!(ssh.0.owners.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn bootstrap_native_stopped_registration_cannot_reserve_or_authorize() {
    let ssh = transport(LingerState::Enabled);
    ssh.0.native.lock().unwrap().registration = BootstrapAccountRegistration::Stopped {
        pin: ServerPin {
            identity: [4; 16],
            public_key: [5; 32],
        },
        generation: 2,
    };
    let responses = Arc::new(AtomicUsize::new(0));
    let native = NativeBootstrapAccount::new(Arc::new(ssh.clone())).unwrap();
    let coordinator = BootstrapCoordinator::new(
        Arc::new(SshNativeServerSetup::new(ssh.clone(), native)),
        Enrollment,
        Prompt(responses),
    );
    let failure = coordinator
        .bootstrap(ForegroundBootstrap::confirm(route(), 1).unwrap(), None)
        .await
        .err()
        .unwrap();
    assert_eq!(
        failure.class,
        BootstrapFailureClass::Pairing(PairingFailure::ExplicitStartRequired)
    );
    assert_eq!(ssh.0.reservations.load(Ordering::Acquire), 0);
    assert_eq!(ssh.0.installs.load(Ordering::Acquire), 0);
    assert_eq!(ssh.0.owners.load(Ordering::Acquire), 0);
}
