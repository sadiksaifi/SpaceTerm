use super::*;
use crate::protocol::version::ProtocolVersion;
use crate::service::runtime::test_support::{
    no_floor, with_artifact, with_build, with_fixture_trust, with_floor,
};
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
    ) -> Result<Launch, RestartRefusal> {
        self.0.lock().unwrap().push((candidate, silent));
        Ok(Box::new(|| {}))
    }
}

struct Machine {
    fixture: Fixture,
    service: Service,
}

const IDENTITY: &str = "spaceterm-development";

/// `text` with a floor every version meets.
fn candidate(text: &str) -> Candidate {
    Candidate::new(version(text), no_floor(text)).unwrap()
}

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

/// The server reports `sessions` live Terminal Sessions and no session request.
fn sessions(sessions: u32) -> Activity {
    Activity {
        sessions,
        requested: false,
    }
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
                updates.admits(&candidate(older)),
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
        assert_eq!(updates.admits(&candidate("1.10.0")), Ok(()));
        assert_eq!(installations(&machine), ["1.0.0", "1.2.0"]);
    });
}

/// A signed archive of `text` whose build document names `floor`.
fn floored(machine: &Machine, text: &str, floor: &str) -> (PathBuf, PathBuf, String) {
    let directory = machine.fixture.root.join(format!("floored-{text}-{floor}"));
    let floor = Floor::parse(floor).unwrap();
    with_floor(text, Some(floor), Protocols::CURRENT, || {
        with_artifact(&directory, IDENTITY, |archive, signature| {
            let sha = super::super::digest(&std::fs::read(archive).unwrap());
            (archive.to_owned(), signature.to_owned(), sha)
        })
    })
}

#[test]
fn every_source_refuses_a_release_whose_floor_is_above_the_running_version() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let floor = Floor::parse("1.1.0").unwrap();
        let below = Err(InstallError::BelowFloor { floor });
        // A local archive names its floor in its signed build document.
        let (archive, signature, sha) = floored(&machine, "1.2.2", "1.1.0");
        let local = Archive {
            archive: &archive,
            signature: &signature,
            sha256: &sha,
            source: Source::Local,
        };
        assert_eq!(updates.install(local).map(|_| ()), below);
        // The feed and a push declare it before any byte arrives.
        let declared = Candidate::new(version("1.2.2"), Some(floor)).unwrap();
        assert_eq!(updates.admits(&declared), below);
        assert_eq!(
            updates.begin_install(declared.clone(), 10).map(|_| ()),
            below
        );
        // The floor applies to the running version, whatever waits as the Pending Version.
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Local,
        )
        .unwrap();
        assert_eq!(updates.admits(&declared), below);
        assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
        assert_eq!(updates.facts().last_failure, None);
    });
}

#[test]
fn an_archive_whose_build_document_names_another_floor_than_declared_fails_verification() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let (archive, signature, sha) = floored(&machine, "1.2.2", "1.1.0");
        let installing = updates.begin_install(candidate("1.2.2"), 10).unwrap();
        assert_eq!(
            installing
                .install(Archive {
                    archive: &archive,
                    signature: &signature,
                    sha256: &sha,
                    source: Source::Push,
                })
                .map(|_| ()),
            Err(InstallError::Failed(InstallFailure::Verification))
        );
        assert_eq!(installations(&machine), ["1.0.0"]);
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
        let first = updates.facts().pending.unwrap();
        // A crash left an unfinished unpacking behind.
        let unfinished = machine
            .service
            .installations_directory()
            .join(format!(".prepare-{}", "ab".repeat(16)));
        std::fs::create_dir_all(unfinished.join("bin")).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        install(
            &updates,
            &machine,
            "1.2.0",
            Protocols::CURRENT,
            Source::Push,
        )
        .unwrap();
        assert_eq!(installations(&machine), ["1.0.0", "1.2.0"]);
        assert!(!unfinished.exists());
        let pending = updates.facts().pending.unwrap();
        assert_eq!(pending.version, version("1.2.0"));
        // It waits as long as the version it replaced, so it is Overdue as soon.
        assert_eq!(pending.installed_at, first.installed_at);
    });
}

#[test]
fn a_starting_core_removes_what_a_crash_or_a_finished_update_left_behind() {
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
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        // A waiting Pending Version stays.
        updates.remove_leftovers();
        assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
        // Once no record names it, as after a skip, it is obsolete. A crash left a staged
        // download and an unfinished unpacking.
        change(&data, |record| record.pending = None).unwrap();
        let staging = machine.fixture.paths.data.join("staging");
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&staging)
            .unwrap();
        // Its writer took the owner lock, which no process holds now.
        for name in ["tar.gz", "lock"] {
            std::os::unix::fs::OpenOptionsExt::mode(
                std::fs::OpenOptions::new().write(true).create_new(true),
                0o600,
            )
            .open(staging.join(format!("{}.{name}", "ab".repeat(16))))
            .unwrap();
        }
        let unfinished = machine
            .service
            .installations_directory()
            .join(format!(".prepare-{}", "cd".repeat(16)));
        std::fs::create_dir_all(&unfinished).unwrap();
        updates.remove_leftovers();
        assert_eq!(installations(&machine), ["1.0.0"]);
        assert!(!unfinished.exists());
        assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    });
}

#[test]
fn a_starting_core_removes_the_previous_installation_once_its_activation_releases_the_lock() {
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
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        change(&data, |record| record.pending = None).unwrap();
        // The previous release's helper holds the service lock until its activation settles,
        // and removes nothing itself.
        let helper = data.lock("service.lock", true).unwrap();
        updates.remove_leftovers();
        updates.observe(sessions(1), Instant::now());
        assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
        drop(helper);
        updates.observe(sessions(1), Instant::now());
        assert_eq!(installations(&machine), ["1.0.0"]);
    });
}

/// The update record as SpaceTerm 0.11.2 reads it. A desktop, CLI, helper or Core of that
/// release reads this file while this release runs, so every value in it must be one these
/// types decode.
mod released {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    pub(crate) struct Record {
        pending: Option<Stored>,
        skipped: Option<String>,
        last_failure: Option<UpdateFailure>,
        failures: u64,
        check: UpdateCheck,
        silent: Option<Attempt>,
        activation: Option<[u8; 32]>,
        stepping_stone: Option<String>,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Stored {
        digest: String,
        version: String,
        protocols: serde_json::Value,
        installed_at: u64,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Attempt {
        boot: String,
        digest: String,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    #[allow(dead_code)]
    enum UpdateCheck {
        Disabled,
        Never,
        Succeeded {
            at: u64,
        },
        Failing {
            since: u64,
            last_success: Option<u64>,
            reason: CheckFailure,
        },
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum CheckFailure {
        Network,
        Proxy,
        Verification,
        SystemTooOld,
    }
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct UpdateFailure {
        id: u64,
        at: u64,
        version: String,
        kind: FailureKind,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum FailureKind {
        Install(InstallFailure),
        Start(StartFailure),
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum InstallFailure {
        DiskFull,
        Download,
        Verification,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum StartFailure {
        NotReady,
        PortInUse,
        DiskFull,
    }
}

/// Asserts that SpaceTerm 0.11.2 reads the update record in the Core data root `data`.
pub(crate) fn readable_by_0_11_2(data: &Path) {
    let bytes = std::fs::read(data.join("updates.json")).unwrap();
    serde_json::from_slice::<released::Record>(&bytes).expect("0.11.2 reads the update record");
}

#[test]
fn check_reasons_0_11_2_lacks_stay_out_of_the_values_it_reads() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let data = machine.fixture.paths.data.as_path();
        let record = Directory::open(data, false).unwrap();
        for reason in [
            CheckFailure::ClockBehind,
            CheckFailure::SteppingStoneSkipped,
            CheckFailure::Network,
        ] {
            let failing = UpdateCheck::Failing {
                since: 10,
                last_success: Some(5),
                reason,
            };
            updates.begin_check().unwrap().finish(failing.clone());
            readable_by_0_11_2(data);
            // This runtime reads the reason back. A Release Core reports it in its facts.
            assert_eq!(load(&record).unwrap().check(), failing);
        }
        updates
            .begin_check()
            .unwrap()
            .finish(UpdateCheck::Succeeded { at: 20 });
        assert_eq!(
            load(&record).unwrap().check(),
            UpdateCheck::Succeeded { at: 20 }
        );
        readable_by_0_11_2(data);
    });
}

#[test]
fn unowned_staged_downloads_go_once_no_earlier_release_can_run() {
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
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        // The other installation stands for one of an earlier release, whose CLI could still
        // be downloading.
        change(&data, |record| record.pending = None).unwrap();
        let staging = machine.fixture.paths.data.join("staging");
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&staging)
            .unwrap();
        // Crashes of such a CLI left several downloads, none with an owner lock.
        let unowned: Vec<_> = ["ab", "cd", "ef"]
            .map(|digits| staging.join(format!("{}.tar.gz", digits.repeat(16))))
            .into();
        for path in &unowned {
            std::fs::write(path, b"partial").unwrap();
        }
        // The activation that started this server still holds the service lock.
        let helper = data.lock("service.lock", true).unwrap();
        updates.remove_leftovers();
        assert!(unowned.iter().all(|path| path.exists()));
        assert_eq!(installations(&machine), ["1.0.0", "1.1.0"]);
        drop(helper);
        updates.observe(sessions(1), Instant::now());
        assert_eq!(installations(&machine), ["1.0.0"]);
        assert!(unowned.iter().all(|path| !path.exists()));
    });
}

#[test]
fn an_overdue_pending_version_keeps_the_overdue_value_every_peer_accepts() {
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
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        let waited = |seconds: u64| {
            let record = change(&data, |record| {
                if let Some(pending) = &mut record.pending {
                    pending.installed_at = now() - seconds;
                }
                record.clone()
            })
            .unwrap();
            updates.shared.lock().record = record;
            updates.facts()
        };
        let one_day = waited(OVERDUE_SINCE_AFTER + 60);
        assert_eq!(
            one_day.pending.unwrap().overdue_since,
            None,
            "not yet overdue"
        );
        let two_days = waited(OVERDUE_AFTER + 60);
        assert_eq!(two_days.validate(), Ok(()));
        let pending = two_days.clone().pending.unwrap();
        assert_eq!(
            pending.overdue_since,
            Some(pending.installed_at + OVERDUE_SINCE_AFTER)
        );
        // A desktop of 0.11.2 checks the same value at work 1.0.
        let older = ProtocolVersion { major: 1, minor: 0 };
        assert_eq!(two_days.for_selected(older).validate(), Ok(()));
    });
}

#[test]
fn a_replacement_that_installs_on_retry_settles_its_install_failure() {
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
        // Installing the replacement fails once, then succeeds.
        updates
            .begin_install(candidate("1.2.0"), 10)
            .unwrap()
            .fail(InstallFailure::DiskFull);
        assert!(updates.facts().last_failure.is_some());
        let first = updates.facts().pending.unwrap().installed_at;
        install(
            &updates,
            &machine,
            "1.2.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        let facts = updates.facts();
        assert_eq!(facts.last_failure, None, "the install settled its failure");
        let pending = facts.pending.unwrap();
        assert_eq!(pending.version, version("1.2.0"));
        assert_eq!(
            pending.installed_at, first,
            "it waits as long as the one it replaced"
        );
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
            let installing = updates.begin_install(candidate("1.1.0"), 10).unwrap();
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
        let installing = updates.begin_install(candidate("1.1.0"), 100).unwrap();
        assert_eq!(updates.admits(&candidate("1.2.0")), Err(InstallError::Busy));
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
        let installing = updates.begin_install(candidate("1.1.0"), 100).unwrap();
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
            updates.admits(&candidate("1.1.0")),
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
fn a_core_restarted_into_a_stepping_stone_reports_it_until_it_checks_its_feed() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let data = Directory::open(&machine.fixture.paths.data, false).unwrap();
        let updates = Updates::open(machine.service.clone()).unwrap();
        let (archive, signature, sha) = archive(&machine, "1.1.0", Protocols::CURRENT);
        let checking = updates.begin_check().unwrap();
        checking
            .begin_install(candidate("1.1.0"), 0, true)
            .unwrap()
            .install(Archive {
                archive: &archive,
                signature: &signature,
                sha256: &sha,
                source: Source::Feed,
            })
            .unwrap();
        assert!(!updates.stepping_stone(), "the Core still runs 1.0.0");
        updates
            .begin_check()
            .unwrap()
            .finish(UpdateCheck::Succeeded { at: 10 });
        // The restart into the Pending Version completes.
        let digest = pending_digest(&data).unwrap().unwrap();
        record_completed(&data, &digest).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.installation = digest;
        data.write_json(c"service.json", &state).unwrap();
        let restarted = Updates::open(machine.service.clone()).unwrap();
        assert_eq!(restarted.facts().running, version("1.1.0"));
        assert!(
            restarted.stepping_stone(),
            "a check before the restart keeps the Stepping Stone"
        );
        restarted
            .begin_check()
            .unwrap()
            .finish(UpdateCheck::Succeeded { at: 20 });
        assert!(!restarted.stepping_stone());
        assert!(
            !Updates::open(machine.service.clone())
                .unwrap()
                .stepping_stone()
        );
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
        updates.observe(sessions(3), Instant::now());
        let check = updates.begin_check().unwrap();
        check.finish(UpdateCheck::Succeeded { at: 10 });
        let facts = Updates::stored(&machine.service).unwrap();
        assert_eq!(facts.sessions, 0);
        assert_eq!(facts.operation, None);
        assert_eq!(facts.pending, updates.facts().pending);
    });
}

#[test]
fn a_silent_restart_waits_for_a_quiet_grace_period_and_tries_each_pending_version_once_per_boot() {
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
        let start = Instant::now();
        updates.observe(sessions(0), start);
        updates.observe(sessions(0), start + SILENT_GRACE);
        assert!(admitted.0.lock().unwrap().is_empty(), "nothing waits");
        updates.observe(sessions(2), start);
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        updates.observe(sessions(1), start);
        assert!(admitted.0.lock().unwrap().is_empty(), "sessions are open");
        // The last session ended. The grace period runs from here.
        updates.observe(sessions(0), start);
        updates.observe(sessions(0), start + SILENT_GRACE / 2);
        assert!(
            admitted.0.lock().unwrap().is_empty(),
            "the grace period runs"
        );
        // A session request starts it again, even when no session started.
        let requested = Activity {
            sessions: 0,
            requested: true,
        };
        updates.observe(requested, start + SILENT_GRACE - Duration::from_secs(1));
        updates.observe(sessions(0), start + SILENT_GRACE);
        assert!(
            admitted.0.lock().unwrap().is_empty(),
            "the request started the grace period again"
        );
        assert_eq!(updates.facts().operation, None, "no restart shows yet");
        updates.observe(
            sessions(0),
            start + SILENT_GRACE * 2 - Duration::from_secs(1),
        );
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
        updates.observe(sessions(0), start);
        updates.observe(sessions(0), start + SILENT_GRACE);
        assert_eq!(admitted.0.lock().unwrap().len(), 1);
        assert_eq!(
            updates.restart(Restart::Asked).map(|r| r.to),
            Ok(version("1.1.0"))
        );
        assert_eq!(admitted.0.lock().unwrap()[1], (Some(digest), false));
    });
}

/// The fence refuses every restart its admission finds sessions for.
struct Refusing;
impl Restarter for Refusing {
    fn restart(self: Arc<Self>, _: Option<[u8; 32]>, _: bool) -> Result<Launch, RestartRefusal> {
        Err(RestartRefusal::SessionsOpen)
    }
}

/// A helper that stops the server the moment it launches: it reads the facts every follower
/// heard by then.
#[derive(Default)]
struct StopsAtOnce {
    updates: OnceLock<Updates>,
    heard: Mutex<Option<Option<Operation>>>,
}
impl Restarter for StopsAtOnce {
    fn restart(self: Arc<Self>, _: Option<[u8; 32]>, _: bool) -> Result<Launch, RestartRefusal> {
        Ok(Box::new(move || {
            let updates = self.updates.get().unwrap();
            *self.heard.lock().unwrap() = Some(updates.facts().operation);
            updates.stop();
        }))
    }
}

#[test]
fn a_silent_restart_shows_before_its_helper_can_stop_the_server() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let helper = Arc::new(StopsAtOnce::default());
        let _ = helper.updates.set(updates.clone());
        let restarter: Arc<dyn Restarter> = helper.clone();
        updates
            .run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })
            .unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        let start = Instant::now();
        updates.observe(sessions(0), start);
        updates.observe(sessions(0), start + SILENT_GRACE);
        assert_eq!(
            *helper.heard.lock().unwrap(),
            Some(Some(Operation::Restarting {
                to: version("1.1.0")
            }))
        );
    });
}

#[test]
fn a_restart_the_fence_refuses_never_shows_in_the_facts() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let restarter: Arc<dyn Restarter> = Arc::new(Refusing);
        updates
            .run(Arc::downgrade(&restarter), Arc::new(|_| Vec::new()), || {
                None
            })
            .unwrap();
        install(
            &updates,
            &machine,
            "1.1.0",
            Protocols::CURRENT,
            Source::Feed,
        )
        .unwrap();
        let before = updates.facts();
        let waiting = updates.clone();
        let waiter =
            std::thread::spawn(move || waiting.wait(before.revision, Duration::from_millis(500)));
        let start = Instant::now();
        updates.observe(sessions(0), start);
        updates.observe(sessions(0), start + SILENT_GRACE);
        let seen = waiter.join().unwrap().unwrap();
        assert_eq!(seen.operation, None, "a refused restart never shows");
        assert_eq!(updates.facts().operation, None);
    });
}

#[test]
fn a_restart_without_a_pending_version_shows_that_the_version_stays() {
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
        assert_eq!(
            updates.restart(Restart::Asked).map(|r| r.to),
            Ok(version("1.0.0"))
        );
        let facts = updates.facts();
        assert_eq!(facts.operation, Some(Operation::RestartingSameVersion));
        // A desktop whose admission selected work 1.0 reads the restart it knows.
        let older = ProtocolVersion { major: 1, minor: 0 };
        assert_eq!(
            facts.for_selected(older).operation,
            Some(Operation::Restarting {
                to: version("1.0.0")
            })
        );
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
        updates.observe(sessions(1), Instant::now());
        let breaking = with_major(next, Protocols::CURRENT.service.major);
        let pending = install(&updates, &machine, "2.0.0", breaking, Source::Push).unwrap();
        assert_eq!(pending.stranded, [device]);
        updates.observe(sessions(0), Instant::now());
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
        assert_eq!(updates.wait(first.revision, Duration::ZERO), Some(first));
        let waiting = updates.clone();
        let waiter = std::thread::spawn(move || waiting.wait(1, Duration::from_secs(20)));
        std::thread::sleep(Duration::from_millis(50));
        updates.observe(sessions(4), Instant::now());
        let changed = waiter.join().unwrap().unwrap();
        assert_eq!((changed.revision, changed.sessions), (2, 4));
    });
}

#[test]
fn stopping_the_engine_ends_every_wait_for_facts() {
    with_fixture_trust(|| {
        let machine = machine("1.0.0");
        let updates = Updates::open(machine.service.clone()).unwrap();
        let first = updates.facts();
        let waiting = updates.clone();
        let waiter = std::thread::spawn(move || {
            let facts = waiting.wait(first.revision, Duration::from_secs(20));
            (facts, Instant::now())
        });
        std::thread::sleep(Duration::from_millis(50));
        let stopped = Instant::now();
        updates.stop();
        let (facts, returned) = waiter.join().unwrap();
        assert_eq!(facts, None);
        assert!(returned.duration_since(stopped) < Duration::from_millis(100));
        // A wait that begins after the stop returns at once.
        let late = Instant::now();
        assert_eq!(updates.wait(first.revision, Duration::from_secs(20)), None);
        assert!(late.elapsed() < Duration::from_millis(100));
        assert_eq!(updates.facts().revision, first.revision);
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
        fn feed(&self, _: FeedTarget, _: Option<&Version>) -> Result<Signed, FetchError> {
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
            floor: Floor::parse("0.0.0").unwrap(),
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
            let installing = updates.begin_install(candidate("1.5.0"), 10).unwrap();
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
                updates.begin_install(candidate("1.6.0"), 10).err(),
                Some(InstallError::Busy)
            );
            drop(installing);
            let checking = updates.begin_check().unwrap();
            assert_eq!(updates.begin_check().err(), Some(InstallError::Busy));
            assert_eq!(
                updates.begin_install(candidate("1.6.0"), 10).err(),
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
