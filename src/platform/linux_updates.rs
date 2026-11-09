//! Self-installed Linux releases update from the signed release feed.
//!
//! The adapter verifies the feed and archive, unpacks the archive beside the installation, and
//! exchanges the two directories in one rename. The running process keeps its open files, so the
//! replaced tree stays in the staging directory until the next launch removes it. Relaunch runs
//! through the application quit policy, then replaces the exited process with the new executable.
//! A filesystem without that exchange cannot replace the installation in one step, so the update
//! stops before the download and the install script remains the way to update. A replaced
//! installation also rewrites the desktop entry the install script wrote for it, from the new
//! release's packaged entry.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::app_paths::AppPaths;
use crate::application_identity::ApplicationIdentity;
use crate::updates::policy::UpdateHistory;
use crate::updates::release_feed::{MAX_ARCHIVE_BYTES, ReleaseFeed, release_trust};
use crate::updates::{UpdateAdapter, UpdateError, UpdateEvent, UpdateTransport};
use spaceterm_core::update::feed::{FeedTarget, SystemVersion};
use spaceterm_core::update::trust::UpdateTrust;

#[cfg(target_arch = "x86_64")]
const TARGET: Option<FeedTarget> = Some(FeedTarget::DESKTOP_LINUX);
/// Releases publish no archive for this architecture yet.
#[cfg(not(target_arch = "x86_64"))]
const TARGET: Option<FeedTarget> = None;

const MAX_UNPACKED_BYTES: u64 = 2 * MAX_ARCHIVE_BYTES;
const MAX_ARCHIVE_ENTRIES: usize = 4096;
const DOWNLOAD_CHUNK: usize = 64 * 1024;
/// How long a download waits for a staging cleanup that holds the installation claim.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(10);
const CLAIM_RETRY: Duration = Duration::from_millis(100);

/// The executable a completed update asks the exiting process to become. The process becomes it
/// only when its exit is the update relaunch the person approved, so an ordinary quit after a
/// failed relaunch exits without starting SpaceTerm again.
#[derive(Clone, Default)]
pub(super) struct Relaunch(Rc<RefCell<RelaunchRequest>>);

#[derive(Default)]
struct RelaunchRequest {
    executable: Option<PathBuf>,
    approved: bool,
}

impl Relaunch {
    /// Records whether the exit now starting is the approved update relaunch.
    pub(super) fn exiting(&self, approved: bool) {
        self.0.borrow_mut().approved = approved;
    }

    /// The executable the exited process becomes, only after an approved relaunch.
    pub(super) fn take(&self) -> Option<PathBuf> {
        let mut request = self.0.borrow_mut();
        let executable = request.executable.take();
        executable.filter(|_| std::mem::take(&mut request.approved))
    }

    fn request(&self, executable: PathBuf) {
        self.0.borrow_mut().executable = Some(executable);
    }
}

/// Replaces this process with the installed executable. Returns only when exec fails.
pub(super) fn relaunch(executable: &Path) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    // The new instance queues for the application name this exiting process still releases.
    std::process::Command::new(executable)
        .arg(super::linux_application_instance::InstanceLaunch::NEW_INSTANCE_ARGUMENT)
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID")
        .exec()
}

pub(super) struct LinuxUpdates {
    dependencies: Option<Dependencies>,
    shared: RefCell<Option<Arc<Shared>>>,
    history: super::update_history_file::UpdateHistoryFile,
    relaunch: Relaunch,
    request_quit: Box<dyn Fn()>,
}

struct Dependencies {
    transport: Arc<dyn UpdateTransport>,
    trust: UpdateTrust,
    target: FeedTarget,
    installation: Installation,
    archive_root: &'static str,
    desktop_entry: Option<DesktopEntry>,
}

impl LinuxUpdates {
    pub(super) fn new(
        transport: Arc<dyn UpdateTransport>,
        identity: ApplicationIdentity,
        paths: Option<Arc<AppPaths>>,
        relaunch: Relaunch,
        request_quit: impl Fn() + 'static,
    ) -> Self {
        let installation = std::env::current_exe()
            .ok()
            .and_then(|executable| Installation::locate(&executable));
        let dependencies = match (TARGET, release_trust(), installation) {
            (Some(target), Some(trust), Some(installation)) => Some(Dependencies {
                transport,
                trust,
                target,
                installation,
                archive_root: identity.directory_name(),
                desktop_entry: DesktopEntry::locate(identity.application_id()),
            }),
            _ => None,
        };
        Self {
            dependencies,
            shared: RefCell::new(None),
            history: super::update_history_file::UpdateHistoryFile::new(paths),
            relaunch,
            request_quit: Box::new(request_quit),
        }
    }

    fn shared(&self) -> Result<Arc<Shared>, UpdateError> {
        self.shared.borrow().clone().ok_or(UpdateError::Unavailable)
    }
}

impl UpdateAdapter for LinuxUpdates {
    fn start(&self, events: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
        if self.shared.borrow().is_some() {
            return Err(UpdateError::Unavailable);
        }
        let dependencies = self.dependencies.as_ref().ok_or(UpdateError::Unavailable)?;
        let shared = Arc::new(Shared {
            transport: Arc::clone(&dependencies.transport),
            trust: dependencies.trust.clone(),
            target: dependencies.target,
            installation: dependencies.installation.clone(),
            archive_root: dependencies.archive_root,
            desktop_entry: dependencies.desktop_entry.clone(),
            current_version: env!("SPACETERM_VERSION"),
            system: spaceterm_core::update::system::host,
            events,
            cycle: Mutex::new(Cycle::default()),
        });
        // An earlier update leaves the tree it replaced in the staging directory.
        shared.discard_staging_in_background();
        *self.shared.borrow_mut() = Some(shared);
        Ok(())
    }

    fn observe_clock_changes(&self, changed: Box<dyn Fn() + Send + Sync>) {
        super::linux_clock_changes::observe(changed);
    }

    fn check(&self) -> Result<(), UpdateError> {
        let shared = self.shared()?;
        let generation = shared.begin(Phase::Idle, Phase::Checking, UpdateError::Check)?;
        spawn(shared, move |shared| shared.check(generation))
    }

    fn download(&self) -> Result<(), UpdateError> {
        let shared = self.shared()?;
        let (feed, generation) = {
            let mut cycle = shared.lock();
            match std::mem::take(&mut cycle.phase) {
                Phase::Available(feed) => {
                    cycle.phase = Phase::Downloading;
                    (feed, cycle.generation)
                }
                phase => {
                    cycle.phase = phase;
                    return Err(UpdateError::Download);
                }
            }
        };
        if !shared.installation.writable() {
            shared.lock().reset();
            shared.fail(UpdateError::ReadOnly);
            return Ok(());
        }
        spawn(shared, move |shared| shared.download(&feed, generation))
    }

    fn install(&self) -> Result<(), UpdateError> {
        let shared = self.shared()?;
        let mut cycle = shared.lock();
        match cycle.phase {
            // The quit confirmation was cancelled after the installation was replaced.
            Phase::Installed => {
                drop(cycle);
                (self.request_quit)();
                return Ok(());
            }
            Phase::Ready => {}
            _ => return Err(UpdateError::Installation),
        }
        if let Err(error) = shared.installation.replace() {
            cycle.reset();
            drop(cycle);
            shared.discard_staging_in_background();
            shared.fail(installation_failure(&error));
            return Ok(());
        }
        cycle.phase = Phase::Installed;
        drop(cycle);
        shared.refresh_desktop_entry();
        self.relaunch
            .request(shared.installation.executable.clone());
        shared.send(UpdateEvent::Installing);
        (self.request_quit)();
        Ok(())
    }

    fn finish_on_quit(&self) -> Result<(), UpdateError> {
        let shared = self.shared()?;
        let mut cycle = shared.lock();
        if !matches!(cycle.phase, Phase::Ready) {
            return Err(UpdateError::Unavailable);
        }
        shared
            .installation
            .replace()
            .map_err(|error| installation_failure(&error))?;
        cycle.phase = Phase::Installed;
        drop(cycle);
        shared.refresh_desktop_entry();
        Ok(())
    }

    fn cancel(&self) {
        let Ok(shared) = self.shared() else {
            return;
        };
        let mut cycle = shared.lock();
        if matches!(cycle.phase, Phase::Idle | Phase::Installed) {
            return;
        }
        // A worker still blocked on the network finds its cycle gone when it resumes, so
        // cancellation and quit never wait for the network.
        cycle.reset();
        drop(cycle);
        shared.discard_staging_in_background();
        shared.send(UpdateEvent::Finished);
    }

    fn load_history(&self) -> UpdateHistory {
        self.history.load()
    }

    fn save_history(&self, history: &UpdateHistory) {
        self.history.save(history);
    }
}

/// Classifies a failure to stage or replace the installation. A filesystem without exchange
/// support can never replace it, so its failure names the install script remedy instead of
/// offering a retry. A full disk or quota asks for space.
fn installation_failure(error: &io::Error) -> UpdateError {
    if error.kind() == io::ErrorKind::Unsupported {
        UpdateError::ReadOnly
    } else if disk_full(error) {
        UpdateError::DiskFull
    } else {
        UpdateError::Installation
    }
}

/// Classifies a failure to unpack a verified archive: a full disk asks for space, and an archive
/// that does not hold one well-formed installation fails verification.
fn unpack_failure(error: &io::Error) -> UpdateError {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::UnexpectedEof => {
            UpdateError::Verification
        }
        _ => installation_failure(error),
    }
}

fn disk_full(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) || matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT))
}

fn spawn(
    shared: Arc<Shared>,
    work: impl FnOnce(&Shared) + Send + 'static,
) -> Result<(), UpdateError> {
    let worker = Arc::clone(&shared);
    if std::thread::Builder::new()
        .name("spaceterm-updates".into())
        .spawn(move || work(&worker))
        .is_ok()
    {
        Ok(())
    } else {
        shared.lock().reset();
        Err(UpdateError::Check)
    }
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Checking,
    Available(ReleaseFeed),
    Downloading,
    Ready,
    /// The installation was replaced. Only relaunch remains.
    Installed,
}

#[derive(Default)]
struct Cycle {
    phase: Phase,
    /// Advances whenever a cycle ends. A worker acts only while its generation is current.
    generation: u64,
    /// The installation claim a download holds until its cycle ends or the process exits.
    claim: Option<File>,
}

impl Cycle {
    fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.claim = None;
        self.generation += 1;
    }
}

/// State shared by the main-thread adapter and the worker of the current cycle.
struct Shared {
    transport: Arc<dyn UpdateTransport>,
    trust: UpdateTrust,
    target: FeedTarget,
    installation: Installation,
    archive_root: &'static str,
    desktop_entry: Option<DesktopEntry>,
    current_version: &'static str,
    /// The host's glibc version, which a release must meet.
    system: fn() -> Option<SystemVersion>,
    events: async_channel::Sender<UpdateEvent>,
    cycle: Mutex<Cycle>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cycle> {
        self.cycle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves an idle cycle forward and returns the generation its worker acts for.
    fn begin(&self, from: Phase, to: Phase, error: UpdateError) -> Result<u64, UpdateError> {
        let mut cycle = self.lock();
        if std::mem::discriminant(&cycle.phase) != std::mem::discriminant(&from) {
            return Err(error);
        }
        cycle.phase = to;
        Ok(cycle.generation)
    }

    fn send(&self, event: UpdateEvent) {
        let _ = self.events.try_send(event);
    }

    fn fail(&self, error: UpdateError) {
        self.send(UpdateEvent::Failed(error));
        self.send(UpdateEvent::Finished);
    }

    fn current(&self, generation: u64) -> bool {
        self.lock().generation == generation
    }

    /// Sends a worker's event only while its cycle is current, so nothing follows Finished.
    fn send_current(&self, generation: u64, event: UpdateEvent) -> Result<(), UpdateError> {
        let cycle = self.lock();
        if cycle.generation != generation {
            return Err(UpdateError::Download);
        }
        self.send(event);
        Ok(())
    }

    fn check(&self, generation: u64) {
        let result = self.fetch_feed();
        let mut cycle = self.lock();
        if cycle.generation != generation {
            return;
        }
        match result {
            // A release that needs a newer glibc than this computer has is not offered. The
            // check says so instead of reporting this release as current.
            Ok(feed) if feed.supersedes(self.current_version) && !feed.runs_on((self.system)()) => {
                cycle.reset();
                drop(cycle);
                self.fail(UpdateError::SystemTooOld);
            }
            Ok(feed) if feed.supersedes(self.current_version) => {
                let (version, published_at) = (feed.version.clone(), feed.published_at);
                cycle.phase = Phase::Available(feed);
                drop(cycle);
                self.send(UpdateEvent::ReleaseMetadata {
                    published_at,
                    prepared: false,
                });
                self.send(UpdateEvent::Available(version));
            }
            Ok(_) => {
                cycle.reset();
                drop(cycle);
                self.send(UpdateEvent::UpToDate);
                self.send(UpdateEvent::Finished);
            }
            Err(error) => {
                cycle.reset();
                drop(cycle);
                self.fail(error);
            }
        }
    }

    fn fetch_feed(&self) -> Result<ReleaseFeed, UpdateError> {
        ReleaseFeed::next(
            &self.trust,
            self.target,
            self.current_version,
            crate::updates::now(),
            |url, limit| {
                self.transport
                    .get(url, limit)
                    .map_err(|_| UpdateError::Check)
            },
        )
    }

    fn download(&self, feed: &ReleaseFeed, generation: u64) {
        let result = self.prepare(feed, generation);
        let mut cycle = self.lock();
        if cycle.generation != generation {
            return;
        }
        match result {
            Ok(()) => {
                cycle.phase = Phase::Ready;
                drop(cycle);
                self.send(UpdateEvent::Ready);
            }
            Err(error) => {
                cycle.reset();
                drop(cycle);
                self.discard_staging_in_background();
                self.fail(error);
            }
        }
    }

    /// Downloads, verifies, and unpacks the archive into the staging directory.
    fn prepare(&self, feed: &ReleaseFeed, generation: u64) -> Result<(), UpdateError> {
        let claim = self.claim(generation)?;
        {
            let mut cycle = self.lock();
            if cycle.generation != generation {
                return Err(UpdateError::Download);
            }
            cycle.claim = Some(claim);
        }
        let installation = &self.installation;
        installation
            .create_staging()
            .map_err(|error| installation_failure(&error))?;
        installation
            .confirm_exchange()
            .map_err(|error| installation_failure(&error))?;
        let archive = self.fetch_archive(feed, generation)?;
        self.send_current(generation, UpdateEvent::Verifying)?;
        feed.archive.verify(&self.trust, &archive)?;
        unpack(&archive, self.archive_root, &installation.staged_tree())
            .map_err(|error| unpack_failure(&error))?;
        if !Installation::executable_in(&installation.staged_tree()).is_file() {
            return Err(UpdateError::Verification);
        }
        Ok(())
    }

    /// Claims the installation, waiting briefly for a staging cleanup that holds the claim.
    /// Another process's update holds it longer, so this download fails instead of sharing its
    /// staging directory.
    fn claim(&self, generation: u64) -> Result<File, UpdateError> {
        let deadline = Instant::now() + CLAIM_TIMEOUT;
        loop {
            match self.installation.claim() {
                Ok(claim) => return Ok(claim),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && Instant::now() < deadline
                        && self.current(generation) =>
                {
                    std::thread::sleep(CLAIM_RETRY);
                }
                Err(_) => return Err(UpdateError::Installation),
            }
        }
    }

    fn fetch_archive(&self, feed: &ReleaseFeed, generation: u64) -> Result<Vec<u8>, UpdateError> {
        let total = feed.archive.size;
        let mut reader = self
            .transport
            .open(&feed.archive.url, total)
            .map_err(|_| UpdateError::Download)?;
        let mut archive = Vec::with_capacity(total as usize);
        let mut chunk = vec![0; DOWNLOAD_CHUNK];
        let mut reported = None;
        self.send_current(generation, UpdateEvent::Downloading { received: 0, total })?;
        loop {
            let read = reader.read(&mut chunk).map_err(|_| UpdateError::Download)?;
            if !self.current(generation) {
                return Err(UpdateError::Download);
            }
            if read == 0 {
                break;
            }
            archive.extend_from_slice(&chunk[..read]);
            let received = archive.len() as u64;
            if received > total {
                return Err(UpdateError::Download);
            }
            // Report whole percentages so a fast download does not flood the main thread.
            let percent = received * 100 / total;
            if reported != Some(percent) {
                reported = Some(percent);
                self.send_current(generation, UpdateEvent::Downloading { received, total })?;
            }
        }
        if archive.len() as u64 != total {
            return Err(UpdateError::Download);
        }
        Ok(archive)
    }

    /// Rewrites the desktop entry after the installation was replaced. The entry only describes
    /// how to launch SpaceTerm, so a failure leaves the update in place.
    fn refresh_desktop_entry(&self) {
        if let Some(entry) = &self.desktop_entry
            && entry.refresh(&self.installation).unwrap_or(false)
        {
            entry.refresh_database();
        }
    }

    fn discard_staging_in_background(&self) {
        let installation = self.installation.clone();
        // A replaced tree can be large; removing it must not block the main thread.
        let _ = std::thread::Builder::new()
            .name("spaceterm-update-cleanup".into())
            .spawn(move || {
                let _ = installation.discard_unclaimed_staging();
            });
    }
}

/// An installation the installer laid out: `<root>/bin/spaceterm-desktop` beside `<root>/share/spaceterm`.
#[derive(Clone, Debug)]
struct Installation {
    root: PathBuf,
    staging: PathBuf,
    executable: PathBuf,
    /// Exchanges two entries of the installation's filesystem in one rename.
    exchange: fn(&Path, &Path) -> io::Result<()>,
}

impl Installation {
    fn locate(executable: &Path) -> Option<Self> {
        let bin = executable.parent()?;
        if executable.file_name()? != "spaceterm-desktop" || bin.file_name()? != "bin" {
            return None;
        }
        let root = bin.parent()?;
        if !root.join("share").join("spaceterm").is_dir() {
            return None;
        }
        let mut staging = OsString::from(".");
        staging.push(root.file_name()?);
        staging.push(".update");
        Some(Self {
            staging: root.parent()?.join(staging),
            executable: Self::executable_in(root),
            root: root.to_path_buf(),
            exchange: super::linux_atomic_rename::exchange_paths,
        })
    }

    fn executable_in(root: &Path) -> PathBuf {
        root.join("bin").join("spaceterm-desktop")
    }

    fn staged_tree(&self) -> PathBuf {
        self.staging.join("tree")
    }

    /// Replacing the installation renames entries in its parent and inside the root itself.
    fn writable(&self) -> bool {
        [self.root.parent(), Some(self.root.as_path())]
            .into_iter()
            .all(|directory| directory.is_some_and(writable_directory))
    }

    fn create_staging(&self) -> io::Result<()> {
        self.discard_staging()?;
        fs::DirBuilder::new().mode(0o700).create(&self.staging)
    }

    /// Claims the installation for one process's update: an advisory lock on the installation
    /// directory, released when the returned file closes, including at exit and exec.
    fn claim(&self) -> io::Result<File> {
        use std::os::fd::AsRawFd as _;
        let directory = File::open(&self.root)?;
        // SAFETY: the descriptor is open for the duration of the call.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(directory)
    }

    /// Discards staging no update owns. While another process's update holds the claim, its
    /// staging stays. Without the installation directory there is nothing to claim, so staging
    /// stays too.
    fn discard_unclaimed_staging(&self) -> io::Result<()> {
        let _claim = self.claim()?;
        self.discard_staging()
    }

    fn discard_staging(&self) -> io::Result<()> {
        match fs::remove_dir_all(&self.staging) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    /// Confirms that the filesystem holding the installation exchanges entries in one rename,
    /// by exchanging two empty directories in the staging directory beside it.
    fn confirm_exchange(&self) -> io::Result<()> {
        let (first, second) = (self.staging.join("exchange"), self.staging.join("tree"));
        for directory in [&first, &second] {
            fs::DirBuilder::new().mode(0o700).create(directory)?;
        }
        let exchanged = (self.exchange)(&first, &second);
        for directory in [&first, &second] {
            fs::remove_dir(directory)?;
        }
        exchanged
    }

    /// Exchanges the staged tree with the installation in one rename, so an interruption at any
    /// point leaves a complete installation in place. A filesystem without exchange support
    /// reports `Unsupported` and keeps the current installation.
    fn replace(&self) -> io::Result<()> {
        (self.exchange)(&self.staged_tree(), &self.root)?;
        if let Some(parent) = self.root.parent() {
            let _ = File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    }
}

fn writable_directory(directory: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the path is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// The desktop entry the install script wrote for an installation:
/// `$XDG_DATA_HOME/applications/<application ID>.desktop`, from the release's packaged entry with
/// the installation's executable and icon filled in. A self-update writes it the same way from
/// the release it installed, so the entry follows the release.
#[derive(Clone, Debug)]
struct DesktopEntry {
    path: PathBuf,
    id: &'static str,
}

impl DesktopEntry {
    fn locate(id: &'static str) -> Option<Self> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(Self::under(&data_home, id))
    }

    fn under(data_home: &Path, id: &'static str) -> Self {
        Self {
            path: data_home.join("applications").join(format!("{id}.desktop")),
            id,
        }
    }

    /// Rewrites the entry from the release `installation` holds now. An entry that launches
    /// another installation, or that the person removed, stays as it is. Returns whether the
    /// entry changed.
    fn refresh(&self, installation: &Installation) -> io::Result<bool> {
        let Some(exec) = exec_value(&installation.executable) else {
            return Ok(false);
        };
        let exec = format!("Exec={exec} %u");
        let current = match fs::read_to_string(&self.path) {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !current.lines().any(|line| line == exec) {
            return Ok(false);
        }
        let packaged = fs::read_to_string(
            installation
                .root
                .join("share/applications")
                .join(format!("{}.desktop", self.id)),
        )?;
        let icon = installation
            .root
            .join("share/icons/hicolor/scalable/apps")
            .join(format!("{}.svg", self.id));
        let Some(icon) = icon.to_str() else {
            return Ok(false);
        };
        let icon = format!("Icon={}", icon.replace('\\', "\\\\"));
        let entry: String = packaged
            .lines()
            .map(|line| {
                let line = if line.starts_with("Exec=") {
                    exec.as_str()
                } else if line.starts_with("Icon=") {
                    icon.as_str()
                } else {
                    line
                };
                format!("{line}\n")
            })
            .collect();
        if entry == current {
            return Ok(false);
        }
        let Some(directory) = self.path.parent() else {
            return Ok(false);
        };
        // Desktop shells watch the directory, so the entry is replaced whole.
        let staged = directory.join(format!(".{}.desktop.{}", self.id, std::process::id()));
        fs::write(&staged, entry)?;
        fs::rename(&staged, &self.path).inspect_err(|_| {
            let _ = fs::remove_file(&staged);
        })?;
        Ok(true)
    }

    /// Updates the MIME cache that maps the `spaceterm:` scheme to the entry, as the install
    /// script does, where the desktop provides the tool.
    fn refresh_database(&self) {
        use std::process::{Command, Stdio};
        if let Some(directory) = self.path.parent() {
            let _ = Command::new("update-desktop-database")
                .arg(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// The install script's Exec value for `executable`: Desktop Entry Exec quoting, then string
/// value escaping. A path that is not UTF-8 has no entry to match.
fn exec_value(executable: &Path) -> Option<String> {
    let mut value = String::from("\"");
    for character in executable.to_str()?.chars() {
        match character {
            '\\' => value.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                value.push_str("\\\\");
                value.push(character);
            }
            character => value.push(character),
        }
    }
    value.push('"');
    Some(value)
}

/// Unpacks the regular files and directories under `root/` into `destination`.
///
/// Links, devices, absolute paths, and parent components are refused, so every entry lands
/// inside the freshly created destination. The unpacked stream and entry count are bounded.
fn unpack(archive: &[u8], root: &str, destination: &Path) -> io::Result<()> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    fs::DirBuilder::new().mode(0o755).create(destination)?;
    let unpacked = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED_BYTES);
    let mut archive = tar::Archive::new(unpacked);
    for (index, entry) in archive.entries()?.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(invalid());
        }
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        if components.next() != Some(Component::Normal(OsStr::new(root))) {
            return Err(invalid());
        }
        let relative = components.as_path().to_path_buf();
        if !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(invalid());
        }
        let target = destination.join(&relative);
        let executable = entry.header().mode()? & 0o111 != 0;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if !relative.as_os_str().is_empty() {
                    fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o755)
                        .create(&target)?;
                }
            }
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o755)
                        .create(parent)?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(if executable { 0o755 } else { 0o644 })
                    .open(&target)?;
                io::copy(&mut entry, &mut file)?;
                file.flush()?;
            }
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::release_feed::testing::{MINIMUM, ReleaseSigner};
    use std::collections::HashMap;

    const ROOT: &str = "spaceterm";

    /// A uniquely named directory removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "spaceterm-updates-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _directory: Scratch,
        installation: Installation,
    }

    fn installed(contents: &str) -> Fixture {
        let directory = Scratch::new();
        let root = directory.path().join("lib").join("spaceterm");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("share").join("spaceterm")).unwrap();
        fs::write(root.join("bin").join("spaceterm-desktop"), contents).unwrap();
        let installation =
            Installation::locate(&root.join("bin").join("spaceterm-desktop")).unwrap();
        Fixture {
            _directory: directory,
            installation,
        }
    }

    fn installed_contents(installation: &Installation) -> String {
        fs::read_to_string(&installation.executable).unwrap()
    }

    fn archive(entries: &[(&str, tar::EntryType, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, kind, contents, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(*mode);
            header.set_size(contents.len() as u64);
            if *kind == tar::EntryType::Symlink {
                header.set_link_name("/etc").unwrap();
            }
            // Bypass the builder's path checks so hostile names reach the reader.
            let name = header.as_old_mut().name.as_mut();
            name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn release(binary: &[u8]) -> Vec<u8> {
        archive(&[
            ("spaceterm/", tar::EntryType::Directory, b"", 0o755),
            ("spaceterm/bin/", tar::EntryType::Directory, b"", 0o755),
            (
                "spaceterm/bin/spaceterm-desktop",
                tar::EntryType::Regular,
                binary,
                0o755,
            ),
            (
                "spaceterm/share/spaceterm/",
                tar::EntryType::Directory,
                b"",
                0o755,
            ),
            (
                "spaceterm/share/spaceterm/notes.txt",
                tar::EntryType::Regular,
                b"notes",
                0o644,
            ),
        ])
    }

    #[test]
    fn linux_installation_should_be_located_from_its_installed_executable() {
        let fixture = installed("old");
        let root = fixture.installation.root.clone();
        assert_eq!(
            fixture.installation.executable,
            root.join("bin/spaceterm-desktop")
        );
        assert_eq!(
            fixture.installation.staging,
            root.parent().unwrap().join(".spaceterm.update")
        );
        // A source build runs from Cargo's target directory, which is not an installation.
        assert!(Installation::locate(Path::new("/tmp/target/debug/spaceterm-desktop")).is_none());
    }

    #[test]
    fn linux_unpack_should_keep_release_files_and_their_modes() {
        let directory = Scratch::new();
        let destination = directory.path().join("tree");
        unpack(&release(b"new"), ROOT, &destination).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        let executable = fs::metadata(destination.join("bin/spaceterm-desktop")).unwrap();
        assert_eq!(executable.permissions().mode() & 0o777, 0o755);
        assert_eq!(
            fs::read(destination.join("share/spaceterm/notes.txt")).unwrap(),
            b"notes"
        );
    }

    #[test]
    fn linux_unpack_should_refuse_entries_outside_the_release_root() {
        for entries in [
            vec![(
                "spaceterm/../escape",
                tar::EntryType::Regular,
                &b"x"[..],
                0o644,
            )],
            vec![("/etc/escape", tar::EntryType::Regular, &b"x"[..], 0o644)],
            vec![(
                "other/bin/spaceterm-desktop",
                tar::EntryType::Regular,
                &b"x"[..],
                0o755,
            )],
            vec![("spaceterm/link", tar::EntryType::Symlink, &b""[..], 0o777)],
        ] {
            let directory = Scratch::new();
            let destination = directory.path().join("tree");
            assert!(unpack(&archive(&entries), ROOT, &destination).is_err());
            assert!(!directory.path().join("escape").exists());
        }
    }

    #[test]
    fn linux_replace_should_exchange_the_staged_tree_with_the_installation() {
        let fixture = installed("old");
        let installation = &fixture.installation;
        installation.create_staging().unwrap();
        unpack(&release(b"new"), ROOT, &installation.staged_tree()).unwrap();
        installation.replace().unwrap();
        assert_eq!(installed_contents(installation), "new");
        // The running process keeps the replaced tree until the next launch discards it.
        assert_eq!(
            fs::read_to_string(installation.staged_tree().join("bin/spaceterm-desktop")).unwrap(),
            "old"
        );
        installation.discard_staging().unwrap();
        assert!(!installation.staging.exists());
    }

    fn without_exchange(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    #[test]
    fn linux_replace_without_exchange_should_keep_the_installation() {
        let mut fixture = installed("old");
        fixture.installation.exchange = without_exchange;
        let installation = &fixture.installation;
        installation.create_staging().unwrap();
        unpack(&release(b"new"), ROOT, &installation.staged_tree()).unwrap();
        assert_eq!(
            installation.replace().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(installed_contents(installation), "old");
        assert_eq!(
            installation_failure(&io::ErrorKind::Unsupported.into()),
            UpdateError::ReadOnly
        );
    }

    #[test]
    fn linux_full_disk_should_ask_for_space_instead_of_failing_verification() {
        for full in [libc::ENOSPC, libc::EDQUOT] {
            let error = io::Error::from_raw_os_error(full);
            assert_eq!(installation_failure(&error), UpdateError::DiskFull);
            assert_eq!(unpack_failure(&error), UpdateError::DiskFull);
        }
        assert_eq!(
            unpack_failure(&io::ErrorKind::InvalidData.into()),
            UpdateError::Verification
        );
        assert_eq!(
            unpack_failure(&io::Error::from_raw_os_error(libc::EACCES)),
            UpdateError::Installation
        );
    }

    #[test]
    fn linux_relaunch_should_follow_only_the_approved_exit() {
        let relaunch = Relaunch::default();
        relaunch.request(PathBuf::from("/opt/spaceterm/bin/spaceterm-desktop"));
        // The approved relaunch failed, then the person quit SpaceTerm.
        relaunch.exiting(false);
        assert_eq!(relaunch.take(), None);

        relaunch.request(PathBuf::from("/opt/spaceterm/bin/spaceterm-desktop"));
        relaunch.exiting(true);
        assert_eq!(
            relaunch.take(),
            Some(PathBuf::from("/opt/spaceterm/bin/spaceterm-desktop"))
        );
        assert_eq!(relaunch.take(), None);
    }

    const DESKTOP_ID: &str = "com.peblon.spaceterm";

    /// Writes the release's packaged entry into the installation and the install script's entry
    /// for it into `data_home`, which launches `executable`.
    fn desktop_entries(fixture: &Fixture, data_home: &Path, executable: &Path, packaged: &str) {
        let root = &fixture.installation.root;
        fs::create_dir_all(root.join("share/applications")).unwrap();
        fs::write(
            root.join(format!("share/applications/{DESKTOP_ID}.desktop")),
            packaged,
        )
        .unwrap();
        fs::create_dir_all(data_home.join("applications")).unwrap();
        fs::write(
            data_home.join(format!("applications/{DESKTOP_ID}.desktop")),
            format!(
                "[Desktop Entry]\nName=SpaceTerm\nExec={} %u\nIcon=old\n",
                exec_value(executable).unwrap()
            ),
        )
        .unwrap();
    }

    #[test]
    fn linux_update_should_rewrite_the_desktop_entry_from_the_installed_release() {
        let fixture = installed("old");
        let data_home = fixture._directory.path().join("data");
        let installation = &fixture.installation;
        desktop_entries(
            &fixture,
            &data_home,
            &installation.executable,
            "[Desktop Entry]\nName=SpaceTerm\nExec=spaceterm-desktop %u\nMimeType=x-scheme-handler/spaceterm;\nIcon=com.peblon.spaceterm\n",
        );
        let entry = DesktopEntry::under(&data_home, DESKTOP_ID);
        assert!(entry.refresh(installation).unwrap());
        let root = installation.root.to_str().unwrap();
        assert_eq!(
            fs::read_to_string(&entry.path).unwrap(),
            format!(
                "[Desktop Entry]\nName=SpaceTerm\nExec=\"{root}/bin/spaceterm-desktop\" %u\nMimeType=x-scheme-handler/spaceterm;\nIcon={root}/share/icons/hicolor/scalable/apps/{DESKTOP_ID}.svg\n"
            )
        );
        // The same release writes nothing again, and leaves no staged entry behind.
        assert!(!entry.refresh(installation).unwrap());
        assert_eq!(
            fs::read_dir(data_home.join("applications"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn linux_update_should_leave_an_entry_for_another_installation_or_none() {
        let fixture = installed("old");
        let data_home = fixture._directory.path().join("data");
        let packaged = "[Desktop Entry]\nExec=spaceterm-desktop %u\nName=New\n";
        desktop_entries(
            &fixture,
            &data_home,
            Path::new("/elsewhere/spaceterm/bin/spaceterm-desktop"),
            packaged,
        );
        let entry = DesktopEntry::under(&data_home, DESKTOP_ID);
        let before = fs::read_to_string(&entry.path).unwrap();
        assert!(!entry.refresh(&fixture.installation).unwrap());
        assert_eq!(fs::read_to_string(&entry.path).unwrap(), before);

        fs::remove_file(&entry.path).unwrap();
        assert!(!entry.refresh(&fixture.installation).unwrap());
        assert!(!entry.path.exists(), "a removed entry stays removed");
    }

    #[test]
    fn linux_exec_value_should_quote_as_the_install_script_does() {
        assert_eq!(
            exec_value(Path::new(r#"/home/a b/x\"$`y"#)).unwrap(),
            r#""/home/a b/x\\\\\\"\\$\\`y""#
        );
    }

    /// Serves fixed responses for the feed, its signature, and the archive.
    #[derive(Default)]
    struct ServedRelease(HashMap<String, Vec<u8>>);

    impl UpdateTransport for ServedRelease {
        fn get(&self, url: &str, limit: usize) -> io::Result<Vec<u8>> {
            let body = self.0.get(url).ok_or(io::ErrorKind::NotFound)?;
            if body.len() > limit {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(body.clone())
        }

        fn open(&self, url: &str, _: u64) -> io::Result<Box<dyn Read + Send>> {
            let body = self.0.get(url).ok_or(io::ErrorKind::NotFound)?;
            Ok(Box::new(io::Cursor::new(body.clone())))
        }
    }

    fn served(signer: &ReleaseSigner, version: &str, archive: &[u8]) -> ServedRelease {
        published(signer, &[(version, "0.0.0", MINIMUM, archive)])
    }

    /// Serves `releases`, as `(version, floor, minimum system, archive)` and newest first, at
    /// their versions, and the first as the newest release.
    fn published(
        signer: &ReleaseSigner,
        releases: &[(&str, &str, SystemVersion, &[u8])],
    ) -> ServedRelease {
        let target = FeedTarget::DESKTOP_LINUX;
        let mut served = HashMap::new();
        for (index, &(version, floor, minimum, archive)) in releases.iter().enumerate() {
            let (feed, signature) = signer.floored_feed(
                target,
                version,
                floor,
                minimum,
                crate::updates::now() - 60,
                archive,
            );
            let parsed = spaceterm_core::update::facts::Version::parse(version).unwrap();
            if index == 0 {
                served.insert(target.feed_url(None), feed.clone());
                served.insert(target.feed_signature_url(None), signature.clone());
            }
            served.insert(target.feed_url(Some(&parsed)), feed);
            served.insert(target.feed_signature_url(Some(&parsed)), signature);
            served.insert(
                format!(
                    "https://github.com/sadiksaifi/SpaceTerm/releases/download/v{version}/spaceterm-desktop-{version}-linux-x86_64.tar.gz"
                ),
                archive.to_vec(),
            );
        }
        ServedRelease(served)
    }

    fn shared(
        fixture: &Fixture,
        transport: ServedRelease,
        trust: UpdateTrust,
    ) -> (Arc<Shared>, async_channel::Receiver<UpdateEvent>) {
        let (events, receiver) = async_channel::unbounded();
        let shared = Arc::new(Shared {
            transport: Arc::new(transport),
            trust,
            target: FeedTarget::DESKTOP_LINUX,
            installation: fixture.installation.clone(),
            archive_root: ROOT,
            desktop_entry: None,
            current_version: "1.0.0",
            system: || Some(MINIMUM),
            events,
            cycle: Mutex::new(Cycle::default()),
        });
        (shared, receiver)
    }

    fn drained(receiver: &async_channel::Receiver<UpdateEvent>) -> Vec<String> {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|event| match event {
                UpdateEvent::Downloading { .. } => "Downloading".to_owned(),
                UpdateEvent::ReleaseMetadata { .. } => "ReleaseMetadata".to_owned(),
                event => format!("{event:?}"),
            })
            .fold(Vec::new(), |mut events, event| {
                if events.last() != Some(&event) {
                    events.push(event);
                }
                events
            })
    }

    #[test]
    fn linux_signed_release_should_be_found_downloaded_and_installed() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let archive = release(b"new");
        let (shared, receiver) =
            shared(&fixture, served(&signer, "1.1.0", &archive), signer.trust());
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(
            drained(&receiver),
            ["ReleaseMetadata", "Available(\"1.1.0\")"]
        );
        let Phase::Available(feed) = std::mem::take(&mut shared.lock().phase) else {
            panic!("the signed release should be available");
        };
        shared.lock().phase = Phase::Downloading;
        shared.download(&feed, generation);
        assert_eq!(drained(&receiver), ["Downloading", "Verifying", "Ready"]);
        fixture.installation.replace().unwrap();
        assert_eq!(installed_contents(&fixture.installation), "new");
    }

    #[test]
    fn linux_filesystem_without_exchange_should_refuse_the_update_before_downloading() {
        let mut fixture = installed("old");
        fixture.installation.exchange = without_exchange;
        let signer = ReleaseSigner::new(3);
        let (shared, receiver) = shared(
            &fixture,
            served(&signer, "1.1.0", &release(b"new")),
            signer.trust(),
        );
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        let Phase::Available(feed) = std::mem::take(&mut shared.lock().phase) else {
            panic!("the signed release should be available");
        };
        drained(&receiver);
        shared.lock().phase = Phase::Downloading;
        shared.download(&feed, generation);
        // The failure arrives before any download progress, and the install script is the remedy.
        assert_eq!(drained(&receiver), ["Failed(ReadOnly)", "Finished"]);
        assert!(matches!(shared.lock().phase, Phase::Idle));
        assert!(!fixture.installation.staged_tree().exists());
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    #[test]
    fn linux_current_release_should_be_up_to_date() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let (shared, receiver) = shared(
            &fixture,
            served(&signer, "1.0.0", &release(b"same")),
            signer.trust(),
        );
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(drained(&receiver), ["UpToDate", "Finished"]);
    }

    #[test]
    fn linux_desktop_below_the_newest_floor_should_install_the_release_at_that_floor() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let transport = published(
            &signer,
            &[
                ("1.2.0", "1.1.0", MINIMUM, &release(b"newest")),
                ("1.1.0", "1.0.0", MINIMUM, &release(b"stone")),
            ],
        );
        let (shared, receiver) = shared(&fixture, transport, signer.trust());
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(
            drained(&receiver),
            ["ReleaseMetadata", "Available(\"1.1.0\")"]
        );
        let Phase::Available(feed) = std::mem::take(&mut shared.lock().phase) else {
            panic!("the Stepping Stone should be available");
        };
        shared.lock().phase = Phase::Downloading;
        shared.download(&feed, generation);
        assert_eq!(drained(&receiver), ["Downloading", "Verifying", "Ready"]);
        fixture.installation.replace().unwrap();
        assert_eq!(installed_contents(&fixture.installation), "stone");
    }

    #[test]
    fn linux_floors_that_lead_to_no_release_should_fail_verification() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let transport = published(
            &signer,
            &[
                ("1.2.0", "1.1.0", MINIMUM, &release(b"newest")),
                ("1.1.0", "1.1.0", MINIMUM, &release(b"stone")),
            ],
        );
        let (shared, receiver) = shared(&fixture, transport, signer.trust());
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(drained(&receiver), ["Failed(Verification)", "Finished"]);
    }

    #[test]
    fn linux_stepping_stone_for_a_newer_system_should_say_the_system_is_too_old() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let newer = SystemVersion {
            major: 2,
            minor: 39,
        };
        let transport = published(
            &signer,
            &[
                ("1.2.0", "1.1.0", MINIMUM, &release(b"newest")),
                ("1.1.0", "1.0.0", newer, &release(b"stone")),
            ],
        );
        let (shared, receiver) = shared(&fixture, transport, signer.trust());
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(drained(&receiver), ["Failed(SystemTooOld)", "Finished"]);
        assert!(matches!(shared.lock().phase, Phase::Idle));
    }

    #[test]
    fn linux_release_signed_by_another_key_should_fail_verification() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let (shared, receiver) = shared(
            &fixture,
            served(&signer, "1.1.0", &release(b"new")),
            ReleaseSigner::new(4).trust(),
        );
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        assert_eq!(drained(&receiver), ["Failed(Verification)", "Finished"]);
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    #[test]
    fn linux_archive_that_differs_from_its_signature_should_never_be_staged() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let mut transport = served(&signer, "1.1.0", &release(b"new"));
        let url = "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.1.0/spaceterm-desktop-1.1.0-linux-x86_64.tar.gz";
        let mut tampered = release(b"evil");
        tampered.resize(transport.0[url].len(), 0);
        transport.0.insert(url.to_owned(), tampered);
        let (shared, receiver) = shared(&fixture, transport, signer.trust());
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        let Phase::Available(feed) = std::mem::take(&mut shared.lock().phase) else {
            panic!("the signed feed should verify");
        };
        drained(&receiver);
        shared.lock().phase = Phase::Downloading;
        shared.download(&feed, generation);
        assert_eq!(
            drained(&receiver),
            [
                "Downloading",
                "Verifying",
                "Failed(Verification)",
                "Finished"
            ]
        );
        assert!(!fixture.installation.staged_tree().join("bin").exists());
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    #[test]
    fn linux_worker_of_a_cancelled_download_should_stay_silent_and_stage_nothing() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let (shared, receiver) = shared(
            &fixture,
            served(&signer, "1.1.0", &release(b"new")),
            signer.trust(),
        );
        let generation = shared
            .begin(Phase::Idle, Phase::Checking, UpdateError::Check)
            .unwrap();
        shared.check(generation);
        let Phase::Available(feed) = std::mem::take(&mut shared.lock().phase) else {
            panic!("the signed release should be available");
        };
        drained(&receiver);
        {
            let mut cycle = shared.lock();
            cycle.phase = Phase::Downloading;
            cycle.reset();
        }
        shared.download(&feed, generation);
        assert!(drained(&receiver).is_empty());
        assert!(matches!(shared.lock().phase, Phase::Idle));
        assert!(!fixture.installation.staging.exists());
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    #[test]
    fn linux_read_only_installation_should_report_read_only_without_downloading() {
        use std::os::unix::fs::PermissionsExt as _;
        // Root bypasses permission bits, so only an unprivileged run observes the refusal.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let updates = adapter(
            &fixture,
            served(&signer, "1.1.0", &release(b"new")),
            signer.trust(),
        );
        let (events, receiver) = async_channel::unbounded();
        updates.start(events).unwrap();
        let shared = updates.shared().unwrap();
        // A test build has no stable version, so make the signed release available directly.
        shared.lock().phase = Phase::Available(shared.fetch_feed().unwrap());
        let parent = fixture.installation.root.parent().unwrap().to_path_buf();
        assert!(fixture.installation.writable());
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let result = updates.download();
        let writable = fixture.installation.writable();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result, Ok(()));
        assert!(!writable);
        assert_eq!(drained(&receiver), ["Failed(ReadOnly)", "Finished"]);
        assert!(matches!(shared.lock().phase, Phase::Idle));
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    fn adapter(
        fixture: &Fixture,
        transport: impl UpdateTransport + 'static,
        trust: UpdateTrust,
    ) -> LinuxUpdates {
        LinuxUpdates {
            dependencies: Some(Dependencies {
                transport: Arc::new(transport),
                trust,
                target: FeedTarget::DESKTOP_LINUX,
                installation: fixture.installation.clone(),
                archive_root: ROOT,
                desktop_entry: None,
            }),
            shared: RefCell::new(None),
            history: crate::platform::update_history_file::UpdateHistoryFile::new(None),
            relaunch: Relaunch::default(),
            request_quit: Box::new(|| {}),
        }
    }

    /// Serves the release, but the archive body blocks until the test drops the gate.
    struct StalledArchive {
        release: ServedRelease,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl UpdateTransport for StalledArchive {
        fn get(&self, url: &str, limit: usize) -> io::Result<Vec<u8>> {
            self.release.get(url, limit)
        }

        fn open(&self, _: &str, _: u64) -> io::Result<Box<dyn Read + Send>> {
            let gate = self
                .gate
                .lock()
                .unwrap()
                .take()
                .ok_or(io::ErrorKind::NotFound)?;
            Ok(Box::new(Stalled(gate)))
        }
    }

    struct Stalled(std::sync::mpsc::Receiver<()>);

    impl Read for Stalled {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    #[test]
    fn linux_cancel_should_finish_while_the_archive_download_stalls() {
        let fixture = installed("old");
        let signer = ReleaseSigner::new(3);
        let (gate, stalled) = std::sync::mpsc::channel();
        let updates = adapter(
            &fixture,
            StalledArchive {
                release: served(&signer, "1.1.0", &release(b"new")),
                gate: Mutex::new(Some(stalled)),
            },
            signer.trust(),
        );
        let (events, receiver) = async_channel::unbounded();
        updates.start(events).unwrap();
        let shared = updates.shared().unwrap();
        shared.lock().phase = Phase::Available(shared.fetch_feed().unwrap());
        updates.download().unwrap();
        // The worker reports the download, then blocks on the archive body.
        assert!(matches!(
            receiver.recv_blocking(),
            Ok(UpdateEvent::Downloading { received: 0, .. })
        ));
        updates.cancel();
        assert!(matches!(receiver.try_recv(), Ok(UpdateEvent::Finished)));
        assert!(matches!(shared.lock().phase, Phase::Idle));
        assert!(shared.lock().claim.is_none());
        drop(gate);
        assert_eq!(installed_contents(&fixture.installation), "old");
    }

    #[test]
    fn linux_installation_claim_should_admit_one_update_at_a_time() {
        let fixture = installed("old");
        let installation = &fixture.installation;
        let claim = installation.claim().unwrap();
        assert_eq!(
            installation.claim().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        // Cleanup leaves the staging of an update that holds the claim.
        installation.create_staging().unwrap();
        assert!(installation.discard_unclaimed_staging().is_err());
        assert!(installation.staging.exists());
        drop(claim);
        installation.discard_unclaimed_staging().unwrap();
        assert!(!installation.staging.exists());
    }

    #[test]
    fn release_appcast_enclosure_should_pass_desktop_admission() {
        let Some(directory) = std::env::var_os("SPACETERM_RELEASE_FIXTURE").map(PathBuf::from)
        else {
            return;
        };
        let url = fs::read_to_string(directory.join("appcast-enclosure")).unwrap();
        assert!(crate::updates::allows_release(
            "1.0.1", "1.0.1", &url, false
        ));
        assert!(!crate::updates::allows_release(
            "1.0.1",
            "1.0.1",
            &url.replace("macos-arm64", "darwin-arm64"),
            false
        ));
    }

    /// test:updater:linux signs a release with the release task and passes its directory here,
    /// so the updater verifies exactly what a release publishes. Without it there is nothing to
    /// verify.
    #[test]
    fn linux_release_task_assets_should_verify_and_install() {
        let Some(directory) = std::env::var_os("SPACETERM_RELEASE_FIXTURE").map(PathBuf::from)
        else {
            return;
        };
        let trust =
            UpdateTrust::parse(&fs::read(directory.join("update-trust.json")).unwrap()).unwrap();
        let target = FeedTarget::DESKTOP_LINUX;
        let feed = ReleaseFeed::next(
            &trust,
            target,
            "dev.0123456789ab",
            crate::updates::now(),
            |url, _| Ok(fs::read(directory.join(url.rsplit('/').next().unwrap())).unwrap()),
        )
        .unwrap();
        let name = feed.archive.url.rsplit('/').next().unwrap();
        let archive = fs::read(directory.join(name)).unwrap();
        assert_eq!(feed.archive.verify(&trust, &archive), Ok(()));
        let mut tampered = archive.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            feed.archive.verify(&trust, &tampered),
            Err(UpdateError::Verification)
        );
        let fixture = installed("old");
        fixture.installation.create_staging().unwrap();
        unpack(&archive, ROOT, &fixture.installation.staged_tree()).unwrap();
        fixture.installation.replace().unwrap();
        assert_eq!(
            fs::read(&fixture.installation.executable).unwrap(),
            fs::read(directory.join("executable")).unwrap()
        );
        fs::write(directory.join("verified"), feed.version).unwrap();
    }
}
