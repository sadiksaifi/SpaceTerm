use spaceterm_host::session_capabilities::{
    Capability, CapabilityFailure, CapabilityMatrix, CapabilityObservation, GuiSessionFacts,
    GuiSessionObserver, NormalizedShellObservation, ProbeContext, ProbeDeadline, ProbeOutcome,
    SessionAdapterBroker, SessionPurpose, ShellStartupPhase,
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
fn local_environment_facts_bind_context_and_reference_without_claiming_cross_process_equality() {
    use spaceterm_host::session_capabilities::{
        LocalEnvironmentComparison, LocaleDifference, PathDifference,
    };
    for context in [ProbeContext::GuiParented, ProbeContext::DaemonParented] {
        let values = LocalEnvironmentComparison::decode(
            &record([2, context as u32, 1, 3, 3, 0, 0, 0])[..20],
        )
        .unwrap();
        assert_eq!(values.context(), context);
        assert_eq!(values.path(), PathDifference::ComponentsChanged);
        assert_eq!(values.locale(), LocaleDifference::ValuesChanged);
    }
    for fields in [
        [1, 1, 2, 1, 1, 0, 0, 0], // Retired cross-process format.
        [2, 0, 1, 1, 1, 0, 0, 0],
        [2, 1, 2, 1, 1, 0, 0, 0], // Unknown local reference.
        [2, 1, 1, 0, 1, 0, 0, 0],
        [2, 1, 1, 1, 4, 0, 0, 0],
    ] {
        assert_eq!(
            LocalEnvironmentComparison::decode(&record(fields)[..20]),
            Err(CapabilityFailure::InvalidRecord)
        );
    }
    assert!(LocalEnvironmentComparison::decode(b"PATH=/synthetic/bin").is_err());
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

#[cfg(unix)]
#[test]
fn replaced_agent_socket_cannot_complete_the_original_session_operation() {
    use spaceterm_host::session_capabilities::{
        SystemGuiSessionObserver, native::SystemSessionConnector,
    };
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join(".gnupg");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("S.gpg-agent");
    let original = UnixListener::bind(&path).unwrap();
    let account = fixture_account(unsafe { libc::getuid() });
    let broker = SessionAdapterBroker::new(SystemGuiSessionObserver::with_native_peer(
        AccountPeer(account.clone()),
        account.key(),
        NativeSessionPeer(Arc::new(Mutex::new(Some([1; 16])))),
    ));
    let lease = broker
        .bind(SessionPurpose::GpgAgent, 100, Duration::from_secs(1))
        .unwrap();
    let connector =
        SystemSessionConnector::with_locations(&account, Locations(temporary.path().into()));
    let result = connector.with_connection(
        &broker,
        &lease,
        SessionPurpose::GpgAgent,
        || 101,
        |_| {
            original.accept().unwrap();
            std::fs::remove_file(&path).unwrap();
            let replacement = UnixListener::bind(&path).unwrap();
            assert!(replacement.local_addr().is_ok());
            Ok(())
        },
    );
    assert_eq!(result, Err(CapabilityFailure::StaleAssessment));
}

#[cfg(all(target_os = "linux", feature = "native-tests"))]
#[test]
fn native_linux_user_bus_authentication_uses_only_the_bound_account_session() {
    use spaceterm_host::session_capabilities::{
        SystemGuiSessionObserver, native::SystemSessionConnector,
    };
    let account = fixture_account(unsafe { libc::getuid() });
    let broker = SessionAdapterBroker::new(SystemGuiSessionObserver::new(
        AccountPeer(account.clone()),
        account.key(),
    ));
    let now = || {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) },
            0
        );
        value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
    };
    let lease = broker
        .bind(SessionPurpose::UserBus, now(), Duration::from_secs(1))
        .unwrap();
    SystemSessionConnector::new(&account)
        .with_connection(&broker, &lease, SessionPurpose::UserBus, now, |stream| {
            let identity: String = account
                .key()
                .account_uid
                .to_string()
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            stream
                .write_all(format!("\0AUTH EXTERNAL {identity}\r\n").as_bytes())
                .map_err(|_| CapabilityFailure::NativeUnavailable)?;
            let mut response = [0_u8; 128];
            let mut used = 0;
            loop {
                if used == response.len() {
                    return Err(CapabilityFailure::LimitExceeded);
                }
                stream
                    .read_exact(&mut response[used..used + 1])
                    .map_err(|_| CapabilityFailure::NativeUnavailable)?;
                used += 1;
                if response[..used].ends_with(b"\r\n") {
                    break;
                }
            }
            let authenticated = response[..used].starts_with(b"OK ");
            response.fill(0);
            if authenticated {
                Ok(())
            } else {
                Err(CapabilityFailure::NativeUnavailable)
            }
        })
        .unwrap();
}

#[test]
fn native_session_issuance_rejects_account_change_during_observation() {
    use spaceterm_host::session_capabilities::SystemGuiSessionObserver;
    use std::sync::atomic::{AtomicU8, Ordering};
    struct ChangingAccount(AtomicU8);
    impl NativeAccountProvider for ChangingAccount {
        fn observe(&self, key: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
            let generation = self.0.fetch_add(1, Ordering::Relaxed);
            AccountBinding::new(key, 20, NativeAccountId::macos([generation; 16])?)
        }
    }
    let key = fixture_account(501).key();
    let observer = SystemGuiSessionObserver::with_native_peer(
        ChangingAccount(AtomicU8::new(1)),
        key,
        NativeSessionPeer(Arc::new(Mutex::new(Some([1; 16])))),
    );
    assert_eq!(observer.observe(), Err(CapabilityFailure::StaleAssessment));
}

#[test]
fn native_session_issuance_rejects_session_change_during_observation() {
    use spaceterm_host::session_capabilities::{NativeGuiSessionPeer, SystemGuiSessionObserver};
    use std::sync::atomic::{AtomicU8, Ordering};
    struct ChangingSession(AtomicU8);
    impl NativeGuiSessionPeer for ChangingSession {
        fn observe(&self, _: u32) -> Result<Option<[u8; 16]>, CapabilityFailure> {
            Ok(Some([self.0.fetch_add(1, Ordering::Relaxed); 16]))
        }
    }
    let account = fixture_account(501);
    let observer = SystemGuiSessionObserver::with_native_peer(
        AccountPeer(account.clone()),
        account.key(),
        ChangingSession(AtomicU8::new(1)),
    );
    assert_eq!(observer.observe(), Err(CapabilityFailure::StaleAssessment));
}

#[cfg(unix)]
struct SessionIoFixture {
    _directory: tempfile::TempDir,
    listener: std::os::unix::net::UnixListener,
    connector: spaceterm_host::session_capabilities::native::SystemSessionConnector<Locations>,
    broker: SessionAdapterBroker<SessionObserver>,
    current: Arc<Mutex<Option<GuiSessionFacts>>>,
}
#[cfg(unix)]
fn session_io_fixture() -> SessionIoFixture {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let socket_directory = directory.path().join(".gnupg");
    std::fs::create_dir(&socket_directory).unwrap();
    std::fs::set_permissions(&socket_directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener =
        std::os::unix::net::UnixListener::bind(socket_directory.join("S.gpg-agent")).unwrap();
    let account = fixture_account(unsafe { libc::getuid() });
    let current = Arc::new(Mutex::new(Some(
        GuiSessionFacts::new(account.sha256(), [1; 16]).unwrap(),
    )));
    let broker = SessionAdapterBroker::new(SessionObserver(current.clone()));
    let connector =
        spaceterm_host::session_capabilities::native::SystemSessionConnector::with_locations(
            &account,
            Locations(directory.path().into()),
        );
    SessionIoFixture {
        _directory: directory,
        listener,
        connector,
        broker,
        current,
    }
}

#[cfg(unix)]
#[test]
fn slow_drip_response_cannot_extend_the_original_session_lease() {
    use std::io::Write;
    let SessionIoFixture {
        _directory,
        listener,
        connector,
        broker,
        ..
    } = session_io_fixture();
    let started = std::time::Instant::now();
    let now = || started.elapsed().as_nanos() as u64;
    let lease = broker
        .bind(SessionPurpose::GpgAgent, now(), Duration::from_millis(250))
        .unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(60));
            if socket.write_all(b"x").is_err() {
                break;
            }
        }
    });
    let result =
        connector.with_connection(&broker, &lease, SessionPurpose::GpgAgent, now, |socket| {
            let mut reply = [0; 8];
            socket.read_exact(&mut reply)
        });
    let elapsed = started.elapsed();
    peer.join().unwrap();
    assert_eq!(result, Err(CapabilityFailure::StaleAssessment));
    assert!(
        elapsed < Duration::from_millis(400),
        "session operation outlived its lease"
    );
}

#[cfg(unix)]
#[test]
fn expired_session_sends_no_request_after_the_operation_starts() {
    use std::io::Read;
    let SessionIoFixture {
        _directory,
        listener,
        connector,
        broker,
        ..
    } = session_io_fixture();
    let started = std::time::Instant::now();
    let now = || started.elapsed().as_nanos() as u64;
    let lease = broker
        .bind(SessionPurpose::GpgAgent, now(), Duration::from_millis(150))
        .unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 16];
        socket.read(&mut request).unwrap()
    });
    let result =
        connector.with_connection(&broker, &lease, SessionPurpose::GpgAgent, now, |socket| {
            std::thread::sleep(Duration::from_millis(200));
            socket.write_all(b"GETINFO version\n")
        });
    let sent = peer.join().unwrap();
    assert_eq!(result, Err(CapabilityFailure::StaleAssessment));
    assert_eq!(sent, 0, "expired session sent a request");
}

#[cfg(unix)]
#[test]
fn logout_during_read_rejects_the_next_request() {
    use std::io::{ErrorKind, Read, Write};
    let SessionIoFixture {
        _directory,
        listener,
        connector,
        broker,
        current,
    } = session_io_fixture();
    let started = std::time::Instant::now();
    let now = || started.elapsed().as_nanos() as u64;
    let lease = broker
        .bind(SessionPurpose::GpgAgent, now(), Duration::from_secs(1))
        .unwrap();
    let (reading, read_started) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        // Darwin can reject socket options once logout closes the peer. Bound
        // the fixture read before allowing that close, not after its greeting.
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        read_started.recv_timeout(Duration::from_secs(1)).unwrap();
        *current.lock().unwrap() = None;
        if let Err(error) = socket.write_all(b"OK\n") {
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::NotConnected
                ),
                "unexpected greeting failure kind={:?}",
                error.kind()
            );
        }
        let mut request = [0; 16];
        match socket.read(&mut request) {
            Ok(count) => count,
            // Closed peers can reset (Linux) or disconnect (Darwin). Neither
            // error credits a request; any successful bytes still fail.
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::NotConnected
                ) =>
            {
                0
            }
            Err(_) => panic!("unexpected fixture read failure"),
        }
    });
    let result =
        connector.with_connection(&broker, &lease, SessionPurpose::GpgAgent, now, |socket| {
            let mut greeting = [0; 3];
            reading.send(()).unwrap();
            socket.read_exact(&mut greeting)?;
            socket.write_all(b"GETINFO version\n")
        });
    let sent = peer.join().unwrap();
    assert_eq!(result, Err(CapabilityFailure::StaleAssessment));
    assert_eq!(sent, 0, "logged-out session sent a request");
}
