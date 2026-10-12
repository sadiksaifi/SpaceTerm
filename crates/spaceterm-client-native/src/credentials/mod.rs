//! Owned by pairing Job B. Implements this module after the H0 declaration freeze.
pub mod adapter;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
