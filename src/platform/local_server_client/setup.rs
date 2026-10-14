//! Explicit local setup over the host's confirmed install and native owner enrollment.

use super::{
    classify, discover, enrollment,
    foreground::{ForegroundSetup, SetupKind},
};
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::JournalController,
    ports::ClientFuture,
};
use spaceterm_client_native::NativeClientProvider;
use spaceterm_host::{
    account_service::{ServiceOutcome, ServicePhase, ServiceRequestState},
    native_deployment::contracts::NativeRequestId,
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::Arc;

pub(crate) fn offer(
    native: NativeClientProvider,
    journal: Arc<JournalController>,
    legacy: Option<spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation>,
) -> Result<Option<Arc<dyn ForegroundSetup>>, ClientFailure> {
    let local = discover(&native, &journal)?;
    if local.as_ref().is_some_and(|local| local.binding.is_some()) {
        return Ok(None);
    }
    let expected = local.as_ref().map(|local| local.registration.server_pin());
    let snapshot = journal.snapshot()?;
    // A previous pairing or incomplete preparation needs deliberate recovery,
    // never a launch-time replacement identity or another authorization request.
    if expected.is_some_and(|pin| {
        snapshot.bindings().iter().any(|saved| {
            saved
                .binding()
                .is_ok_and(|binding| binding.target().pin() == pin)
        }) || snapshot
            .tombstones()
            .iter()
            .any(|suppressed| suppressed.parts().server.bytes() == pin.identity)
    }) {
        return Ok(None);
    }
    let installs = native
        .management()
        .supervisor()
        .retained_initial_install_requests()
        .map_err(classify)?;
    let pending: Vec<_> = installs
        .iter()
        .filter(|progress| progress.phase != ServicePhase::Completed)
        .collect();
    if pending.len() > 1 {
        return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired));
    }
    let install = pending.first().map(|progress| progress.request);
    Ok(Some(Arc::new(NativeSetup {
        native,
        journal,
        expected,
        legacy: if expected.is_none() && install.is_none() {
            legacy
        } else {
            None
        },
        install: tokio::sync::Mutex::new(install),
        execution: tokio::sync::Mutex::new(()),
    })))
}

struct NativeSetup {
    native: NativeClientProvider,
    journal: Arc<JournalController>,
    expected: Option<ServerPin>,
    legacy: Option<spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation>,
    install: tokio::sync::Mutex<Option<NativeRequestId>>,
    execution: tokio::sync::Mutex<()>,
}
impl NativeSetup {
    fn seed(
        &self,
        assessment: spaceterm_host::account_service::ServiceInstallAssessment,
    ) -> spaceterm_host::account_service::ServiceInstallAssessment {
        if self.expected.is_none() {
            assessment.with_local_desktop_seed(self.legacy.clone())
        } else {
            assessment
        }
    }
}
impl ForegroundSetup for NativeSetup {
    fn kind(&self) -> SetupKind {
        if self.expected.is_some() {
            SetupKind::Enroll
        } else {
            SetupKind::Install
        }
    }
    fn execute(&self) -> ClientFuture<'_, ()> {
        Box::pin(async {
            let _execution = self.execution.lock().await;
            self.journal.ensure_healthy()?;
            let management = self.native.management();
            let supervisor = management.supervisor();
            let registration = supervisor.discover().map_err(classify)?;
            if let Some(pin) = self.expected
                && registration
                    .as_ref()
                    .is_none_or(|registration| registration.server_pin() != pin)
            {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            let mut retained = self.install.lock().await;
            if registration.is_none() || retained.is_some() {
                let progress = if let Some(request) = *retained {
                    let progress = supervisor
                        .resolve_install(request)
                        .await
                        .map_err(classify)?;
                    if progress.phase != ServicePhase::Completed {
                        // A crash before archive staging may need the original bytes.
                        // The host rejects a newer feed rather than replacing this request.
                        let artifact = management
                            .download_installation(None)
                            .await
                            .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?;
                        supervisor
                            .resume_install_artifact(request, artifact)
                            .await
                            .map_err(classify)?
                    } else {
                        progress
                    }
                } else {
                    let artifact = management
                        .download_installation(None)
                        .await
                        .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?;
                    let assessment =
                        self.seed(supervisor.assess_install(artifact).map_err(classify)?);
                    let request = supervisor.allocate_request().map_err(classify)?;
                    *retained = Some(request);
                    // Only this method is reachable after the foreground affirmative action.
                    supervisor
                        .install(request, assessment.confirm())
                        .await
                        .map_err(classify)?
                };
                if progress.phase != ServicePhase::Completed
                    || !matches!(
                        progress.state,
                        ServiceRequestState::Completed(ServiceOutcome::Installed { .. })
                    )
                {
                    return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired));
                }
            }
            drop(retained);
            enrollment::enroll(&self.native, self.journal.clone()).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::local_server_client::tests::{Root, journal};
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        startup::{BuildIdentity, DesiredServerState},
    };

    #[test]
    fn first_run_offer_is_read_only_and_existing_stopped_owner_requires_only_pairing() {
        let root = Root::new();
        let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
        let (_runtime, journal) = journal();
        let native = NativeClientProvider::from_supervisor(fixture.reopen());
        assert_eq!(
            offer(native.clone(), journal.clone(), None)
                .unwrap()
                .unwrap()
                .kind(),
            SetupKind::Install
        );
        assert!(!fixture.anchor().exists());
        assert!(!fixture.data_directory().exists());
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        assert_eq!(
            offer(native, journal, None).unwrap().unwrap().kind(),
            SetupKind::Enroll
        );
        assert_eq!(
            fixture.reopen().discover().unwrap().unwrap().server_pin(),
            registration.server_pin()
        );
        assert_eq!(
            fixture
                .reopen()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Stopped
        );
    }

    #[test]
    fn foreground_pairing_refuses_stale_registration_before_credentials_or_start() {
        let root = Root::new();
        let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        let (runtime, journal) = journal();
        let setup = NativeSetup {
            native: NativeClientProvider::from_supervisor(fixture.reopen()),
            journal: journal.clone(),
            expected: Some(ServerPin {
                identity: [5; 16],
                public_key: [5; 32],
            }),
            legacy: None,
            install: tokio::sync::Mutex::new(None),
            execution: tokio::sync::Mutex::new(()),
        };
        let before = journal.snapshot().unwrap().revision();
        assert_eq!(
            runtime.block_on(setup.execute()).unwrap_err().class(),
            ClientFailureClass::StaleAuthority
        );
        assert_eq!(journal.snapshot().unwrap().revision(), before);
        assert_eq!(
            fixture.reopen().discover().unwrap().unwrap().server_pin(),
            registration.server_pin()
        );
        assert_eq!(
            fixture
                .reopen()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Stopped
        );
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod installation_tests;
