pub(crate) use spaceterm_host::shell_integration::*;

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "unix_adapter_tests/shell_integration.rs"]
mod unix_adapter_tests;
