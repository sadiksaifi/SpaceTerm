include!("application_modules.rs");

/// Starts the native desktop application.
#[cfg(feature = "desktop")]
pub fn run_desktop() {
    if let Some(code) = platform::dispatch_helper_from_environment() {
        std::process::exit(code);
    }

    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let identity = application_identity::ApplicationIdentity::current();
    match arguments.as_slice() {
        [argument] if argument == "--build-info" => {
            println!(
                "{}",
                serde_json::json!({
                    "schema": 1,
                    "identity": identity.directory_name(),
                    "version": env!("SPACETERM_VERSION"),
                    "commit": env!("SPACETERM_COMMIT"),
                    "target": env!("SPACETERM_TARGET"),
                    "server_candidate_sha256": env!("SPACETERM_SERVER_SHA256"),
                })
            );
            return;
        }
        [argument] if argument == "--version" => {
            println!("{}", identity.version_label());
            return;
        }
        [argument] if argument == "--help" => {
            println!("Usage: spaceterm-desktop [--version | --build-info | --help]");
            return;
        }
        _ => {}
    }
    if let Err(error) = local_startup::supervise_development_client() {
        eprintln!("{error}");
        return;
    }
    platform::main();
}
