fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), spaceterm_server::ServerError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return spaceterm_server::run_development();
    }
    #[cfg(feature = "client")]
    {
        let client = spaceterm_server::LocalServer::for_identity("spaceterm-development")?;
        let status = if args == ["setup"] {
            client.setup_and_start()?
        } else if args == ["status"] {
            client.refresh()?
        } else {
            return Err(spaceterm_server::ServerError::Protocol);
        };
        println!(
            "Authenticated local Development server; protocol {}.",
            status.protocol_major
        );
        Ok(())
    }
    #[cfg(not(feature = "client"))]
    Err(spaceterm_server::ServerError::Protocol)
}
