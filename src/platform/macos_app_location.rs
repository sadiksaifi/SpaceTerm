//! Leaving the disk image SpaceTerm was opened from.
//!
//! SpaceTerm installs SpaceTerm Core and updates itself only from Applications, so a SpaceTerm
//! opened from its mounted DMG asks once, before any window opens, to move itself there. Moving
//! is the person's explicit choice: it copies the app to Applications, or to the account's own
//! Applications folder when the shared one is not writable, then opens the copy and quits.
//! When another SpaceTerm of this build already runs, it comes forward instead. SpaceTerm checks
//! for one before the prompt, after it, and just before the copy replaces the installed app,
//! since the person can open the installed app at any time meanwhile.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use core_foundation::base::TCFType as _;
use core_foundation::error::CFErrorRef;
use core_foundation::url::{CFURL, CFURLRef};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationOptions, NSRunningApplication,
};
use objc2_foundation::{NSBundle, NSString, ns_string};

use super::macos_alert::{AlertActivation, AlertSeverity, application_alert, run_modal_alert};

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTranslocateIsTranslocatedURL(
        path: CFURLRef,
        translocated: *mut bool,
        error: *mut CFErrorRef,
    ) -> bool;
    fn SecTranslocateCreateOriginalPathForURL(
        translocated: CFURLRef,
        error: *mut CFErrorRef,
    ) -> CFURLRef;
}

/// Offers to move SpaceTerm to Applications when it runs from a disk image. Returns whether
/// SpaceTerm must quit now: the person chose Quit, or the moved copy opens in its place.
pub(crate) fn leave_disk_image() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let bundle = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    let original = original_location(&bundle);
    if !read_only_volume(&original) {
        return false;
    }
    leave(&Desktop { mtm, original });
    true
}

/// The steps of leaving the disk image. Tests script them.
trait Leaving {
    type Running;
    /// Another running SpaceTerm of this build, read afresh each time.
    fn running_copy(&self) -> Option<Self::Running>;
    /// Brings `running` forward in this SpaceTerm's place.
    fn activate(&self, running: &Self::Running);
    /// Asks the person. True when they chose Move to Applications.
    fn ask(&self) -> bool;
    /// Moves this app into Applications, unless another SpaceTerm starts first. `None` when the
    /// move failed.
    fn move_and_reopen(&self) -> Option<Installation>;
    /// Says the move failed and how to move SpaceTerm by hand.
    fn report_failure(&self);
}

/// Asks to move SpaceTerm to Applications and moves it. A SpaceTerm that already runs, such as
/// the one in Applications, can't be replaced, and the person gets the SpaceTerm they asked
/// for. It is checked before the prompt, after it, and just before the copy takes the installed
/// app's place, since the person can open the installed app while the prompt is open or the copy
/// runs.
fn leave<L: Leaving>(steps: &L) {
    let forward = |steps: &L| {
        steps
            .running_copy()
            .map(|running| steps.activate(&running))
            .is_some()
    };
    if forward(steps) || !steps.ask() || forward(steps) {
        return;
    }
    match steps.move_and_reopen() {
        Some(Installation::Installed(_)) => {}
        // It opened while the copy ran, so the copy was discarded and the installed app stays.
        Some(Installation::Running) => {
            forward(steps);
        }
        None => steps.report_failure(),
    }
}

/// The disk image's own SpaceTerm, on the event thread.
struct Desktop {
    mtm: MainThreadMarker,
    original: PathBuf,
}

impl Leaving for Desktop {
    type Running = Retained<NSRunningApplication>;

    fn running_copy(&self) -> Option<Self::Running> {
        running_copy()
    }

    fn activate(&self, running: &Self::Running) {
        NSApplication::sharedApplication(self.mtm).yieldActivationToApplication(running);
        running.activateFromApplication_options(
            &NSRunningApplication::currentApplication(),
            NSApplicationActivationOptions::ActivateAllWindows,
        );
    }

    fn ask(&self) -> bool {
        let alert = application_alert(AlertSeverity::Standard, self.mtm);
        alert.setMessageText(ns_string!("Move SpaceTerm to Applications?"));
        alert.setInformativeText(ns_string!(
            "SpaceTerm runs from Applications so it can install SpaceTerm Core and update itself."
        ));
        alert.addButtonWithTitle(ns_string!("Move to Applications"));
        alert.addButtonWithTitle(ns_string!("Quit"));
        run_modal_alert(&alert, AlertActivation::FORCEFUL, |_| {}) == NSAlertFirstButtonReturn
    }

    fn move_and_reopen(&self) -> Option<Installation> {
        move_and_reopen(&self.original)
    }

    fn report_failure(&self) {
        let failed = application_alert(AlertSeverity::Standard, self.mtm);
        failed.setMessageText(ns_string!("SpaceTerm couldn\u{2019}t move to Applications"));
        failed.setInformativeText(&NSString::from_str(
            "Drag SpaceTerm to Applications in Finder, then open it there.",
        ));
        failed.addButtonWithTitle(ns_string!("Quit"));
        run_modal_alert(&failed, AlertActivation::FORCEFUL, |_| {});
    }
}

/// Another running SpaceTerm of this build, read afresh each time.
fn running_copy() -> Option<Retained<NSRunningApplication>> {
    let identifier = NSBundle::mainBundle().bundleIdentifier()?;
    let current = NSRunningApplication::currentApplication().processIdentifier();
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
        .iter()
        .find(|app| app.processIdentifier() != current)
}

/// Where the person keeps the app: Gatekeeper may run an app from a disk image through a
/// read-only translocated copy, whose original is the disk image's own.
fn original_location(bundle: &Path) -> PathBuf {
    let Some(url) = CFURL::from_path(bundle, true) else {
        return bundle.to_owned();
    };
    let mut translocated = false;
    // SAFETY: `url` is a live file URL, `translocated` is writable, and a null error pointer
    // asks for no error object.
    let checked = unsafe {
        SecTranslocateIsTranslocatedURL(
            url.as_concrete_TypeRef(),
            &raw mut translocated,
            std::ptr::null_mut(),
        )
    };
    if !checked || !translocated {
        return bundle.to_owned();
    }
    // SAFETY: `url` names a translocated bundle. The function returns a URL this caller owns,
    // or null.
    let original = unsafe {
        SecTranslocateCreateOriginalPathForURL(url.as_concrete_TypeRef(), std::ptr::null_mut())
    };
    if original.is_null() {
        return bundle.to_owned();
    }
    // SAFETY: The URL came from a Create function, so this wrapper takes its one reference.
    let original = unsafe { CFURL::wrap_under_create_rule(original) };
    original.to_path().unwrap_or_else(|| bundle.to_owned())
}

/// Whether `path` sits on a read-only volume, as a mounted disk image does.
fn read_only_volume(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut volume = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `volume` is writable storage for one statfs record.
    if unsafe { libc::statfs(path.as_ptr(), volume.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs succeeded, so it filled the record.
    let volume = unsafe { volume.assume_init() };
    volume.f_flags & libc::MNT_RDONLY as u32 != 0
}

/// Copies the app at `original` into Applications and opens the copy once this process exits.
/// An older copy there goes to the Trash only after the new copy took its place. `None` when
/// the move failed.
fn move_and_reopen(original: &Path) -> Option<Installation> {
    let name = original.file_name()?.to_owned();
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let shared = Path::new("/Applications");
    let folder = if writable(shared) {
        shared.to_owned()
    } else {
        let personal = home.join("Applications");
        std::fs::create_dir_all(&personal).ok()?;
        personal
    };
    let destination = match install(
        original,
        &folder,
        |source, staging| {
            // ditto keeps the bundle's code signature, extended attributes and symbolic links.
            let copied = run(Command::new("/usr/bin/ditto").arg(source).arg(staging)).is_some();
            // The person already opened this copy once, so Gatekeeper need not ask again or run
            // the moved app from a translocated copy.
            if copied {
                let _ = run(Command::new("/usr/bin/xattr")
                    .args(["-d", "-r", "com.apple.quarantine"])
                    .arg(staging));
            }
            copied
        },
        || running_copy().is_some(),
        |previous| trash(previous, &name, &home.join(".Trash")),
    )? {
        Installation::Installed(destination) => destination,
        Installation::Running => return Some(Installation::Running),
    };
    Command::new("/bin/sh")
        .args([
            "-c",
            "while /bin/kill -0 \"$1\" 2>/dev/null; do /bin/sleep 0.2; done; /usr/bin/open \"$2\"",
            "sh",
        ])
        .arg(std::process::id().to_string())
        .arg(&destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    Some(Installation::Installed(destination))
}

/// What a completed [`install`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Installation {
    /// The copy took the installed app's place, at this path.
    Installed(PathBuf),
    /// Another SpaceTerm of this build started before the copy could take its place, so the copy
    /// was discarded and the installed app is unchanged.
    Running,
}

/// Puts a complete copy of the app at `source` into `folder` under the same name, and returns
/// where. `copy` fills a hidden staging path in `folder` first, so `folder` never holds a
/// partial app. `running` is asked last, just before anything in `folder` changes: while another
/// SpaceTerm runs, the staged copy goes and the installed app stays. Otherwise an app already
/// there moves aside, the staged copy takes its name, and only then does `discard` receive the
/// old app. Any failure puts the old app back and removes the staged copy.
fn install(
    source: &Path,
    folder: &Path,
    copy: impl FnOnce(&Path, &Path) -> bool,
    running: impl FnOnce() -> bool,
    discard: impl FnOnce(&Path),
) -> Option<Installation> {
    let name = source.file_name()?;
    let destination = folder.join(name);
    let hidden = |role: &str| {
        let mut hidden = std::ffi::OsString::from(".");
        hidden.push(name);
        hidden.push(format!(".{role}-{}", std::process::id()));
        folder.join(hidden)
    };
    let staging = hidden("staging");
    let _ = std::fs::remove_dir_all(&staging);
    if !copy(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return None;
    }
    if running() {
        let _ = std::fs::remove_dir_all(&staging);
        return Some(Installation::Running);
    }
    let previous = destination
        .symlink_metadata()
        .is_ok()
        .then(|| hidden("previous"));
    if let Some(previous) = &previous
        && std::fs::rename(&destination, previous).is_err()
    {
        let _ = std::fs::remove_dir_all(&staging);
        return None;
    }
    if std::fs::rename(&staging, &destination).is_err() {
        if let Some(previous) = &previous {
            let _ = std::fs::rename(previous, &destination);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return None;
    }
    if let Some(previous) = previous {
        discard(&previous);
    }
    Some(Installation::Installed(destination))
}

/// Moves the replaced app at `previous` into `trash` as `name`, numbered when the Trash already
/// holds one. An app the Trash cannot take is removed, so Applications keeps no hidden copy.
fn trash(previous: &Path, name: &std::ffi::OsStr, trash: &Path) {
    let mut trashed = trash.join(name);
    let mut copy = 1;
    while trashed.symlink_metadata().is_ok() {
        copy += 1;
        let Some(stem) = Path::new(name).file_stem() else {
            break;
        };
        let mut numbered = stem.to_owned();
        numbered.push(format!(" {copy}.app"));
        trashed = trash.join(numbered);
    }
    if std::fs::rename(previous, &trashed).is_err() {
        let _ = std::fs::remove_dir_all(previous);
    }
}

fn writable(directory: &Path) -> bool {
    CString::new(directory.as_os_str().as_bytes())
        // SAFETY: The path is NUL-terminated; access only reads it.
        .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
}

fn run(command: &mut Command) -> Option<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(std::process::ExitStatus::success)
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for one test, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "spaceterm-app-location-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An app bundle whose one file reads `contents`.
    fn app(at: &Path, contents: &str) {
        std::fs::create_dir_all(at.join("Contents")).unwrap();
        std::fs::write(at.join("Contents/Info.plist"), contents).unwrap();
    }

    fn contents(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap()
    }

    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Copies as ditto does, into a directory it creates.
    fn copy_tree(source: &Path, target: &Path) -> bool {
        Command::new("/bin/cp")
            .arg("-R")
            .arg(source)
            .arg(target)
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A copy that fails partway leaves the installed app as it was and nothing else behind.
    #[test]
    fn a_failed_copy_leaves_the_installed_app_untouched() {
        let disk_image = Folder::new();
        let applications = Folder::new();
        let source = disk_image.0.join("SpaceTerm.app");
        app(&source, "new");
        app(&applications.0.join("SpaceTerm.app"), "old");
        let mut discarded = Vec::new();

        let installed = install(
            &source,
            &applications.0,
            |_, staging| {
                std::fs::create_dir_all(staging.join("Contents")).unwrap();
                false
            },
            || false,
            |previous| discarded.push(previous.to_owned()),
        );

        assert_eq!(installed, None);
        assert_eq!(entries(&applications.0), ["SpaceTerm.app"]);
        assert_eq!(contents(&applications.0.join("SpaceTerm.app")), "old");
        assert!(discarded.is_empty());
    }

    /// The new copy takes the installed app's place whole, and the old app goes only after.
    #[test]
    fn a_complete_copy_replaces_the_installed_app_before_the_old_one_goes() {
        let disk_image = Folder::new();
        let applications = Folder::new();
        let source = disk_image.0.join("SpaceTerm.app");
        app(&source, "new");
        let destination = applications.0.join("SpaceTerm.app");
        app(&destination, "old");
        let mut discarded = Vec::new();

        let installed = install(
            &source,
            &applications.0,
            copy_tree,
            || false,
            |previous| {
                discarded.push((contents(previous), contents(&destination)));
                std::fs::remove_dir_all(previous).unwrap();
            },
        );

        assert_eq!(installed, Some(Installation::Installed(destination.clone())));
        assert_eq!(entries(&applications.0), ["SpaceTerm.app"]);
        assert_eq!(contents(&destination), "new");
        assert_eq!(discarded, [("old".to_owned(), "new".to_owned())]);
    }

    /// The installed SpaceTerm opened while the copy ran. Nothing in Applications changes: the
    /// staged copy goes, the installed app stays, and no old app is discarded.
    #[test]
    fn an_installed_app_that_opens_during_the_copy_keeps_its_place() {
        let disk_image = Folder::new();
        let applications = Folder::new();
        let source = disk_image.0.join("SpaceTerm.app");
        app(&source, "new");
        let destination = applications.0.join("SpaceTerm.app");
        app(&destination, "old");
        let copied = std::cell::Cell::new(false);
        let mut discarded = Vec::new();

        let installed = install(
            &source,
            &applications.0,
            |source, staging| {
                copied.set(copy_tree(source, staging));
                copied.get()
            },
            || {
                assert!(copied.get(), "the check runs after the copy, just before the swap");
                true
            },
            |previous| discarded.push(previous.to_owned()),
        );

        assert_eq!(installed, Some(Installation::Running));
        assert_eq!(entries(&applications.0), ["SpaceTerm.app"]);
        assert_eq!(contents(&destination), "old");
        assert!(discarded.is_empty());
    }

    /// Scripted steps: each running-copy read answers the next value, then none.
    struct Scripted {
        running: std::cell::RefCell<std::collections::VecDeque<bool>>,
        moved: Option<Installation>,
        steps: std::cell::RefCell<Vec<&'static str>>,
    }

    impl Scripted {
        fn new(running: impl IntoIterator<Item = bool>, moved: Option<Installation>) -> Self {
            Self {
                running: std::cell::RefCell::new(running.into_iter().collect()),
                moved,
                steps: std::cell::RefCell::default(),
            }
        }

        fn steps(&self) -> Vec<&'static str> {
            self.steps.borrow().clone()
        }
    }

    impl Leaving for Scripted {
        type Running = ();

        fn running_copy(&self) -> Option<()> {
            self.running
                .borrow_mut()
                .pop_front()
                .unwrap_or(false)
                .then_some(())
        }

        fn activate(&self, (): &()) {
            self.steps.borrow_mut().push("activate");
        }

        fn ask(&self) -> bool {
            self.steps.borrow_mut().push("ask");
            true
        }

        fn move_and_reopen(&self) -> Option<Installation> {
            self.steps.borrow_mut().push("move");
            self.moved.clone()
        }

        fn report_failure(&self) {
            self.steps.borrow_mut().push("report_failure");
        }
    }

    /// The person opened the installed SpaceTerm while the prompt was open. Choosing Move to
    /// Applications then brings it forward and moves nothing.
    #[test]
    fn an_installed_app_that_opens_during_the_prompt_comes_forward_instead() {
        let steps = Scripted::new([false, true], Some(Installation::Installed(PathBuf::new())));
        leave(&steps);
        assert_eq!(steps.steps(), ["ask", "activate"]);
    }

    /// The installed SpaceTerm opened while the copy ran, so the move discarded its copy. That
    /// SpaceTerm comes forward, and nothing reports a failure.
    #[test]
    fn an_installed_app_that_opens_during_the_copy_comes_forward() {
        let steps = Scripted::new([false, false, true], Some(Installation::Running));
        leave(&steps);
        assert_eq!(steps.steps(), ["ask", "move", "activate"]);
    }

    /// With no other SpaceTerm running, the move runs once, and only a failed one is reported.
    #[test]
    fn without_a_running_copy_the_move_runs_and_reports_only_failure() {
        let steps = Scripted::new([], Some(Installation::Installed(PathBuf::new())));
        leave(&steps);
        assert_eq!(steps.steps(), ["ask", "move"]);
        let steps = Scripted::new([], None);
        leave(&steps);
        assert_eq!(steps.steps(), ["ask", "move", "report_failure"]);
    }
}
