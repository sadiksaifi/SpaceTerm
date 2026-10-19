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
    // The Linux status item's control-only window for one server operation.
    #[cfg(target_os = "linux")]
    if let [flag, operation] = std::env::args_os().skip(1).collect::<Vec<_>>().as_slice()
        && flag == platform::linux_server_control::ARGUMENT
    {
        std::process::exit(platform::linux_server_control::main(operation));
    }
    platform::main();
}
