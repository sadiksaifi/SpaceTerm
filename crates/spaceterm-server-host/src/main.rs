fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), spaceterm_server::ServerError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--version"] {
        println!("SpaceTerm Server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let [command, identity] = args.as_slice() else {
        return Err(spaceterm_server::ServerError::Protocol);
    };
    let identity = identity
        .to_str()
        .ok_or(spaceterm_server::ServerError::Protocol)?;
    if command == "serve" {
        if identity == "spaceterm-development" {
            return spaceterm_server::run_local_with_panes(
                identity,
                spaceterm::runtime::SharedWorkspaceHost::local()?,
            );
        }
        return spaceterm_server::run_local(identity);
    }
    #[cfg(feature = "client")]
    {
        let client = spaceterm_server::LocalServer::for_identity(identity)?;
        let status = if command == "setup" {
            client.setup_and_start()?
        } else if command == "status" {
            client.refresh()?
        } else {
            return Err(spaceterm_server::ServerError::Protocol);
        };
        println!(
            "Authenticated local server; protocol {}.",
            status.protocol_major
        );
        Ok(())
    }
    #[cfg(not(feature = "client"))]
    Err(spaceterm_server::ServerError::Protocol)
}
