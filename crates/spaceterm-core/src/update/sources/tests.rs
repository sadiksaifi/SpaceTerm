use super::*;
use crate::protocol::version::ProtocolVersion;
use crate::update::facts::{Platform, Protocols, Version};
use crate::update::feed::{Feed, FeedArchive};
use crate::update::fetch::Signed;
use crate::update::ordering::{Candidate, Floor};
use crate::update::trust::testing::{TestKey, trust};

const NOW: u64 = 1_793_750_400;
const HOST: SystemVersion = SystemVersion {
    major: 2,
    minor: 39,
};

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn floor(text: &str) -> Floor {
    Floor::parse(text).unwrap()
}

/// `text` with a floor every version meets.
fn candidate(text: &str) -> Candidate {
    Candidate::new(version(text), Some(floor("0.0.0"))).unwrap()
}

/// What the fake engine saw, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    BeginCheck,
    Finish(UpdateCheck),
    /// A check ended without a finished state.
    Unfinished,
    BeginInstall(Version, u64),
    Progress(u64),
    Install {
        source: Source,
        bytes: Vec<u8>,
        signature: Vec<u8>,
        sha256: String,
    },
    Fail(InstallFailure),
    /// An install ended without installing or failing.
    Dropped,
}

#[derive(Default)]
struct State {
    installed: Vec<Version>,
    skipped: Option<Version>,
    check: Option<UpdateCheck>,
    /// The engine's own verification refuses the next install.
    refuse: Option<InstallError>,
    /// Another install, check or restart owns the update operation.
    busy: bool,
    events: Vec<Event>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<State>>);

impl Fake {
    fn with(installed: &[&str]) -> Self {
        let fake = Self::default();
        fake.state().installed = installed.iter().map(|v| version(v)).collect();
        fake
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn events(&self) -> Vec<Event> {
        self.state()
            .events
            .iter()
            .filter(|event| !matches!(event, Event::Progress(_)))
            .cloned()
            .collect()
    }

    fn progress(&self) -> Vec<u64> {
        self.state()
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Progress(received) => Some(*received),
                _ => None,
            })
            .collect()
    }

    fn record(&self, event: Event) {
        self.state().events.push(event);
    }

    fn pending(version: Version) -> PendingVersion {
        PendingVersion {
            version,
            protocols: Protocols::CURRENT,
            breaking: false,
            installed_at: NOW,
            overdue_since: None,
            stranded: Vec::new(),
        }
    }
}

struct FakeInstalling {
    fake: Fake,
    version: Version,
    ended: bool,
}

struct FakeChecking {
    fake: Fake,
    finished: std::cell::Cell<bool>,
    /// The check handed its operation to an install.
    handed_over: bool,
}

impl Installing for FakeInstalling {
    fn progress(&self, received: u64) {
        self.fake.record(Event::Progress(received));
    }
    fn install(mut self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        self.ended = true;
        let fake = self.fake.clone();
        fake.record(Event::Install {
            source: archive.source,
            bytes: std::fs::read(archive.archive).unwrap(),
            signature: std::fs::read(archive.signature).unwrap(),
            sha256: archive.sha256.to_owned(),
        });
        if let Some(refused) = fake.state().refuse.take() {
            return Err(refused);
        }
        fake.state().installed.push(self.version.clone());
        Ok(Fake::pending(self.version.clone()))
    }
    fn fail(mut self, failure: InstallFailure) {
        self.ended = true;
        self.fake.record(Event::Fail(failure));
    }
}

impl Drop for FakeInstalling {
    fn drop(&mut self) {
        if !self.ended {
            self.fake.record(Event::Dropped);
        }
        self.fake.state().busy = false;
    }
}

impl Checking for FakeChecking {
    type Installing = FakeInstalling;
    fn admits(&self, candidate: &Candidate) -> Result<(), InstallError> {
        self.fake.ordered(candidate)
    }
    fn finish(&self, check: UpdateCheck) {
        self.finished.set(true);
        self.fake.state().check = Some(check.clone());
        self.fake.record(Event::Finish(check));
    }
    fn begin_install(
        mut self,
        candidate: Candidate,
        total: u64,
    ) -> Result<FakeInstalling, InstallError> {
        self.fake.ordered(&candidate)?;
        let version = candidate.version().clone();
        self.handed_over = true;
        self.fake
            .record(Event::BeginInstall(version.clone(), total));
        Ok(FakeInstalling {
            fake: self.fake.clone(),
            version,
            ended: false,
        })
    }
}

impl Drop for FakeChecking {
    fn drop(&mut self) {
        if !self.finished.get() {
            self.fake.record(Event::Unfinished);
        }
        if !self.handed_over {
            self.fake.state().busy = false;
        }
    }
}

impl Fake {
    fn ordered(&self, candidate: &Candidate) -> Result<(), InstallError> {
        let state = self.state();
        super::super::ordering::admit(
            candidate,
            &state.installed[0],
            state.installed.iter().skip(1).last(),
            state.skipped.as_ref(),
        )
        .map_err(|refusal| match refusal {
            super::super::ordering::Refusal::NotNewer => InstallError::NotNewer,
            super::super::ordering::Refusal::Skipped => InstallError::Skipped,
            super::super::ordering::Refusal::BelowFloor { floor } => {
                InstallError::BelowFloor { floor }
            }
        })
    }

    /// Takes the update operation, as the engine does, or refuses while another owns it.
    fn take(&self) -> Result<(), InstallError> {
        let mut state = self.state();
        if state.busy {
            return Err(InstallError::Busy);
        }
        state.busy = true;
        Ok(())
    }
}

impl Engine for Fake {
    type Installing = FakeInstalling;
    type Checking<'a> = FakeChecking;

    fn facts(&self) -> UpdateFacts {
        let state = self.state();
        UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: state.installed[0].clone(),
            pending: None,
            sessions: 0,
            operation: None,
            check: state.check.clone().unwrap_or(UpdateCheck::Never),
            skipped: state.skipped.clone(),
            last_failure: None,
        }
    }

    fn begin_install(
        &self,
        candidate: Candidate,
        total: u64,
    ) -> Result<FakeInstalling, InstallError> {
        if self.state().busy {
            return Err(InstallError::Busy);
        }
        self.ordered(&candidate)?;
        let version = candidate.version().clone();
        self.take()?;
        self.record(Event::BeginInstall(version.clone(), total));
        Ok(FakeInstalling {
            fake: self.clone(),
            version,
            ended: false,
        })
    }

    fn install(&self, _: Archive<'_>) -> Result<PendingVersion, InstallError> {
        unreachable!("local archives install through the engine directly")
    }

    fn begin_check(&self) -> Result<FakeChecking, InstallError> {
        self.take()?;
        self.record(Event::BeginCheck);
        Ok(FakeChecking {
            fake: self.clone(),
            finished: std::cell::Cell::new(false),
            handed_over: false,
        })
    }
}

/// A private data directory, removed when the test ends.
struct Data(PathBuf);

impl Data {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "spaceterm-sources-{:x}",
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        Self(path)
    }

    /// The staging files left behind.
    fn staged(&self) -> usize {
        std::fs::read_dir(self.0.join("staging")).map_or(0, |entries| entries.count())
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A signed release: its feed, the feed's signature and the archive the feed names.
#[derive(Clone)]
struct Release {
    feed: Vec<u8>,
    signature: Vec<u8>,
    archive: Vec<u8>,
}

fn release(key: &TestKey, version: &str, minimum: SystemVersion, archive: &[u8]) -> Release {
    let version = Version::parse(version).unwrap();
    let feed = Feed {
        target: FeedTarget::CORE_LINUX,
        archive: FeedArchive {
            name: FeedTarget::CORE_LINUX.archive(&version),
            size: archive.len() as u64,
            sha256: ring::digest::digest(&ring::digest::SHA256, archive)
                .as_ref()
                .try_into()
                .unwrap(),
            signature: key.sign(archive).try_into().unwrap(),
        },
        version,
        floor: Floor::parse("0.0.0").unwrap(),
        published_at: NOW,
        protocols: Protocols {
            work: ProtocolVersion { major: 1, minor: 0 },
            service: ProtocolVersion { major: 1, minor: 0 },
            desktop_channel: ProtocolVersion { major: 1, minor: 0 },
        },
        minimum_system: minimum,
    }
    .to_bytes();
    Release {
        signature: key.sign(&feed),
        feed,
        archive: archive.to_vec(),
    }
}

/// The release host: answers with `feed` or a failure, and serves `archive` bytes, or fails.
#[derive(Clone)]
struct Host {
    feed: Result<Signed, FetchError>,
    archive: Result<Vec<u8>, FetchError>,
    downloads: Arc<Mutex<u32>>,
}

impl Host {
    fn serving(release: &Release) -> Self {
        Self {
            feed: Ok(Signed {
                feed: release.feed.clone(),
                signature: release.signature.clone(),
            }),
            archive: Ok(release.archive.clone()),
            downloads: Arc::default(),
        }
    }

    fn failing(error: FetchError) -> Self {
        Self {
            feed: Err(error),
            archive: Err(error),
            downloads: Arc::default(),
        }
    }

    fn downloads(&self) -> u32 {
        *self.downloads.lock().unwrap()
    }
}

impl FeedSource for Host {
    fn feed(&self, target: FeedTarget) -> Result<Signed, FetchError> {
        assert_eq!(target, FeedTarget::CORE_LINUX);
        self.feed.clone()
    }
    fn archive(
        &self,
        _: FeedTarget,
        _: &Version,
        size: u64,
        into: &mut dyn Write,
    ) -> Result<(), FetchError> {
        *self.downloads.lock().unwrap() += 1;
        let bytes = self.archive.clone()?;
        super::super::fetch::copy_exactly(&mut bytes.as_slice(), into, size)
    }
}

struct Check {
    check: FeedCheck,
    proxies: Arc<Mutex<Vec<Option<Proxy>>>>,
}

fn feed_check(
    data: &Data,
    trust: Option<UpdateTrust>,
    host: Host,
    system: fn() -> Option<SystemVersion>,
) -> Check {
    let proxies: Arc<Mutex<Vec<Option<Proxy>>>> = Arc::default();
    let seen = proxies.clone();
    Check {
        check: FeedCheck {
            target: FeedTarget::CORE_LINUX,
            trust,
            sources: Box::new(move |proxy| {
                seen.lock().unwrap().push(proxy.cloned());
                Box::new(host.clone())
            }),
            system,
            clock: || NOW,
            data: data.0.clone(),
        },
        proxies,
    }
}

fn host_system() -> Option<SystemVersion> {
    Some(HOST)
}

const OLD_GLIBC: SystemVersion = SystemVersion {
    major: 2,
    minor: 35,
};

#[test]
fn only_the_release_identity_fetches_a_feed() {
    let data = PathBuf::from("/nonexistent");
    assert!(FeedCheck::for_identity("spaceterm", data.clone()).is_some());
    assert!(FeedCheck::for_identity("spaceterm-preflight", data.clone()).is_none());
    assert!(FeedCheck::for_identity("spaceterm-development", data).is_none());
    assert_eq!(SourceError::NoFeed.code(), "no_update_feed");
}

#[test]
fn a_newer_release_is_downloaded_and_installed_from_the_feed() {
    let (data, key) = (Data::new(), TestKey::generate());
    let release = release(&key, "1.5.0", OLD_GLIBC, &[7; 300_000]);
    let host = Host::serving(&release);
    let check = feed_check(
        &data,
        Some(trust(&key, &TestKey::generate())),
        host.clone(),
        host_system,
    );
    let fake = Fake::with(&["1.4.0"]);
    let outcome = check.check.run(&fake).unwrap();
    assert_eq!(outcome, Outcome::Installed(Fake::pending(version("1.5.0"))));
    assert_eq!(
        fake.events(),
        [
            Event::BeginCheck,
            Event::Finish(UpdateCheck::Succeeded { at: NOW }),
            Event::BeginInstall(version("1.5.0"), 300_000),
            Event::Install {
                source: Source::Feed,
                bytes: release.archive.clone(),
                signature: key.sign(&release.archive),
                sha256: hex(ring::digest::digest(&ring::digest::SHA256, &release.archive).as_ref()),
            },
        ]
    );
    let progress = fake.progress();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&300_000));
    assert_eq!(*check.proxies.lock().unwrap(), [None]);
    assert_eq!(data.staged(), 0, "the staging files are removed");
}

#[test]
fn a_feed_that_names_no_newer_version_downloads_nothing() {
    let key = TestKey::generate();
    for (installed, skipped) in [
        (&["1.5.0"][..], None),
        (&["1.6.0"][..], None),
        (&["1.4.0", "1.5.0"][..], None),
        (&["1.4.0"][..], Some("1.5.0")),
    ] {
        let data = Data::new();
        let release = release(&key, "1.5.0", OLD_GLIBC, b"archive");
        let host = Host::serving(&release);
        let check = feed_check(
            &data,
            Some(trust(&key, &TestKey::generate())),
            host.clone(),
            host_system,
        );
        let fake = Fake::with(installed);
        fake.state().skipped = skipped.map(version);
        assert_eq!(check.check.run(&fake), Ok(Outcome::Current));
        assert_eq!(
            fake.events(),
            [
                Event::BeginCheck,
                Event::Finish(UpdateCheck::Succeeded { at: NOW })
            ]
        );
        assert_eq!(host.downloads(), 0);
    }
}

#[test]
fn a_failed_check_keeps_the_first_failure_time_and_the_last_success() {
    let key = TestKey::generate();
    for (host, trust_keys, reason) in [
        (
            Host::failing(FetchError::Network),
            true,
            CheckFailure::Network,
        ),
        (Host::failing(FetchError::Proxy), true, CheckFailure::Proxy),
        (
            Host::failing(FetchError::TooLarge),
            true,
            CheckFailure::Verification,
        ),
        (
            Host::serving(&release(&TestKey::generate(), "1.5.0", OLD_GLIBC, b"a")),
            true,
            CheckFailure::Verification,
        ),
        (
            Host::serving(&release(&key, "1.5.0", OLD_GLIBC, b"a")),
            false,
            CheckFailure::Verification,
        ),
    ] {
        let data = Data::new();
        let check = feed_check(
            &data,
            trust_keys.then(|| trust(&key, &TestKey::generate())),
            host.clone(),
            host_system,
        );
        let fake = Fake::with(&["1.4.0"]);
        fake.state().check = Some(UpdateCheck::Succeeded { at: NOW - 60 });
        assert_eq!(check.check.run(&fake), Err(SourceError::Check(reason)));
        assert_eq!(
            fake.events(),
            [
                Event::BeginCheck,
                Event::Finish(UpdateCheck::Failing {
                    since: NOW,
                    last_success: Some(NOW - 60),
                    reason
                })
            ],
            "{reason:?}"
        );
        assert_eq!(host.downloads(), 0);
    }
}

#[test]
fn a_release_for_a_newer_system_is_not_downloaded() {
    let (data, key) = (Data::new(), TestKey::generate());
    let release = release(
        &key,
        "1.5.0",
        SystemVersion {
            major: 2,
            minor: 40,
        },
        b"a",
    );
    let host = Host::serving(&release);
    let check = feed_check(
        &data,
        Some(trust(&key, &TestKey::generate())),
        host.clone(),
        host_system,
    );
    let fake = Fake::with(&["1.4.0"]);
    assert_eq!(
        check.check.run(&fake),
        Err(SourceError::Check(CheckFailure::SystemTooOld))
    );
    assert_eq!(
        fake.events()[1],
        Event::Finish(UpdateCheck::Failing {
            since: NOW,
            last_success: None,
            reason: CheckFailure::SystemTooOld
        })
    );
    assert_eq!(host.downloads(), 0);
    let unknown = feed_check(&data, Some(trust(&key, &TestKey::generate())), host, || {
        None
    });
    assert_eq!(
        unknown.check.run(&Fake::with(&["1.4.0"])),
        Err(SourceError::Check(CheckFailure::SystemTooOld)),
        "a host that does not name its system is not known to be new enough"
    );
}

#[test]
fn a_release_signed_by_the_backup_key_installs() {
    let (data, current, backup) = (Data::new(), TestKey::generate(), TestKey::generate());
    let release = release(&backup, "1.5.0", OLD_GLIBC, b"archive");
    let check = feed_check(
        &data,
        Some(trust(&current, &backup)),
        Host::serving(&release),
        host_system,
    );
    assert!(matches!(
        check.check.run(&Fake::with(&["1.4.0"])),
        Ok(Outcome::Installed(_))
    ));
}

#[test]
fn a_download_that_breaks_or_differs_records_an_install_failure() {
    let key = TestKey::generate();
    let good = release(&key, "1.5.0", OLD_GLIBC, &[1; 1000]);
    let mut tampered = Host::serving(&good);
    tampered.archive = Ok(vec![2; 1000]);
    let mut short = Host::serving(&good);
    short.archive = Ok(vec![1; 999]);
    let mut broken = Host::serving(&good);
    broken.archive = Err(FetchError::Network);
    let mut full = Host::serving(&good);
    full.archive = Err(FetchError::DiskFull);
    for (host, failure) in [
        (tampered, InstallFailure::Verification),
        (short, InstallFailure::Download),
        (broken, InstallFailure::Download),
        (full, InstallFailure::DiskFull),
    ] {
        let data = Data::new();
        let check = feed_check(
            &data,
            Some(trust(&key, &TestKey::generate())),
            host,
            host_system,
        );
        let fake = Fake::with(&["1.4.0"]);
        assert_eq!(
            check.check.run(&fake),
            Err(SourceError::Install(InstallError::Failed(failure)))
        );
        assert_eq!(
            fake.events().last(),
            Some(&Event::Fail(failure)),
            "{failure:?}"
        );
        assert_eq!(data.staged(), 0);
    }
}

#[test]
fn the_engines_refusal_after_download_is_reported() {
    let (data, key) = (Data::new(), TestKey::generate());
    let release = release(&key, "1.5.0", OLD_GLIBC, b"archive");
    let check = feed_check(
        &data,
        Some(trust(&key, &TestKey::generate())),
        Host::serving(&release),
        host_system,
    );
    let fake = Fake::with(&["1.4.0"]);
    fake.state().refuse = Some(InstallError::Failed(InstallFailure::Verification));
    assert_eq!(
        check.check.run(&fake).map_err(|error| error.code()),
        Err("candidate_unverified")
    );
    let fake = Fake::with(&["1.4.0"]);
    fake.state().busy = true;
    assert_eq!(
        check.check.run(&fake),
        Err(SourceError::Install(InstallError::Busy))
    );
    assert_eq!(
        fake.events(),
        [],
        "a check never starts while another operation owns the engine"
    );
    assert!(fake.state().busy, "and it leaves that operation alone");
}

#[test]
fn feed_fetches_go_through_the_proxy_set_at_that_moment() {
    let (data, key) = (Data::new(), TestKey::generate());
    let check = feed_check(
        &data,
        Some(trust(&key, &TestKey::generate())),
        Host::failing(FetchError::Proxy),
        host_system,
    );
    let fake = Fake::with(&["1.4.0"]);
    let _ = check.check.run(&fake);
    let proxy = Proxy::parse("http://proxy.example:3128").unwrap();
    super::super::proxy::write(&data.0, Some(&proxy)).unwrap();
    let _ = check.check.run(&fake);
    super::super::proxy::write(&data.0, None).unwrap();
    let _ = check.check.run(&fake);
    assert_eq!(*check.proxies.lock().unwrap(), [None, Some(proxy), None]);
}

fn digest(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

#[test]
fn a_push_installs_once_its_last_chunk_arrives() {
    let data = Data::new();
    let pushes = Pushes::new(data.0.clone());
    let fake = Fake::with(&["1.4.0"]);
    let archive = vec![9; 10];
    let id = pushes
        .begin(
            &fake,
            1,
            candidate("1.5.0"),
            10,
            &digest(&archive),
            &[3; 64],
        )
        .unwrap();
    assert_eq!(pushes.chunk(&id, 0, &archive[..4]), Ok(Received::Next(4)));
    assert_eq!(
        pushes.chunk(&id, 0, &archive[..4]),
        Err(SourceError::Protocol),
        "a chunk out of order is refused"
    );
    assert_eq!(
        pushes.chunk(&id, 4, &archive[4..]),
        Ok(Received::Installed(Fake::pending(version("1.5.0"))))
    );
    assert_eq!(
        fake.events(),
        [
            Event::BeginInstall(version("1.5.0"), 10),
            Event::Install {
                source: Source::Push,
                bytes: archive.clone(),
                signature: vec![3; 64],
                sha256: digest(&archive),
            }
        ]
    );
    assert_eq!(fake.progress().last(), Some(&10));
    assert_eq!(
        pushes.chunk(&id, 10, &[1]),
        Err(SourceError::Install(InstallError::Core(CoreError::Changed))),
        "a finished push takes no more chunks"
    );
    assert_eq!(data.staged(), 0);
}

#[test]
fn a_push_of_a_version_the_core_would_not_install_receives_nothing() {
    let data = Data::new();
    let pushes = Pushes::new(data.0.clone());
    let fake = Fake::with(&["1.4.0", "1.5.0"]);
    fake.state().skipped = Some(version("1.4.5"));
    for (pushed, code) in [
        ("1.5.0", "not_newer"),
        ("1.4.9", "not_newer"),
        ("1.4.5", "version_skipped"),
    ] {
        assert_eq!(
            pushes
                .begin(&fake, 1, candidate(pushed), 10, &"ab".repeat(32), &[3; 64])
                .map_err(|error| error.code()),
            Err(code)
        );
    }
    assert!(fake.events().is_empty());
    assert_eq!(data.staged(), 0);
}

#[test]
fn a_malformed_push_is_refused_before_the_engine_is_asked() {
    let data = Data::new();
    let pushes = Pushes::new(data.0.clone());
    let fake = Fake::with(&["1.4.0"]);
    let sha = "ab".repeat(32);
    for (length, sha256, signature) in [
        (0, sha.as_str(), &[3; 64][..]),
        (MAX_ARCHIVE + 1, sha.as_str(), &[3; 64][..]),
        (10, "AB", &[3; 64][..]),
        (10, sha.as_str(), &[3; 63][..]),
    ] {
        assert_eq!(
            pushes.begin(&fake, 1, candidate("1.5.0"), length, sha256, signature),
            Err(SourceError::Protocol)
        );
    }
    assert!(fake.events().is_empty());
}

#[test]
fn a_completed_push_whose_digest_differs_records_a_verification_failure() {
    let data = Data::new();
    let pushes = Pushes::new(data.0.clone());
    let fake = Fake::with(&["1.4.0"]);
    let id = pushes
        .begin(&fake, 1, candidate("1.5.0"), 3, &digest(b"abc"), &[3; 64])
        .unwrap();
    assert_eq!(
        pushes.chunk(&id, 0, b"abd"),
        Err(SourceError::Install(InstallError::Failed(
            InstallFailure::Verification
        )))
    );
    assert_eq!(
        fake.events().last(),
        Some(&Event::Fail(InstallFailure::Verification))
    );
    assert_eq!(data.staged(), 0);
}

#[test]
fn a_push_that_breaks_off_records_nothing() {
    let data = Data::new();
    let pushes = Pushes::new(data.0.clone());
    let fake = Fake::with(&["1.4.0"]);
    let first = pushes
        .begin(&fake, 1, candidate("1.5.0"), 3, &digest(b"abc"), &[3; 64])
        .unwrap();
    pushes.chunk(&first, 0, b"a").unwrap();
    pushes.closed(2);
    assert_eq!(
        fake.events().last(),
        Some(&Event::BeginInstall(version("1.5.0"), 3))
    );
    pushes.closed(1);
    assert_eq!(fake.events().last(), Some(&Event::Dropped));
    assert_eq!(data.staged(), 0);

    let replaced = pushes
        .begin(&fake, 3, candidate("1.5.0"), 3, &digest(b"abc"), &[3; 64])
        .unwrap();
    let second = pushes
        .begin(&fake, 4, candidate("1.5.0"), 3, &digest(b"abc"), &[3; 64])
        .unwrap();
    assert_eq!(
        fake.events().last(),
        Some(&Event::BeginInstall(version("1.5.0"), 3))
    );
    assert_eq!(
        fake.events()
            .iter()
            .filter(|event| **event == Event::Dropped)
            .count(),
        2,
        "a new push replaces the unfinished one"
    );
    assert_eq!(
        pushes.chunk(&replaced, 0, b"a"),
        Err(SourceError::Install(InstallError::Core(CoreError::Changed)))
    );
    assert!(matches!(
        pushes.chunk(&second, 0, b"abc"),
        Ok(Received::Installed(_))
    ));
    assert!(
        !fake
            .events()
            .iter()
            .any(|event| matches!(event, Event::Fail(_)))
    );
}

#[test]
fn every_failure_has_its_contract_code() {
    for (error, code) in [
        (
            SourceError::Check(CheckFailure::Network),
            "feed_unreachable",
        ),
        (SourceError::Check(CheckFailure::Proxy), "proxy_unreachable"),
        (
            SourceError::Check(CheckFailure::Verification),
            "candidate_unverified",
        ),
        (
            SourceError::Check(CheckFailure::SystemTooOld),
            "system_too_old",
        ),
        (SourceError::Install(InstallError::NotNewer), "not_newer"),
        (
            SourceError::Install(InstallError::Skipped),
            "version_skipped",
        ),
        (SourceError::Install(InstallError::Busy), "service_busy"),
        (
            SourceError::Install(InstallError::Failed(InstallFailure::DiskFull)),
            "disk_full",
        ),
        (
            SourceError::Install(InstallError::Failed(InstallFailure::Download)),
            "download_failed",
        ),
        (
            SourceError::Install(InstallError::Core(CoreError::StorageUnavailable)),
            "storage_unavailable",
        ),
        (SourceError::Protocol, "protocol_error"),
    ] {
        assert_eq!(error.code(), code);
    }
}
