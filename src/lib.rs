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
            let candidate = local_startup::discover_candidate();
            if cfg!(spaceterm_packaged)
                && let Err(error) = candidate
            {
                eprintln!("{}", spaceterm_core::CoreError::from(error));
                std::process::exit(1);
            }
            println!(
                "{}",
                serde_json::json!({
                    "schema": 1,
                    "identity": identity.directory_name(),
                    "version": env!("SPACETERM_VERSION"),
                    "commit": env!("SPACETERM_COMMIT"),
                    "target": env!("SPACETERM_TARGET"),
                    "core_candidate_sha256": candidate.as_ref().ok().map(|candidate| candidate.digest()),
                    "core_provenance": if cfg!(spaceterm_packaged) { core_resource_manifest::compiled_build().ok() } else { None },
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
