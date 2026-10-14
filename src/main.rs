include!("application_modules.rs");

fn main() {
    #[cfg(not(all(feature = "desktop-test-support", debug_assertions)))]
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--test-account-root")
    {
        eprintln!("desktop test composition unavailable");
        std::process::exit(2);
    }
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
