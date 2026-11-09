//! Update sources: where a SpaceTerm Core's new versions come from. Every source hands a signed
//! archive to the update engine, which verifies it and installs it beside the running version.
//! Install ends nothing; a later restart moves the Core to the Pending Version.
//!
//! - A Release Core checks its own signed feed ([`FeedCheck`]) on a schedule and when asked.
//!   While the Core is below the newest release's Update Floor, the check steps down to the
//!   release at that floor ([`feed::walk`]). After a restart into such a Stepping Stone, the
//!   Core checks again at once.
//! - Preflight and Development Cores never fetch a feed. They take a desktop push or a local
//!   archive only.
//! - Any Core takes a push of a desktop's embedded build ([`Pushes`]) and a local archive.
//!
//! The engine refuses a version that is not newer than every version the Core has, that it
//! skipped, or whose Update Floor is above the running version, from every source
//! ([`super::ordering`]). Sources ask it before they download or receive anything, so a refused
//! version costs nothing.
use super::check;
use super::facts::{CheckFailure, InstallFailure, PendingVersion, UpdateCheck, UpdateFacts};
use super::feed::{self, FeedTarget, MAX_ARCHIVE, SystemVersion};
use super::fetch::{FeedSource, FetchError, Releases};
use super::ordering::Candidate;
use super::proxy::{self, Proxy};
use super::source::FactsSource;
use super::trust::{SIGNATURE_BYTES, UpdateTrust};
use crate::CoreError;
use crate::service::updates::{self, Archive, InstallError, Source, Updates};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The build identity that publishes a feed.
pub const RELEASE: &str = "spaceterm";
/// How long after a Core starts it checks its feed first.
pub const FIRST_CHECK: Duration = Duration::from_secs(120);
/// How often a running Release Core checks its feed.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How often install progress is reported while bytes arrive.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// What the sources need from the update engine. [`Updates`] is the engine; tests use a fake.
pub trait Engine: Send + Sync {
    type Installing: Installing;
    type Checking<'a>: Checking<Installing = Self::Installing>
    where
        Self: 'a;
    fn facts(&self) -> UpdateFacts;
    fn begin_install(
        &self,
        candidate: Candidate,
        total: u64,
    ) -> Result<Self::Installing, InstallError>;
    fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError>;
    /// Whether `candidate` is a version the Core installs, whatever update operation runs now.
    fn admits_version(&self, candidate: &Candidate) -> Result<(), InstallError>;
    /// Busy while another install, check or restart owns the update operation.
    fn begin_check(&self) -> Result<Self::Checking<'_>, InstallError>;
    /// Whether the Core runs a Stepping Stone its feed check installed and has not checked
    /// its feed since.
    fn stepping_stone(&self) -> bool;
}

/// An install the engine reports as `Installing` until it ends. Dropping it records nothing.
pub trait Installing: Send {
    fn progress(&self, received: u64);
    fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError>;
    fn fail(self, failure: InstallFailure);
}

/// A feed check that owns the engine's update operation, which it reports as `Checking`, until
/// it is dropped or becomes the install of the release it found.
pub trait Checking {
    type Installing;
    /// Whether the check's own install of `candidate` would be admitted.
    fn admits(&self, candidate: &Candidate) -> Result<(), InstallError>;
    /// Records the check's outcome. The check keeps the operation.
    fn finish(&self, check: UpdateCheck);
    /// Turns the check into the install of `candidate`, a Stepping Stone when `stepping_stone`.
    fn begin_install(
        self,
        candidate: Candidate,
        total: u64,
        stepping_stone: bool,
    ) -> Result<Self::Installing, InstallError>;
}

impl Engine for Updates {
    type Installing = updates::Installing;
    type Checking<'a> = updates::Checking<'a>;

    fn facts(&self) -> UpdateFacts {
        FactsSource::facts(self)
    }
    fn begin_install(
        &self,
        candidate: Candidate,
        total: u64,
    ) -> Result<updates::Installing, InstallError> {
        Updates::begin_install(self, candidate, total)
    }
    fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        Updates::install(self, archive)
    }
    fn admits_version(&self, candidate: &Candidate) -> Result<(), InstallError> {
        Updates::admits_version(self, candidate)
    }
    fn begin_check(&self) -> Result<updates::Checking<'_>, InstallError> {
        Updates::begin_check(self)
    }
    fn stepping_stone(&self) -> bool {
        Updates::stepping_stone(self)
    }
}

impl Installing for updates::Installing {
    fn progress(&self, received: u64) {
        updates::Installing::progress(self, received);
    }
    fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        updates::Installing::install(self, archive)
    }
    fn fail(self, failure: InstallFailure) {
        updates::Installing::fail(self, failure);
    }
}

impl Checking for updates::Checking<'_> {
    type Installing = updates::Installing;
    fn admits(&self, candidate: &Candidate) -> Result<(), InstallError> {
        updates::Checking::admits(self, candidate)
    }
    fn finish(&self, check: UpdateCheck) {
        updates::Checking::finish(self, check);
    }
    fn begin_install(
        self,
        candidate: Candidate,
        total: u64,
        stepping_stone: bool,
    ) -> Result<updates::Installing, InstallError> {
        updates::Checking::begin_install(self, candidate, total, stepping_stone)
    }
}

/// What a feed check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The release the feed names is now the Pending Version.
    Installed(PendingVersion),
    /// The feed names no version newer than the ones the Core has, or only a skipped one.
    Current,
}

/// Why a source did not install anything. It carries no URL, path or native error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Preflight and Development Cores never fetch a feed.
    #[error("this build identity has no update feed")]
    NoFeed,
    /// The feed check failed. The engine records it as the check state.
    #[error("the update check failed")]
    Check(CheckFailure),
    /// The engine refused or failed the install.
    #[error("the install did not finish")]
    Install(InstallError),
    /// A malformed or out-of-order push request.
    #[error("the push request is malformed")]
    Protocol,
}

impl SourceError {
    /// The CLI output contract's code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoFeed => "no_update_feed",
            Self::Check(CheckFailure::Network) => "feed_unreachable",
            Self::Check(CheckFailure::Proxy) => "proxy_unreachable",
            Self::Check(CheckFailure::Verification) => "candidate_unverified",
            Self::Check(CheckFailure::SystemTooOld) => "system_too_old",
            Self::Check(CheckFailure::ClockBehind) => crate::service::lane::CLOCK_BEHIND,
            Self::Check(CheckFailure::SteppingStoneSkipped) => "version_skipped",
            Self::Install(error) => install_code(error),
            Self::Protocol => "protocol_error",
        }
    }
}

/// The CLI output contract's code of an engine install refusal.
pub fn install_code(error: &InstallError) -> &'static str {
    match error {
        InstallError::NotNewer => "not_newer",
        InstallError::Skipped => "version_skipped",
        InstallError::BelowFloor { .. } => "below_floor",
        InstallError::Busy => "service_busy",
        InstallError::Failed(InstallFailure::DiskFull) => "disk_full",
        InstallError::Failed(InstallFailure::Download) => "download_failed",
        InstallError::Failed(InstallFailure::Verification) => "candidate_unverified",
        InstallError::Core(error) => crate::service::lane_codes::code(error),
    }
}

impl From<InstallError> for SourceError {
    fn from(error: InstallError) -> Self {
        Self::Install(error)
    }
}

impl From<CoreError> for SourceError {
    fn from(error: CoreError) -> Self {
        Self::Install(InstallError::Core(error))
    }
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Builds the feed source for one check, through the proxy set at that moment.
pub type Sources = dyn Fn(Option<&Proxy>) -> Box<dyn FeedSource> + Send + Sync;

/// A Release Core's feed check: fetch the feed and its signature, verify both, refuse a version
/// the engine would not install, check the host's system, then download and install the archive.
pub struct FeedCheck {
    target: FeedTarget,
    /// None when the embedded trust is malformed. Every check then fails verification.
    trust: Option<UpdateTrust>,
    sources: Box<Sources>,
    system: fn() -> Option<SystemVersion>,
    clock: fn() -> u64,
    /// The Core's data directory, which holds the proxy setting and the staging files.
    data: PathBuf,
}

impl FeedCheck {
    /// The feed check of the Core of `identity` whose data directory is `data`. None for every
    /// identity but Release, which alone fetches a feed.
    pub fn for_identity(identity: &str, data: PathBuf) -> Option<Self> {
        (identity == RELEASE).then(|| Self {
            target: FeedTarget::core(super::system::PLATFORM),
            trust: UpdateTrust::release().ok(),
            sources: Box::new(|proxy| Box::new(Releases::new(proxy))),
            system: super::system::host,
            clock: unix_now,
            data,
        })
    }

    /// A Release feed check over `source` with the fixture `trust`, for tests of the lane.
    #[cfg(test)]
    pub(crate) fn fixture(
        data: PathBuf,
        trust: Option<UpdateTrust>,
        source: Arc<dyn FeedSource>,
    ) -> Self {
        struct Shared(Arc<dyn FeedSource>);
        impl FeedSource for Shared {
            fn feed(
                &self,
                target: FeedTarget,
                release: Option<&super::facts::Version>,
            ) -> Result<super::fetch::Signed, FetchError> {
                self.0.feed(target, release)
            }
            fn archive(
                &self,
                target: FeedTarget,
                version: &super::facts::Version,
                size: u64,
                into: &mut dyn Write,
            ) -> Result<(), FetchError> {
                self.0.archive(target, version, size, into)
            }
        }
        Self {
            target: FeedTarget::core(super::system::PLATFORM),
            trust,
            sources: Box::new(move |_| Box::new(Shared(source.clone()))),
            system: super::system::host,
            clock: unix_now,
            data,
        }
    }

    /// Checks the feed once, and installs the release the floors lead to when the engine admits
    /// it.
    pub fn run<E: Engine>(&self, engine: &E) -> Result<Outcome, SourceError> {
        // The check owns the engine's update operation from here, so no push or other check can
        // start until it ends or hands over to its own install.
        let checking = engine.begin_check()?;
        let at = (self.clock)();
        // The previous state is read when the check ends, as the engine persists it.
        let finish = |checking: &E::Checking<'_>, outcome: Result<(), CheckFailure>| {
            checking.finish(check::next(&engine.facts().check, at, outcome));
            outcome.map_err(SourceError::Check)
        };
        let proxy = proxy::read(&self.data)?;
        let source = (self.sources)(proxy.as_ref());
        let running = engine.facts().running;
        // A chain of floors that leads to no release is a publishing error, which the Core
        // treats as a feed that does not verify.
        let next = feed::walk(&running, |release| {
            let signed = source.feed(self.target, release).map_err(check_failure)?;
            let trust = self.trust.as_ref().ok_or(CheckFailure::Verification)?;
            feed::read(self.target, &signed.feed, &signed.signature, trust, at).map_err(|error| {
                match error {
                    feed::FeedError::ClockBehind => CheckFailure::ClockBehind,
                    _ => CheckFailure::Verification,
                }
            })
        })
        .and_then(|next| next.ok_or(CheckFailure::Verification));
        let (verified, stepping_stone) = match next {
            Ok(next) => (next.feed, next.stepping_stone),
            Err(failure) => {
                return finish(&checking, Err(failure)).map(|()| Outcome::Current);
            }
        };
        match checking.admits(&verified.candidate()) {
            Ok(()) => {}
            // The newest release waits behind a Stepping Stone that failed to start, so the
            // Core can't reach it. That is no current Core.
            Err(InstallError::Skipped) if stepping_stone => {
                return finish(&checking, Err(CheckFailure::SteppingStoneSkipped))
                    .map(|()| Outcome::Current);
            }
            Err(InstallError::NotNewer | InstallError::Skipped) => {
                return finish(&checking, Ok(())).map(|()| Outcome::Current);
            }
            Err(error) => {
                finish(&checking, Ok(()))?;
                return Err(error.into());
            }
        }
        if (self.system)().is_none_or(|host| host < verified.minimum_system) {
            return finish(&checking, Err(CheckFailure::SystemTooOld)).map(|()| Outcome::Current);
        }
        finish(&checking, Ok(()))?;
        let installing =
            checking.begin_install(verified.candidate(), verified.archive.size, stepping_stone)?;
        self.download(source.as_ref(), &verified, installing)
            .map(Outcome::Installed)
    }

    /// Downloads at most the feed's archive size into private staging files, checks the size and
    /// digest, and hands the archive with the feed's signature to the engine, which verifies the
    /// signature and the build document before it keeps anything.
    fn download<I: Installing>(
        &self,
        source: &dyn FeedSource,
        verified: &feed::Feed,
        installing: I,
    ) -> Result<PendingVersion, SourceError> {
        let staging = match Staging::create(&self.data) {
            Ok(staging) => staging,
            Err(error) => {
                installing.fail(InstallFailure::DiskFull);
                return Err(error.into());
            }
        };
        let received = (|| {
            let mut file = staging.archive_file().map_err(|_| FetchError::Storage)?;
            let mut writer = Digesting::new(&mut file, |received| installing.progress(received));
            source.archive(
                verified.target,
                &verified.version,
                verified.archive.size,
                &mut writer,
            )?;
            let digest = writer.finish();
            file.sync_all()
                .map_err(|error| super::fetch::storage(&error))?;
            Ok(digest)
        })();
        let sha256 = match received {
            Ok(sha256) => sha256,
            Err(error) => {
                let failure = install_failure(error);
                installing.fail(failure);
                return Err(InstallError::Failed(failure).into());
            }
        };
        if sha256 != verified.archive.sha256 {
            installing.fail(InstallFailure::Verification);
            return Err(InstallError::Failed(InstallFailure::Verification).into());
        }
        if staging
            .write_signature(&verified.archive.signature)
            .is_err()
        {
            installing.fail(InstallFailure::DiskFull);
            return Err(InstallError::Failed(InstallFailure::DiskFull).into());
        }
        let digest = hex(&sha256);
        Ok(installing.install(staging.archive(&digest, Source::Feed))?)
    }

    /// How long a Core that starts waits before its first check: none on a Stepping Stone its
    /// feed check installed, otherwise [`FIRST_CHECK`].
    pub fn first_check<E: Engine>(engine: &E) -> Duration {
        if engine.stepping_stone() {
            Duration::ZERO
        } else {
            FIRST_CHECK
        }
    }

    /// Runs the checks of a running Release Core until `stopping` returns true: first after
    /// [`Self::first_check`], then every [`CHECK_INTERVAL`].
    pub fn spawn<E: Engine + 'static>(
        self: Arc<Self>,
        engine: E,
        stopping: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let _ = std::thread::Builder::new()
            .name("spaceterm-update-check".to_owned())
            .spawn(move || {
                let mut wait = Self::first_check(&engine);
                loop {
                    let until = std::time::Instant::now() + wait;
                    while std::time::Instant::now() < until {
                        if stopping() {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    let _ = self.run(&engine);
                    wait = CHECK_INTERVAL;
                }
            });
    }
}

fn check_failure(error: FetchError) -> CheckFailure {
    match error {
        FetchError::Proxy => CheckFailure::Proxy,
        FetchError::TooLarge => CheckFailure::Verification,
        FetchError::Network | FetchError::DiskFull | FetchError::Storage => CheckFailure::Network,
    }
}

fn install_failure(error: FetchError) -> InstallFailure {
    match error {
        FetchError::DiskFull | FetchError::Storage => InstallFailure::DiskFull,
        FetchError::TooLarge => InstallFailure::Verification,
        FetchError::Network | FetchError::Proxy => InstallFailure::Download,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The pushes a running Core receives over its service lane: a desktop's embedded build, or a
/// local archive the CLI sends. One push at a time. A push whose connection closes ends with it.
/// A push that breaks off tells its sender alone; the Core records a failure only for a completed
/// push that fails verification.
///
/// Two desktops may push to one Machine at once. A newer version replaces an unfinished push,
/// whose next chunk then fails `changed`, because the Machine no longer needs it. A push of the
/// same or an older version waits: it fails `service_busy` while the unfinished one still
/// receives, and its sender tries again later. An unfinished push that received nothing for
/// [`PUSH_IDLE`] is abandoned, and any push replaces it.
pub struct Pushes {
    data: PathBuf,
    slot: Mutex<Option<Push>>,
}

/// What one received chunk led to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    /// The push expects its next chunk at this offset.
    Next(u64),
    /// The whole archive arrived, verified and installed.
    Installed(PendingVersion),
}

/// Whether release `version` is newer than release `than`. Development versions have no order.
fn newer(version: &super::facts::Version, than: &super::facts::Version) -> bool {
    matches!((version.release(), than.release()), (Some(a), Some(b)) if a > b)
}

/// How long an unfinished push may receive nothing before another push may replace it.
pub const PUSH_IDLE: Duration = Duration::from_secs(60);

struct Push {
    id: String,
    connection: u64,
    /// The version it installs.
    version: super::facts::Version,
    /// When its last chunk, or its start, arrived.
    received: std::time::Instant,
    length: u64,
    sha256: [u8; 32],
    next: u64,
    file: std::fs::File,
    digest: ring::digest::Context,
    reported: std::time::Instant,
    staging: Staging,
    installing: Box<dyn BoxedInstalling>,
}

/// A boxed install, so one push slot holds any engine's.
trait BoxedInstalling: Send {
    fn progress(&self, received: u64);
    fn install(self: Box<Self>, archive: Archive<'_>) -> Result<PendingVersion, InstallError>;
    fn fail(self: Box<Self>, failure: InstallFailure);
}

impl<I: Installing> BoxedInstalling for I {
    fn progress(&self, received: u64) {
        Installing::progress(self, received);
    }
    fn install(self: Box<Self>, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        Installing::install(*self, archive)
    }
    fn fail(self: Box<Self>, failure: InstallFailure) {
        Installing::fail(*self, failure);
    }
}

impl Pushes {
    /// The pushes of the Core whose data directory is `data`.
    pub fn new(data: PathBuf) -> Self {
        Self {
            data,
            slot: Mutex::new(None),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Push>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Begins receiving `length` archive bytes of `candidate` with this digest and signature
    /// over `connection`. The engine decides first, so a refused version receives nothing.
    /// Returns the push's random name.
    pub fn begin<E: Engine>(
        &self,
        engine: &E,
        connection: u64,
        candidate: Candidate,
        length: u64,
        sha256: &str,
        signature: &[u8],
    ) -> Result<String, SourceError>
    where
        E::Installing: 'static,
    {
        let sha256 = super::facts::lowercase_hex::<32>(sha256).ok_or(SourceError::Protocol)?;
        if !(1..=MAX_ARCHIVE).contains(&length) || signature.len() != SIGNATURE_BYTES {
            return Err(SourceError::Protocol);
        }
        let mut slot = self.slot();
        // A newer version replaces an unfinished push, which records nothing. Any other waits
        // for it, unless it stalled.
        if let Some(unfinished) = slot.as_ref() {
            if unfinished.received.elapsed() < PUSH_IDLE
                && !newer(candidate.version(), &unfinished.version)
            {
                return Err(SourceError::Install(InstallError::Busy));
            }
            // A version the Core refuses replaces nothing.
            engine.admits_version(&candidate)?;
        }
        *slot = None;
        let candidate_version = candidate.version().clone();
        let installing: Box<dyn BoxedInstalling> =
            Box::new(engine.begin_install(candidate, length)?);
        let staging = Staging::create(&self.data)?;
        let file = staging.archive_file()?;
        staging.write_signature(signature)?;
        let id = hex(&crate::protocol::random()?[..16]);
        *slot = Some(Push {
            id: id.clone(),
            connection,
            version: candidate_version,
            received: std::time::Instant::now(),
            length,
            sha256,
            next: 0,
            file,
            digest: ring::digest::Context::new(&ring::digest::SHA256),
            reported: std::time::Instant::now(),
            staging,
            installing,
        });
        Ok(id)
    }

    /// Writes one chunk of the push named `id` at `offset`. The last chunk installs the archive.
    pub fn chunk(&self, id: &str, offset: u64, data: &[u8]) -> Result<Received, SourceError> {
        let mut slot = self.slot();
        let push = slot
            .as_mut()
            .filter(|push| push.id == id)
            .ok_or(SourceError::Install(InstallError::Core(CoreError::Changed)))?;
        if data.is_empty()
            || offset != push.next
            || push.next.saturating_add(data.len() as u64) > push.length
        {
            return Err(SourceError::Protocol);
        }
        if let Err(error) = push.file.write_all(data) {
            // A push that breaks off tells its sender alone, and records nothing.
            *slot = None;
            return Err(
                InstallError::Failed(install_failure(super::fetch::storage(&error))).into(),
            );
        }
        push.digest.update(data);
        push.next += data.len() as u64;
        push.received = std::time::Instant::now();
        if push.next < push.length {
            if push.reported.elapsed() >= PROGRESS_INTERVAL {
                push.reported = std::time::Instant::now();
                push.installing.progress(push.next);
            }
            return Ok(Received::Next(push.next));
        }
        let push = slot.take().ok_or(SourceError::Protocol)?;
        drop(slot);
        push.installing.progress(push.length);
        let Push {
            file,
            digest,
            sha256,
            staging,
            installing,
            ..
        } = push;
        if file.sync_all().is_err() {
            drop(installing);
            return Err(InstallError::Failed(InstallFailure::DiskFull).into());
        }
        drop(file);
        if digest.finish().as_ref() != sha256 {
            installing.fail(InstallFailure::Verification);
            return Err(InstallError::Failed(InstallFailure::Verification).into());
        }
        let digest = hex(&sha256);
        Ok(Received::Installed(
            installing.install(staging.archive(&digest, Source::Push))?,
        ))
    }

    /// Ends the unfinished push of `connection`, which closed. It records nothing.
    pub fn closed(&self, connection: u64) {
        let mut slot = self.slot();
        if slot
            .as_ref()
            .is_some_and(|push| push.connection == connection)
        {
            *slot = None;
        }
    }
}

/// A writer that digests and counts what passes through it, and reports the count at most every
/// [`PROGRESS_INTERVAL`].
struct Digesting<'a, P: Fn(u64)> {
    into: &'a mut dyn Write,
    digest: ring::digest::Context,
    written: u64,
    reported: std::time::Instant,
    progress: P,
}

impl<'a, P: Fn(u64)> Digesting<'a, P> {
    fn new(into: &'a mut dyn Write, progress: P) -> Self {
        progress(0);
        Self {
            into,
            digest: ring::digest::Context::new(&ring::digest::SHA256),
            written: 0,
            reported: std::time::Instant::now(),
            progress,
        }
    }

    fn finish(self) -> [u8; 32] {
        (self.progress)(self.written);
        let mut out = [0; 32];
        out.copy_from_slice(self.digest.finish().as_ref());
        out
    }
}

impl<P: Fn(u64)> Write for Digesting<'_, P> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.into.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.written += written as u64;
        if self.reported.elapsed() >= PROGRESS_INTERVAL {
            self.reported = std::time::Instant::now();
            (self.progress)(self.written);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.into.flush()
    }
}

/// Removes the files in `<data>/staging` whose install ended without removing them, as a crash
/// leaves them. A live install of this release holds its owner lock, in this process or
/// another, such as a stopped Core's CLI downloading from the feed, so its files stay. An
/// install of an earlier release takes no owner lock and leaves no other sign that it ended,
/// so its files always stay. It removes them itself when it ends; only a crash leaves them.
pub(crate) fn clear_staging(data: &Path) {
    let directory = data.join("staging");
    let Ok(staging) = crate::storage::Directory::open(&directory, false) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.split('.').next()?;
            (id.len() == 32
                && id
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| id.to_owned())
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let names = Staging::names(&id);
        let [_, _, owner] = &names;
        // Taking a lock creates its file, so only a lock file that exists proves an owner, and
        // only a free one proves that owner ended.
        let ended = directory.join(owner).exists()
            && staging.try_lock(owner).is_ok_and(|lock| lock.is_some());
        if ended {
            for name in &names {
                let _ = std::fs::remove_file(directory.join(name));
            }
        }
    }
}

/// One install's private files in `<data>/staging`, removed when it ends. Its owner lock lasts
/// as long as the install, so another process's cleanup leaves them.
struct Staging {
    archive: PathBuf,
    signature: PathBuf,
    owner: PathBuf,
    _lock: crate::storage::Lock,
}

impl Staging {
    fn create(data: &Path) -> Result<Self, CoreError> {
        let directory = data.join("staging");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)
            .map_err(|_| CoreError::StorageUnavailable)?;
        let staging = crate::storage::Directory::open(&directory, false)?;
        let id = hex(&crate::protocol::random()?[..16]);
        // The owner lock comes first, so no file of this install exists without it.
        let [archive, signature, owner] = Self::names(&id);
        let lock = staging.lock(&owner, true)?;
        Ok(Self {
            archive: directory.join(archive),
            signature: directory.join(signature),
            owner: directory.join(owner),
            _lock: lock,
        })
    }

    /// The archive, signature and owner lock names of the install `id`.
    fn names(id: &str) -> [String; 3] {
        [
            format!("{id}.tar.gz"),
            format!("{id}.sig"),
            format!("{id}.lock"),
        ]
    }

    fn archive_file(&self) -> Result<std::fs::File, CoreError> {
        create_private(&self.archive)
    }

    fn write_signature(&self, signature: &[u8]) -> Result<(), CoreError> {
        create_private(&self.signature)?
            .write_all(signature)
            .map_err(|_| CoreError::StorageUnavailable)
    }

    fn archive<'a>(&'a self, sha256: &'a str, source: Source) -> Archive<'a> {
        Archive {
            archive: &self.archive,
            signature: &self.signature,
            sha256,
            source,
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.archive);
        let _ = std::fs::remove_file(&self.signature);
        let _ = std::fs::remove_file(&self.owner);
    }
}

fn create_private(path: &Path) -> Result<std::fs::File, CoreError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| CoreError::StorageUnavailable)
}

#[cfg(test)]
mod tests;
