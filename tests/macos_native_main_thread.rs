#![allow(dead_code, unused_imports, unused_variables)]

include!("../src/application_modules.rs");

#[cfg(target_os = "macos")]
fn main() {
    platform::native_main_thread_tests::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
