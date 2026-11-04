//! Leaving the disk image SpaceTerm was opened from.
//!
//! SpaceTerm installs SpaceTerm Core and updates itself only from Applications, so a SpaceTerm
//! opened from its mounted DMG asks once, before any window opens, to move itself there. Moving
//! is the person's explicit choice: it copies the app to Applications, or to the account's own
//! Applications folder when the shared one is not writable, then opens the copy and quits.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use core_foundation::base::TCFType as _;
use core_foundation::error::CFErrorRef;
use core_foundation::url::{CFURL, CFURLRef};
use objc2::MainThreadMarker;
use objc2_app_kit::NSAlertFirstButtonReturn;
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
    let alert = application_alert(AlertSeverity::Standard, mtm);
    alert.setMessageText(ns_string!("Move SpaceTerm to Applications?"));
    alert.setInformativeText(ns_string!(
        "SpaceTerm runs from Applications so it can install SpaceTerm Core and update itself."
    ));
    alert.addButtonWithTitle(ns_string!("Move to Applications"));
    alert.addButtonWithTitle(ns_string!("Quit"));
    if run_modal_alert(&alert, AlertActivation::FORCEFUL, |_| {}) != NSAlertFirstButtonReturn {
        return true;
    }
    if move_and_reopen(&original).is_none() {
        let failed = application_alert(AlertSeverity::Standard, mtm);
        failed.setMessageText(ns_string!("SpaceTerm couldn\u{2019}t move to Applications"));
        failed.setInformativeText(&NSString::from_str(
            "Drag SpaceTerm to Applications in Finder, then open it there.",
        ));
        failed.addButtonWithTitle(ns_string!("Quit"));
        run_modal_alert(&failed, AlertActivation::FORCEFUL, |_| {});
    }
    true
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

/// Copies the app at `original` into Applications, replacing an older copy there by moving it
/// to the Trash, and opens the copy once this process exits.
fn move_and_reopen(original: &Path) -> Option<()> {
    let name = original.file_name()?;
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let shared = Path::new("/Applications");
    let folder = if writable(shared) {
        shared.to_owned()
    } else {
        let personal = home.join("Applications");
        std::fs::create_dir_all(&personal).ok()?;
        personal
    };
    let destination = folder.join(name);
    if destination.symlink_metadata().is_ok() {
        let trash = home.join(".Trash");
        let mut trashed = trash.join(name);
        let mut copy = 1;
        while trashed.symlink_metadata().is_ok() {
            copy += 1;
            let mut numbered = Path::new(name).file_stem()?.to_owned();
            numbered.push(format!(" {copy}.app"));
            trashed = trash.join(numbered);
        }
        std::fs::rename(&destination, &trashed).ok()?;
    }
    // ditto keeps the bundle's code signature, extended attributes and symbolic links.
    run(Command::new("/usr/bin/ditto")
        .arg(original)
        .arg(&destination))?;
    // The person already opened this copy once, so Gatekeeper need not ask again or run the
    // moved app from a translocated copy.
    let _ = run(Command::new("/usr/bin/xattr")
        .args(["-d", "-r", "com.apple.quarantine"])
        .arg(&destination));
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
    Some(())
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
