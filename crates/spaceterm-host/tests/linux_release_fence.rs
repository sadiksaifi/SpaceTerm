//! Native flock and protected-record composition with injected authorization/process peers.
//! These tests run on both hosts. They are not privileged custody or release evidence.

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "native_authority/release.rs"]
mod native_release;
