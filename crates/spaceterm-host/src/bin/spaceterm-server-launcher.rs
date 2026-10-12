//! Single-threaded one-shot production composition. Missing native authority is fatal.

use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    startup::{
        AccountBuildKey, BuildIdentity, SystemNativeStartupAuthority, UnavailableOneShotLauncher,
        launch_once,
    },
};
use std::{num::NonZeroU64, process::ExitCode};

fn arguments() -> Result<(AccountBuildKey, NonZeroU64), NativeFailure> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--account-uid")) {
        return Err(NativeFailure::InvalidRecord);
    }
    let uid = decimal(args.next())?;
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--registration-generation")) {
        return Err(NativeFailure::InvalidRecord);
    }
    let generation = NonZeroU64::new(decimal(args.next())?).ok_or(NativeFailure::InvalidRecord)?;
    if args.next().is_some() {
        return Err(NativeFailure::InvalidRecord);
    }
    let account_uid = u32::try_from(uid).map_err(|_| NativeFailure::InvalidRecord)?;
    if matches!(account_uid, 0 | u32::MAX) {
        return Err(NativeFailure::InvalidRecord);
    }
    // Release/preflight identity must come from the coordinator's authenticated build
    // identity integration. A CLI flag or runtime environment must never select it.
    Ok((
        AccountBuildKey {
            account_uid,
            build_identity: BuildIdentity::Development,
        },
        generation,
    ))
}
fn decimal(value: Option<std::ffi::OsString>) -> Result<u64, NativeFailure> {
    let value = value.ok_or(NativeFailure::InvalidRecord)?;
    let text = value.to_str().ok_or(NativeFailure::InvalidRecord)?;
    if text.is_empty()
        || text.len() > 20
        || text.starts_with('0')
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(NativeFailure::InvalidRecord);
    }
    text.parse().map_err(|_| NativeFailure::InvalidRecord)
}
fn run() -> Result<(), NativeFailure> {
    let (key, generation) = arguments()?;
    let authority = SystemNativeStartupAuthority;
    let mut launcher = UnavailableOneShotLauncher;
    #[cfg(target_os = "linux")]
    let credentials = spaceterm_host::boot_credentials::linux::SystemdBootCredentials;
    #[cfg(not(target_os = "linux"))]
    let credentials = spaceterm_host::boot_credentials::UnavailableBootCredentials;
    launch_once(key, generation, &authority, &credentials, &mut launcher)
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            // Only the stable classification crosses the process boundary.
            eprintln!("{failure}");
            ExitCode::from(failure as u8)
        }
    }
}
