use super::*;
use crate::protocol::version::ProtocolVersion;
use crate::service::runtime::test_support::{with_artifact, with_build, with_fixture_trust};
use crate::service::tests::Fixture;
use crate::service::{ServiceStartup, Supervisor};
use std::path::PathBuf;

/// A service manager no update step may reach: install and facts never touch the service.
struct Untouched;
impl Supervisor for Untouched {
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn check(&self, _: &Path, _: bool) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn install(&self, _: &Path) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn prepare_start(&self) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn start(&self) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn stop(&self) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn set_automatic_start(&self, _: bool) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn uninstall(&self, _: &Path) -> Result<(), CoreError> {
        panic!("an install never reaches the service manager")
    }
    fn running(&self) -> Result<bool, CoreError> {
        panic!("an install never reaches the service manager")
    }
}

/// The restarts a running server was asked to admit, as `(candidate, silent)`.
#[derive(Default)]
struct Admitted(Mutex<Vec<(Option<[u8; 32]>, bool)>>);
impl Restarter for Admitted {
    fn restart(
        self: Arc<Self>,
        candidate: Option<[u8; 32]>,
        silent: bool,
    ) -> Result<(), RestartRefusal> {
        self.0.lock().unwrap().push((candidate, silent));
        Ok(())
    }
}

struct Machine {
    fixture: Fixture,
    service: Service,
}

const IDENTITY: &str = "spaceterm-development";

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn with_major(work: u16, service: u16) -> Protocols {
    Protocols {
        work: ProtocolVersion {
            major: work,
            minor: 0,
        },
        service: ProtocolVersion {
            major: service,
            minor: 0,
        },
        desktop_channel: Protocols::CURRENT.desktop_channel,
    }
}

/// A signed archive of `version` and `protocols`, with its signature and SHA-256.
fn archive(machine: &Machine, text: &str, protocols: Protocols) -> (PathBuf, PathBuf, String) {
    let directory = machine
        .fixture
        .root
        .join(format!("archive-{text}-{}", protocols.work.major));
    with_build(text, protocols, || {
        with_artifact(&directory, IDENTITY, |archive, signature| {
            let sha = super::super::digest(&std::fs::read(archive).unwrap());
            (archive.to_owned(), signature.to_owned(), sha)
        })
    })
}

/// A Core running `running`, the version its service state approves.
fn machine(running: &str) -> Machine {
    let fixture = Fixture::new();
    let service = Service::new(
        fixture.paths.clone(),
        IDENTITY.to_owned(),
        Arc::new(Untouched),
    );
    let machine = Machine { fixture, service };
    let (archive, signature, sha) = archive(&machine, running, Protocols::CURRENT);
    let retained = machine
        .service
        .stage_candidate(&archive, &signature, &sha)
        .unwrap();
    let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
    let mut state = Service::state(&data).unwrap().unwrap();
    state.installation = retained
        .directory
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .into();
    data.write_json(c"service.json", &state).unwrap();
    machine
}

fn install(
    updates: &Updates,
    machine: &Machine,
    text: &str,
    protocols: Protocols,
    source: Source,
) -> Result<PendingVersion, InstallError> {
    let (archive, signature, sha) = archive(machine, text, protocols);
    updates.install(Archive {
        archive: &archive,
        signature: &signature,
        sha256: &sha,
        source,
    })
}

fn installations(machine: &Machine) -> Vec<String> {
    let mut versions: Vec<String> = std::fs::read_dir(machine.service.installations_directory())
        .unwrap()
        .map(|entry| {
            let build: serde_json::Value = serde_json::from_slice(
                &std::fs::read(entry.unwrap().path().join("build.json")).unwrap(),
            )
            .unwrap();
            build["version"].as_str().unwrap().to_owned()
        })
        .collect();
    versions.sort();
    versions
}

fn approved(machine: &Machine) -> String {
    let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
    Service::state(&data).unwrap().unwrap().installation
}

#[test]
fn an_install_waits_beside_the_running_version_and_changes_nothing_that_runs() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let before = approved(&machine);
        let updates = Updates::open(machine.service.clone()).unwrap();
        let pending = install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Local,
        )
        .unwrap();
        assert_eq!(pending.version, version("1.1.0"));
        assert_eq!(pending.protocols, Protocols::CURRENT);
        assert!(!pending.breaking);
        assert!(pending.stranded.is_empty());
        assert_eq!(pending.overdue_since, None);
        let facts = updates.facts();
        assert_eq!(facts.running, version("1.0.0"));
        assert_eq!(facts.pending, Some(pending));
        assert_eq!(facts.operation, None);
        assert_eq!(facts.check, UpdateCheck::Disabled);
        assert_eq!(approved(&machine), before);
        assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
    });
}

#[test]
fn a_core_refuses_any_version_not_newer_than_the_newest_it_has() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        install(
            &updates,
            &machine,
            "1.2.0",
            Protocols::CURRENT,
            Source::Push,
        )
        .unwrap();
        for older in ["0.9.9", "1.0.0", "1.1.9", "1.2.0"] {
            assert_eq!(
                updates.admits(&version(older)),
                Err(InstallError::NotNewer),
                "{older}"
            );
        }
        assert_eq!(
            install(
                &updates,
                &machine,
                "1.1.0",
                Protocols::CURRENT,
                Source::Local
            ),
            Err(InstallError::NotNewer)
        );
        assert_eq!(updates.admits(&version("1.10.0")), Ok(()));
        assert_eq!(installations(&machine), ["1.0.0", "1.2.0"]);
    });
}

#[test]
fn a_newer_install_replaces_the_waiting_one_so_at_most_two_versions_stay() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        install(
            &updates,
            &machine,
            "1.2.0",
            Protocols::CURRENT,
            Source::Push,
        )
        .unwrap();
        assert_eq!(installations(&machine), ["1.0.0", "1.2.0"]);
        assert_eq!(updates.facts().pending.unwrap().version, version("1.2.0"));
    });
}

#[test]
fn a_pending_version_of_another_work_or_service_major_is_breaking() {
    with_fixture_trust(|| {
        let current = Protocols::CURRENT;
        for (index, protocols) in [
            with_major(current.work.major + 1, current.service.major),
            with_major(current.work.major, current.service.major + 1),
        ]
        .into_iter()
        .enumerate()
        {
            let machine = machine("1.0.0");
            let updates = Updates::open(machine.service.clone()).unwrap();
            let pending = install(&updates, &machine, "2.0.0", protocols, Source::Local).unwrap();
            assert!(pending.breaking, "case {index}");
            assert_eq!(pending.protocols, protocols);
        }
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let mut minor = current;
        minor.work.minor += 1;
        assert!(
            !install(&updates, &machine, "1.1.0", minor, Source::Local)
                .unwrap()
                .breaking
        );
    });
}

#[test]
fn a_feed_install_that_fails_verification_records_it_and_a_local_one_does_not() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let (archive, signature, sha) = archive(&machine, "1.1.0", Protocols::CURRENT);
        let mut bad = std::fs::read(&signature).unwrap();
        bad[0] ^= 1;
        std::fs::write(&signature, bad).unwrap();
        for (source, recorded) in [(Source::Local, false), (Source::Push, true)] {
            let installing = updates.begin_install(version("1.1.0"), 10).unwrap();
            assert_eq!(
                installing.install(Archive {
                    archive: &archive,
                    signature: &signature,
                    sha256: &sha,
                    source,
                }),
                Err(InstallError::Failed(InstallFailure::Verification))
            );
            let facts = updates.facts();
            assert_eq!(facts.pending, None);
            assert_eq!(facts.operation, None);
            assert_eq!(facts.last_failure.is_some(), recorded, "{source:?}");
        }
        let failure = updates.facts().last_failure.unwrap();
        assert_eq!(failure.version, version("1.1.0"));
        assert_eq!(
            failure.kind,
            FailureKind::Install(InstallFailure::Verification)
        );
        assert_eq!(installations(&machine), ["1.0.0"]);
    });
}

#[test]
fn installing_reports_its_progress_and_a_broken_off_push_records_nothing() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let installing = updates.begin_install(version("1.1.0"), 100).unwrap();
        assert_eq!(updates.admits(&version("1.2.0")), Err(InstallError::Busy));
        installing.progress(40);
        assert_eq!(
            updates.facts().operation,
            Some(Operation::Installing {
                version: version("1.1.0"),
                received: 40,
                total: 100
            })
        );
        drop(installing);
        let facts = updates.facts();
        assert_eq!((facts.operation, facts.last_failure), (None, None));
        let installing = updates.begin_install(version("1.1.0"), 100).unwrap();
        installing.fail(InstallFailure::Download);
        assert_eq!(
            updates.facts().last_failure.unwrap().kind,
            FailureKind::Install(InstallFailure::Download)
        );
    });
}

#[test]
fn a_version_that_never_became_ready_is_skipped_and_a_transient_failure_stays_pending() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        let updates = Updates::open(machine.service.clone()).unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        let digest = pending_digest(&data).unwrap().unwrap();
        record_start_failure(&data, [1; 32], &digest, "1.1.0", StartFailure::PortInUse).unwrap();
        let facts = Updates::stored(&machine.service).unwrap();
        assert_eq!(facts.pending.unwrap().version, version("1.1.0"));
        assert_eq!(facts.skipped, None);
        assert_eq!(
            facts.last_failure.as_ref().map(|f| (f.id, f.kind)),
            Some((1, FailureKind::Start(StartFailure::PortInUse)))
        );
        record_start_failure(&data, [2; 32], &digest, "1.1.0", StartFailure::NotReady).unwrap();
        let updates = Updates::open(machine.service.clone()).unwrap();
        let facts = updates.facts();
        assert_eq!(facts.pending, None);
        assert_eq!(facts.skipped, Some(version("1.1.0")));
        assert_eq!(facts.last_failure.map(|f| f.id), Some(2));
        assert_eq!(
            updates.admits(&version("1.1.0")),
            Err(InstallError::Skipped)
        );
        install(
            &updates,
            &machine,
            "1.1.1",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        assert_eq!(updates.facts().skipped, None);
    });
}

#[test]
fn a_stopped_core_reports_the_facts_it_persisted() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Local,
        )
        .unwrap();
        updates.set_sessions(3);
        let check = updates.begin_check().unwrap();
        check.finish(UpdateCheck::Succeeded { at: 10 });
        let facts = Updates::stored(&machine.service).unwrap();
        assert_eq!(facts.sessions, 0);
        assert_eq!(facts.operation, None);
        assert_eq!(facts.pending, updates.facts().pending);
    });
}

#[test]
fn a_silent_restart_waits_for_zero_sessions_and_tries_each_pending_version_once_per_boot() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let admitted = Arc::new(Admitted::default());
        let restarter: Arc<dyn Restarter> = admitted.clone();
        updates
            .run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })
            .unwrap();
        updates.set_sessions(0);
        assert!(admitted.0.lock().unwrap().is_empty(), "nothing waits");
        updates.set_sessions(2);
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        updates.set_sessions(1);
        assert!(admitted.0.lock().unwrap().is_empty(), "sessions are open");
        updates.set_sessions(0);
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        let digest = digest_bytes(&pending_digest(&data).unwrap().unwrap()).unwrap();
        assert_eq!(*admitted.0.lock().unwrap(), [(Some(digest), true)]);
        assert_eq!(
            updates.facts().operation,
            Some(Operation::Restarting {
                to: version("1.1.0")
            })
        );
        // The restart into it returned here, so this boot does not try it again.
        let updates = Updates::open(machine.service.clone()).unwrap();
        updates
            .run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })
            .unwrap();
        updates.set_sessions(0);
        assert_eq!(admitted.0.lock().unwrap().len(), 1);
        assert_eq!(
            updates.restart(Restart::Asked).map(|r| r.to),
            Ok(version("1.1.0"))
        );
        assert_eq!(admitted.0.lock().unwrap()[1], (Some(digest), false));
    });
}

#[test]
fn a_breaking_pending_version_never_restarts_silently_and_names_stranded_devices() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let admitted = Arc::new(Admitted::default());
        let restarter: Arc<dyn Restarter> = admitted.clone();
        let device = StrandedDevice {
            pairing: [7; 32],
            name: "old-mini".into(),
        };
        let stranded = device.clone();
        let next = Protocols::CURRENT.work.major + 1;
        updates
            .run(
                Arc::downgrade(&restarter),
                Arc::new(move |major| {
                    if major == next {
                        vec![stranded.clone()]
                    } else {
                        Vec::new()
                    }
                }),
                || None,
            )
            .unwrap();
        updates.set_sessions(1);
        let breaking = with_major(next, Protocols::CURRENT.service.major);
        let pending = install(&updates, &machine, "2.0.0", breaking, Source::Push).unwrap();
        assert_eq!(pending.stranded, [device]);
        updates.set_sessions(0);
        assert_eq!(
            updates.restart(Restart::Silent),
            Err(RestartRefusal::Breaking)
        );
        assert!(admitted.0.lock().unwrap().is_empty());
        assert!(updates.restart(Restart::Asked).is_ok());
        assert_eq!(admitted.0.lock().unwrap().len(), 1);
    });
}

#[test]
fn waiting_for_facts_returns_once_they_change() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let first = updates.facts();
        assert_eq!(first.revision, 1);
        assert_eq!(updates.wait(first.revision, Duration::ZERO), first);
        let waiting = updates.clone();
        let waiter = std::thread::spawn(move || waiting.wait(1, Duration::from_secs(20)));
        std::thread::sleep(Duration::from_millis(50));
        updates.set_sessions(4);
        let changed = waiter.join().unwrap();
        assert_eq!((changed.revision, changed.sessions), (2, 4));
    });
}

/// Feed checks against this engine: the check owns the update operation and hands it to its
/// own install, and never takes or ends another install's.
mod feed_checks {
    use super::*;
    use crate::update::feed::{Feed, FeedArchive, FeedTarget, SystemVersion};
    use crate::update::fetch::{FeedSource, FetchError, Signed};
    use crate::update::sources::{FeedCheck, Outcome, SourceError};
    use crate::update::trust::testing::{TestKey, trust};

    /// A release host serving one feed and its archive, or unreachable without a feed.
    struct Host {
        feed: Option<Signed>,
        archive: Vec<u8>,
    }

    impl FeedSource for Host {
        fn feed(&self, _: FeedTarget) -> Result<Signed, FetchError> {
            self.feed.clone().ok_or(FetchError::Network)
        }
        fn archive(
            &self,
            _: FeedTarget,
            _: &Version,
            size: u64,
            into: &mut dyn std::io::Write,
        ) -> Result<(), FetchError> {
            crate::update::fetch::copy_exactly(&mut self.archive.as_slice(), into, size)
        }
    }

    /// A feed check of `machine` against a release host for the archive of `text`, signed with
    /// fixture trust.
    fn feed_check(machine: &Machine, text: &str, reachable: bool) -> FeedCheck {
        let key = TestKey::generate();
        let (archive, signature, _) = archive(machine, text, Protocols::CURRENT);
        let bytes = std::fs::read(archive).unwrap();
        let target = FeedTarget::core(crate::update::system::PLATFORM);
        let feed = Feed {
            target,
            version: version(text),
            published_at: now() - 60,
            protocols: Protocols::CURRENT,
            minimum_system: SystemVersion { major: 0, minor: 0 },
            archive: FeedArchive {
                name: target.archive(&version(text)),
                size: bytes.len() as u64,
                sha256: ring::digest::digest(&ring::digest::SHA256, &bytes)
                    .as_ref()
                    .try_into()
                    .unwrap(),
                signature: std::fs::read(signature).unwrap().try_into().unwrap(),
            },
        }
        .to_bytes();
        let host = Host {
            feed: reachable.then(|| Signed {
                signature: key.sign(&feed),
                feed,
            }),
            archive: bytes,
        };
        FeedCheck::fixture(
            machine.fixture.paths.data.clone(),
            Some(trust(&key, &TestKey::generate())),
            Arc::new(host),
        )
    }

    #[test]
    fn a_feed_check_installs_the_newer_release_it_finds() {
        with_fixture_trust(|| {
            let machine = machine("1.0.0");
            let updates = Updates::open(machine.service.clone()).unwrap();
            let outcome = feed_check(&machine, "1.1.0", true).run(&updates).unwrap();
            let Outcome::Installed(pending) = outcome else {
                panic!("expected an install, got {outcome:?}");
            };
            assert_eq!(pending.version, version("1.1.0"));
            let facts = updates.facts();
            assert_eq!(
                facts.pending.map(|pending| pending.version),
                Some(version("1.1.0"))
            );
            assert_eq!(facts.operation, None);
            assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
            assert_eq!(
                feed_check(&machine, "1.1.0", true).run(&updates),
                Ok(Outcome::Current),
                "the installed release is current"
            );
            assert_eq!(updates.facts().operation, None);
        });
    }

    #[test]
    fn a_feed_check_neither_takes_nor_ends_another_install() {
        with_fixture_trust(|| {
            let machine = machine("1.0.0");
            let updates = Updates::open(machine.service.clone()).unwrap();
            let installing = updates.begin_install(version("1.5.0"), 10).unwrap();
            let pushing = Some(Operation::Installing {
                version: version("1.5.0"),
                received: 0,
                total: 10,
            });
            for reachable in [false, true] {
                assert_eq!(
                    feed_check(&machine, "1.1.0", reachable).run(&updates),
                    Err(SourceError::Install(InstallError::Busy))
                );
                assert_eq!(updates.facts().operation, pushing);
            }
            assert_eq!(
                updates.begin_install(version("1.6.0"), 10).err(),
                Some(InstallError::Busy)
            );
            drop(installing);
            let checking = updates.begin_check().unwrap();
            assert_eq!(updates.begin_check().err(), Some(InstallError::Busy));
            assert_eq!(
                updates.begin_install(version("1.6.0"), 10).err(),
                Some(InstallError::Busy)
            );
            assert_eq!(updates.facts().operation, Some(Operation::Checking));
            drop(checking);
            assert_eq!(updates.facts().operation, None);
        });
    }
}

#[test]
fn a_second_engine_refuses_an_older_release_than_one_another_engine_installed() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let first = Updates::open(machine.service.clone()).unwrap();
        let second = Updates::open(machine.service.clone()).unwrap();
        install(&first, &machine, "1.2.0", Protocols::CURRENT, Source::Local).unwrap();
        assert_eq!(
            install(
                &second,
                &machine,
                "1.1.0",
                Protocols::CURRENT,
                Source::Local
            ),
            Err(InstallError::NotNewer)
        );
        assert_eq!(installations(&machine), ["1.0.0", "1.2.0"]);
    });
}

#[test]
fn a_full_disk_at_start_keeps_the_version_pending() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        let updates = Updates::open(machine.service.clone()).unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        let digest = pending_digest(&data).unwrap().unwrap();
        record_start_failure(&data, [3; 32], &digest, "1.1.0", StartFailure::DiskFull).unwrap();
        let facts = Updates::stored(&machine.service).unwrap();
        assert_eq!(facts.pending.map(|p| p.version), Some(version("1.1.0")));
        assert_eq!(facts.skipped, None);
        assert_eq!(
            facts.last_failure.map(|f| f.kind),
            Some(FailureKind::Start(StartFailure::DiskFull))
        );
    });
}
