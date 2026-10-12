//! Owned by pairing Job B. Implements this module after the H0 declaration freeze.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(test, feature = "native-tests"))]
mod native_tests;
#[cfg(test)]
mod tests;
