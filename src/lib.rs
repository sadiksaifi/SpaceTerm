include!("application_modules.rs");

/// Starts the native desktop application.
#[cfg(feature = "desktop")]
pub fn run_desktop() {
    if let Some(code) = platform::dispatch_helper_from_environment() {
        std::process::exit(code);
    }

    if std::env::args_os()
        .skip(1)
        .eq([std::ffi::OsStr::new("--version")])
    {
        println!(
            "{}",
            application_identity::ApplicationIdentity::current().version_label()
        );
        return;
    }
    platform::main();
}
