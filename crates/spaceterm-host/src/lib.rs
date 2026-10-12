//! Headless native boundaries for SpaceTerm Server startup.
//!
//! Protected directory authority retains every ancestor handle on macOS and Linux.
//! [`root_protection::NativeRootProtection`] supplies macOS fixed-root filesystem
//! and ACL inspection. Linux requires a private, independently issued protection
//! lease covering selected-mount mapping and every inode's account-write-authorizing
//! aliases. Native query adapters and lease validation exist, but no production
//! issuer does. Uncontextualized Linux protection and fixed registration reads fail
//! closed. Optional newer mount queries do not raise the Linux 5.15/glibc 2.35/systemd
//! 249 baseline; old layouts without genuine provenance remain unavailable.
//! Filesystem protection is separate from signed-code authentication and execution
//! approval. No privileged or credential admission is supplied by these checks.
//! Startup admission validates an injected native registration and retains only its
//! exact approved installation. These boundaries neither load credentials nor start
//! processes.

#![deny(missing_docs)]

pub mod native_registration;
pub mod protected_directory;
pub mod root_protection;
pub mod startup;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod test_support;
