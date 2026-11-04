//! Update sources: where a SpaceTerm Core's new versions come from. Every source hands a signed
//! archive to the update engine, which verifies it and installs it beside the running version.
//! Install ends nothing; a later restart moves the Core to the Pending Version.
//!
//! - A Release Core checks its own signed feed ([`FeedCheck`]) on a schedule and when asked.
//! - Preflight and Development Cores never fetch a feed. They take a desktop push or a local
//!   archive only.
//! - Any Core takes a push of a desktop's embedded build ([`Pushes`]) and a local archive.
//!
//! The engine refuses a version that is not newer than every version the Core has, or that it
//! skipped, from every source ([`super::ordering`]). Sources ask it before they download or
//! receive anything, so a refused version costs nothing.
use super::check;
use super::facts::{
    CheckFailure, InstallFailure, PendingVersion, UpdateCheck, UpdateFacts, Version,
};
use super::feed::{self, FeedTarget, MAX_ARCHIVE, SystemVersion};
use super::fetch::{FeedSource, FetchError, Releases};
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
    type Checking<'a>: Checking
    where
        Self: 'a;
    fn facts(&self) -> UpdateFacts;
    fn admits(&self, version: &Version) -> Result<(), InstallError>;
    fn begin_install(&self, version: Version, total: u64)
    -> Result<Self::Installing, InstallError>;
    fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError>;
    fn begin_check(&self) -> Self::Checking<'_>;
}

/// An install the engine reports as `Installing` until it ends. Dropping it records nothing.
pub trait Installing: Send {
    fn progress(&self, received: u64);
    fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError>;
    fn fail(self, failure: InstallFailure);
}

/// A feed check the engine reports as `Checking` until it ends.
pub trait Checking {
    fn finish(self, check: UpdateCheck);
}

impl Engine for Updates {
    type Installing = updates::Installing;
    type Checking<'a> = updates::Checking<'a>;

    fn facts(&self) -> UpdateFacts {
        FactsSource::facts(self)
    }
    fn admits(&self, version: &Version) -> Result<(), InstallError> {
        Updates::admits(self, version)
    }
    fn begin_install(
        &self,
        version: Version,
        total: u64,
    ) -> Result<updates::Installing, InstallError> {
        Updates::begin_install(self, version, total)
    }
    fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        Updates::install(self, archive)
    }
    fn begin_check(&self) -> updates::Checking<'_> {
        Updates::begin_check(self)
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
    fn finish(self, check: UpdateCheck) {
        updates::Checking::finish(self, check);
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
            fn feed(&self, target: FeedTarget) -> Result<super::fetch::Signed, FetchError> {
                self.0.feed(target)
            }
            fn archive(
                &self,
                target: FeedTarget,
                version: &Version,
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

    /// Checks the feed once, and installs the release it names when the engine admits it.
    pub fn run<E: Engine>(&self, engine: &E) -> Result<Outcome, SourceError> {
        let checking = engine.begin_check();
        let at = (self.clock)();
        // The previous state is read when the check ends, as the engine persists it.
        let failed = |checking: E::Checking<'_>, reason| {
            checking.finish(check::next(&engine.facts().check, at, Err(reason)));
            SourceError::Check(reason)
        };
        let succeeded = |checking: E::Checking<'_>| {
            checking.finish(check::next(&engine.facts().check, at, Ok(())));
        };
        let proxy = proxy::read(&self.data)?;
        let source = (self.sources)(proxy.as_ref());
        let signed = match source.feed(self.target) {
            Ok(signed) => signed,
            Err(error) => return Err(failed(checking, check_failure(error))),
        };
        let verified = self.trust.as_ref().and_then(|trust| {
            feed::read(self.target, &signed.feed, &signed.signature, trust, at).ok()
        });
        let Some(verified) = verified else {
            return Err(failed(checking, CheckFailure::Verification));
        };
        match engine.admits(&verified.version) {
            Ok(()) => {}
            Err(InstallError::NotNewer | InstallError::Skipped) => {
                succeeded(checking);
                return Ok(Outcome::Current);
            }
            Err(error) => {
                succeeded(checking);
                return Err(error.into());
            }
        }
        if (self.system)().is_none_or(|host| host < verified.minimum_system) {
            return Err(failed(checking, CheckFailure::SystemTooOld));
        }
        succeeded(checking);
        let installing = engine.begin_install(verified.version.clone(), verified.archive.size)?;
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

    /// Runs the checks of a running Release Core until `stopping` returns true: first after
    /// [`FIRST_CHECK`], then every [`CHECK_INTERVAL`].
    pub fn spawn<E: Engine + 'static>(
        self: Arc<Self>,
        engine: E,
        stopping: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let _ = std::thread::Builder::new()
            .name("spaceterm-update-check".to_owned())
            .spawn(move || {
                let mut wait = FIRST_CHECK;
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
/// local archive the CLI sends. One push at a time; a new one replaces an unfinished one, and a
/// push whose connection closes ends with it. Neither records a failure.
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

struct Push {
    id: String,
    connection: u64,
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

    /// Begins receiving `length` archive bytes of `version` with this digest and signature over
    /// `connection`. The engine decides first, so a refused version receives nothing. Returns the
    /// push's random name.
    pub fn begin<E: Engine>(
        &self,
        engine: &E,
        connection: u64,
        version: Version,
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
        // A new push replaces an unfinished one, which records nothing.
        *slot = None;
        let installing: Box<dyn BoxedInstalling> = Box::new(engine.begin_install(version, length)?);
        let staging = Staging::create(&self.data)?;
        let file = staging.archive_file()?;
        staging.write_signature(signature)?;
        let id = hex(&crate::protocol::random()?[..16]);
        *slot = Some(Push {
            id: id.clone(),
            connection,
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
            let failure = install_failure(super::fetch::storage(&error));
            if let Some(push) = slot.take() {
                push.installing.fail(failure);
            }
            return Err(InstallError::Failed(failure).into());
        }
        push.digest.update(data);
        push.next += data.len() as u64;
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
            installing.fail(InstallFailure::DiskFull);
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

/// One install's private files in `<data>/staging`, removed when it ends.
struct Staging {
    archive: PathBuf,
    signature: PathBuf,
}

impl Staging {
    fn create(data: &Path) -> Result<Self, CoreError> {
        let directory = data.join("staging");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)
            .map_err(|_| CoreError::StorageUnavailable)?;
        crate::storage::Directory::open(&directory, false)?;
        let id = hex(&crate::protocol::random()?[..16]);
        Ok(Self {
            archive: directory.join(format!("{id}.tar.gz")),
            signature: directory.join(format!("{id}.sig")),
        })
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
