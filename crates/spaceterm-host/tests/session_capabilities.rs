use spaceterm_host::session_capabilities::{
    Capability, CapabilityFailure, CapabilityMatrix, CapabilityObservation, GuiSessionFacts,
    GuiSessionObserver, NormalizedShellObservation, ProbeContext, ProbeDeadline, ProbeOutcome,
    SessionAdapterBroker, SessionPurpose, ShellStartupPhase, UnavailableGuiSessionObserver,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn record(fields: [u32; 8]) -> Vec<u8> {
    fields.into_iter().flat_map(u32::to_be_bytes).collect()
}

#[test]
fn preflight_success_without_effect_is_not_passed() {
    let preflight_only = record([1, 3, 1, 4, 6, 1, 0, 0]);
    assert!(CapabilityObservation::decode(&preflight_only).is_ok());
    let falsely_passed = record([1, 3, 1, 1, 6, 1, 0, 0]);
    assert_eq!(
        CapabilityObservation::decode(&falsely_passed),
        Err(CapabilityFailure::InvalidRecord)
    );
}

#[test]
fn probe_output_rejects_paths_environment_values_and_native_errors() {
    for output in [
        b"/Users/private/Documents/marker".as_slice(),
        b"PATH=/private/bin:/usr/bin".as_slice(),
        b"NSError domain=private-error code=-1".as_slice(),
    ] {
        assert_eq!(
            CapabilityObservation::decode(output),
            Err(CapabilityFailure::InvalidRecord)
        );
    }
    let valid = record([1, 1, 1, 1, 6, 1, 1, 1]);
    for field in 0..8 {
        let mut invalid = valid.clone();
        invalid[field * 4..field * 4 + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            CapabilityObservation::decode(&invalid),
            Err(CapabilityFailure::InvalidRecord)
        );
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(CapabilityObservation::decode(&trailing).is_err());
    assert!(CapabilityObservation::decode(&valid[..31]).is_err());
    assert_eq!(
        CapabilityObservation::decode(&valid)
            .unwrap()
            .encode()
            .as_slice(),
        valid
    );
}

#[test]
fn gui_and_daemon_results_are_never_coalesced() {
    let gui = CapabilityObservation::decode(&record([1, 3, 1, 1, 6, 1, 1, 0])).unwrap();
    let daemon = CapabilityObservation::decode(&record([1, 3, 2, 2, 6, 2, 2, 0])).unwrap();
    let mut matrix = CapabilityMatrix::default();
    matrix.insert(gui).unwrap();
    assert_eq!(
        matrix.get(Capability::Microphone, ProbeContext::DaemonParented),
        None
    );
    matrix.insert(daemon).unwrap();
    assert_eq!(
        matrix
            .get(Capability::Microphone, ProbeContext::GuiParented)
            .unwrap()
            .outcome(),
        ProbeOutcome::Passed
    );
    assert_eq!(
        matrix
            .get(Capability::Microphone, ProbeContext::DaemonParented)
            .unwrap()
            .outcome(),
        ProbeOutcome::Denied
    );
    assert_eq!(matrix.recorded_count(), 2);
    assert_eq!(matrix.insert(gui), Err(CapabilityFailure::InvalidRecord));
}

#[test]
fn missing_volume_and_agent_are_unrun_or_unavailable() {
    for capability in [2, 10, 11] {
        for outcome in [4, 6] {
            assert!(
                CapabilityObservation::decode(&record([1, capability, 2, outcome, 6, 4, 0, 0]))
                    .is_ok()
            );
        }
    }
    assert!(CapabilityObservation::decode(&record([1, 2, 2, 6, 6, 4, 1, 0])).is_err());
}

#[test]
fn probe_timeout_does_not_reset_after_partial_output() {
    let deadline = ProbeDeadline::ordinary(1_000).unwrap();
    // Calls after partial output keep using the original deadline.
    assert_eq!(deadline.remaining(2_000).unwrap().as_nanos(), 4_999_999_000);
    assert_eq!(
        deadline.remaining(5_000_001_000),
        Err(CapabilityFailure::TimedOut)
    );
    assert_eq!(
        ProbeDeadline::user_mediated(0)
            .unwrap()
            .remaining(60_000_000_000),
        Err(CapabilityFailure::TimedOut)
    );
    assert!(ProbeDeadline::ordinary(u64::MAX).is_err());
}

#[derive(Clone)]
struct SessionObserver(Arc<Mutex<Option<GuiSessionFacts>>>);
impl GuiSessionObserver for SessionObserver {
    fn observe(&self) -> Result<Option<GuiSessionFacts>, CapabilityFailure> {
        Ok(*self.0.lock().unwrap())
    }
}

#[test]
fn session_adapter_rejects_logout_and_replacement_session() {
    let original = GuiSessionFacts::new([1; 32], [1; 16]).unwrap();
    let current = Arc::new(Mutex::new(Some(original)));
    let broker = SessionAdapterBroker::new(SessionObserver(current.clone()));
    let lease = broker
        .bind(SessionPurpose::SshAgent, 100, Duration::from_secs(1))
        .unwrap();
    assert!(
        broker
            .revalidate(&lease, SessionPurpose::SshAgent, 101)
            .is_ok()
    );
    *current.lock().unwrap() = None;
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::SshAgent, 102),
        Err(CapabilityFailure::StaleAssessment)
    );
    *current.lock().unwrap() = Some(GuiSessionFacts::new([1; 32], [2; 16]).unwrap());
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::SshAgent, 103),
        Err(CapabilityFailure::StaleAssessment)
    );
    *current.lock().unwrap() = Some(GuiSessionFacts::new([2; 32], [1; 16]).unwrap());
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::SshAgent, 104),
        Err(CapabilityFailure::StaleAssessment)
    );
}

#[test]
fn session_lease_rejects_other_broker_purpose_and_expiry() {
    let current = Arc::new(Mutex::new(Some(
        GuiSessionFacts::new([1; 32], [1; 16]).unwrap(),
    )));
    let broker = SessionAdapterBroker::new(SessionObserver(current.clone()));
    let other = SessionAdapterBroker::new(SessionObserver(current));
    let lease = broker
        .bind(SessionPurpose::Clipboard, 100, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        other.revalidate(&lease, SessionPurpose::Clipboard, 101),
        Err(CapabilityFailure::StaleAssessment)
    );
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::SshAgent, 101),
        Err(CapabilityFailure::StaleAssessment)
    );
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::Clipboard, 1_000_000_100),
        Err(CapabilityFailure::StaleAssessment)
    );
    assert!(
        broker
            .bind(SessionPurpose::Clipboard, 0, Duration::from_secs(31))
            .is_err()
    );
}

#[cfg(all(target_os = "macos", feature = "native-tests"))]
#[test]
fn native_unprivileged_session_observation_can_issue_an_expiring_lease() {
    use spaceterm_host::session_capabilities::{NativeGuiSessionPeer, SystemNativeGuiSessionPeer};
    // This Mac gate observes the existing GUI session without logout, TCC or
    // account mutation. A no-GUI host cannot credit this issuance regression.
    let uid = unsafe { libc::getuid() };
    let native = SystemNativeGuiSessionPeer::default();
    let observed = native.observe(uid).unwrap();
    assert!(observed.is_some());
    assert_eq!(native.observe(uid).unwrap(), observed);
    assert_eq!(native.observe(u32::MAX).unwrap(), None);
    let account = fixture_account(uid);
    let key = account.key();
    let broker = SessionAdapterBroker::new(
        spaceterm_host::session_capabilities::SystemGuiSessionObserver::new(
            AccountPeer(account),
            key,
        ),
    );
    let lease = broker
        .bind(SessionPurpose::GpgAgent, 100, Duration::from_millis(1))
        .unwrap();
    broker
        .revalidate(&lease, SessionPurpose::GpgAgent, 101)
        .unwrap();
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::GpgAgent, 1_000_100),
        Err(CapabilityFailure::StaleAssessment)
    );
}

#[test]
fn production_session_observer_remains_unavailable() {
    let broker = SessionAdapterBroker::new(UnavailableGuiSessionObserver);
    assert_eq!(
        broker
            .bind(SessionPurpose::SshAgent, 0, Duration::from_secs(1))
            .unwrap_err(),
        CapabilityFailure::NativeUnavailable
    );
}

#[test]
fn normalized_shell_comparison_preserves_login_interactive_order() {
    use ShellStartupPhase::*;
    let order = [Environment, LoginProfile, InteractiveRc, LoginCompletion];
    let baseline = NormalizedShellObservation::new(order, 5, 0, true).unwrap();
    let daemon = NormalizedShellObservation::new(order, 3, 1, false).unwrap();
    let difference = baseline.compare_counts(&daemon);
    assert!(difference.path_component_count_changed);
    assert!(difference.duplicate_component_count_changed);
    assert!(difference.locale_presence_changed);
    assert!(!difference.startup_order_changed);
    assert!(
        NormalizedShellObservation::new(
            [Environment, InteractiveRc, LoginProfile, LoginCompletion],
            5,
            0,
            true
        )
        .is_err()
    );
    assert!(NormalizedShellObservation::new(order, 257, 0, true).is_err());
    assert!(NormalizedShellObservation::new(order, 5, 6, true).is_err());
}

#[test]
fn cleanup_uncertainty_overrides_success_and_sudo_exit_is_not_touch_id_proof() {
    assert!(CapabilityObservation::decode(&record([1, 6, 1, 1, 6, 1, 1, 2])).is_err());
    assert!(CapabilityObservation::decode(&record([1, 6, 1, 10, 6, 1, 1, 2])).is_ok());
    assert!(CapabilityObservation::decode(&record([1, 12, 1, 1, 6, 1, 5, 0])).is_err());
    assert!(CapabilityObservation::decode(&record([1, 12, 1, 4, 6, 1, 5, 0])).is_ok());
}

#[test]
fn exact_environment_difference_is_separate_from_counts_and_rejects_coalesced_contexts() {
    use spaceterm_host::session_capabilities::{
        LocaleDifference, NormalizedEnvironmentDifference, PathDifference,
    };
    let values =
        NormalizedEnvironmentDifference::decode(&record([1, 1, 2, 3, 3, 0, 0, 0])[..20]).unwrap();
    assert_eq!(values.path(), PathDifference::ComponentsChanged);
    assert_eq!(values.locale(), LocaleDifference::ValuesChanged);
    let order =
        NormalizedEnvironmentDifference::decode(&record([1, 1, 2, 4, 1, 0, 0, 0])[..20]).unwrap();
    assert_eq!(order.path(), PathDifference::OrderChanged);
    assert_eq!(order.locale(), LocaleDifference::Equal);
    for fields in [
        [1, 1, 1, 1, 1, 0, 0, 0],
        [1, 2, 1, 1, 1, 0, 0, 0],
        [1, 1, 2, 0, 1, 0, 0, 0],
        [1, 1, 2, 1, 4, 0, 0, 0],
    ] {
        assert_eq!(
            NormalizedEnvironmentDifference::decode(&record(fields)[..20]),
            Err(CapabilityFailure::InvalidRecord)
        );
    }
    assert!(NormalizedEnvironmentDifference::decode(b"PATH=/synthetic/bin").is_err());
    assert!(NormalizedEnvironmentDifference::decode(&record([1, 1, 2, 1, 1, 0, 0, 0])).is_err());
}

use spaceterm_host::{
    native_deployment::account::{AccountBinding, NativeAccountId, NativeAccountProvider},
    native_deployment::contracts::NativeFailure,
    startup::{AccountBuildKey, BuildIdentity},
};
struct AccountPeer(AccountBinding);
impl NativeAccountProvider for AccountPeer {
    fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        if key == self.0.key() {
            Ok(self.0.clone())
        } else {
            Err(NativeFailure::IdentityMismatch)
        }
    }
}
fn fixture_account(uid: u32) -> AccountBinding {
    AccountBinding::new(
        AccountBuildKey {
            account_uid: uid,
            build_identity: BuildIdentity::Development,
        },
        20,
        NativeAccountId::macos([1; 16]).unwrap(),
    )
    .unwrap()
}
#[derive(Clone)]
struct NativeSessionPeer(Arc<Mutex<Option<[u8; 16]>>>);
impl spaceterm_host::session_capabilities::NativeGuiSessionPeer for NativeSessionPeer {
    fn observe(&self, _: u32) -> Result<Option<[u8; 16]>, CapabilityFailure> {
        Ok(*self.0.lock().unwrap())
    }
}
#[test]
fn native_observation_boundary_rechecks_account_session_and_logout() {
    use spaceterm_host::session_capabilities::SystemGuiSessionObserver;
    let current = Arc::new(Mutex::new(Some([1; 16])));
    let account = fixture_account(501);
    let key = account.key();
    let broker = SessionAdapterBroker::new(SystemGuiSessionObserver::with_native_peer(
        AccountPeer(account),
        key,
        NativeSessionPeer(current.clone()),
    ));
    let lease = broker
        .bind(SessionPurpose::UserBus, 100, Duration::from_secs(1))
        .unwrap();
    broker
        .revalidate(&lease, SessionPurpose::UserBus, 101)
        .unwrap();
    *current.lock().unwrap() = Some([2; 16]);
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::UserBus, 102),
        Err(CapabilityFailure::StaleAssessment)
    );
    *current.lock().unwrap() = None;
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::UserBus, 103),
        Err(CapabilityFailure::StaleAssessment)
    );
}

#[cfg(unix)]
struct Locations(std::path::PathBuf);
#[cfg(unix)]
impl spaceterm_host::session_capabilities::native::NativeSessionLocations for Locations {
    fn home(&self, _: u32) -> Result<std::path::PathBuf, CapabilityFailure> {
        Ok(self.0.clone())
    }
    fn runtime(&self, _: u32) -> Result<Option<std::path::PathBuf>, CapabilityFailure> {
        Ok(Some(self.0.clone()))
    }
}
#[cfg(unix)]
#[test]
fn modern_agent_discovery_does_not_need_legacy_environment_and_rechecks_completion() {
    use spaceterm_host::session_capabilities::{
        SystemGuiSessionObserver, native::SystemSessionConnector,
    };
    use std::{
        io::{Read, Write},
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path();
    std::fs::create_dir(home.join(".gnupg")).unwrap();
    std::fs::set_permissions(home.join(".gnupg"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(home.join(".gnupg/S.gpg-agent")).unwrap();
    let agent = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"OK fixture\n").unwrap();
        let mut query = [0; 16];
        stream.read_exact(&mut query).unwrap();
        assert_eq!(&query, b"GETINFO version\n");
        stream.write_all(b"D synthetic\nOK\n").unwrap();
    });
    let uid = unsafe { libc::getuid() };
    let account = fixture_account(uid);
    let key = account.key();
    let current = Arc::new(Mutex::new(Some([1; 16])));
    let broker = SessionAdapterBroker::new(SystemGuiSessionObserver::with_native_peer(
        AccountPeer(account.clone()),
        key,
        NativeSessionPeer(current.clone()),
    ));
    let lease = broker
        .bind(SessionPurpose::GpgAgent, 100, Duration::from_secs(1))
        .unwrap();
    let connector = SystemSessionConnector::with_locations(&account, Locations(home.into()));
    let result = connector.with_connection(
        &broker,
        &lease,
        SessionPurpose::GpgAgent,
        || 101,
        |stream| {
            let mut greeting = [0; 11];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(&greeting, b"OK fixture\n");
            stream.write_all(b"GETINFO version\n").unwrap();
            let mut reply = [0; 15];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"D synthetic\nOK\n");
            *current.lock().unwrap() = None;
            Ok(())
        },
    );
    agent.join().unwrap();
    assert_eq!(result, Err(CapabilityFailure::StaleAssessment));
    assert_eq!(
        connector.with_connection(&broker, &lease, SessionPurpose::UserBus, || 101, |_| Ok(())),
        Err(CapabilityFailure::StaleAssessment)
    );
}
#[cfg(target_os = "linux")]
#[test]
fn linux_user_bus_and_graphical_portal_are_distinct_session_purposes() {
    use spaceterm_host::session_capabilities::{
        SystemGuiSessionObserver, native::SystemSessionConnector,
    };
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(temporary.path().join("bus")).unwrap();
    let uid = unsafe { libc::getuid() };
    let account = fixture_account(uid);
    let key = account.key();
    let broker = SessionAdapterBroker::new(SystemGuiSessionObserver::with_native_peer(
        AccountPeer(account.clone()),
        key,
        NativeSessionPeer(Arc::new(Mutex::new(Some([1; 16])))),
    ));
    let bus = broker
        .bind(SessionPurpose::UserBus, 100, Duration::from_secs(1))
        .unwrap();
    let portal = broker
        .bind(SessionPurpose::GraphicalPortal, 100, Duration::from_secs(1))
        .unwrap();
    let connector =
        SystemSessionConnector::with_locations(&account, Locations(temporary.path().into()));
    assert_eq!(
        connector.with_connection(
            &broker,
            &bus,
            SessionPurpose::GraphicalPortal,
            || 101,
            |_| Ok(())
        ),
        Err(CapabilityFailure::StaleAssessment)
    );
    for (lease, purpose) in [
        (&bus, SessionPurpose::UserBus),
        (&portal, SessionPurpose::GraphicalPortal),
    ] {
        connector
            .with_connection(&broker, lease, purpose, || 101, |_| Ok(()))
            .unwrap();
        listener.accept().unwrap();
    }
}

#[cfg(all(target_os = "linux", feature = "native-tests"))]
#[test]
fn native_linux_session_observation_can_issue_an_expiring_lease() {
    use spaceterm_host::session_capabilities::{
        NativeGuiSessionPeer, SystemGuiSessionObserver, SystemNativeGuiSessionPeer,
    };
    let uid = unsafe { libc::getuid() };
    let native = SystemNativeGuiSessionPeer::default();
    assert!(native.observe(uid).unwrap().is_some());
    let account = fixture_account(uid);
    let key = account.key();
    let broker =
        SessionAdapterBroker::new(SystemGuiSessionObserver::new(AccountPeer(account), key));
    let lease = broker
        .bind(
            SessionPurpose::GraphicalPortal,
            100,
            Duration::from_millis(1),
        )
        .unwrap();
    broker
        .revalidate(&lease, SessionPurpose::GraphicalPortal, 101)
        .unwrap();
    assert_eq!(
        broker.revalidate(&lease, SessionPurpose::GraphicalPortal, 1_000_100),
        Err(CapabilityFailure::StaleAssessment)
    );
}
