mod check;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), spaceterm_server::ServerError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--help"] {
        println!(
            "SpaceTerm Server\n\nserve IDENTITY\nservice-status IDENTITY\nservice-enable-linger IDENTITY\nservice-install IDENTITY RETAINED_INSTALLATION\nservice-start IDENTITY\nservice-stop IDENTITY\nservice-restart IDENTITY\nservice-uninstall IDENTITY\n\nService controls use this executable\'s compiled application identity. Stop and uninstall retain identity and Workspace organization."
        );
        return Ok(());
    }
    if args == ["--version"] {
        println!("SpaceTerm Server {}", env!("SPACETERM_SERVER_VERSION"));
        return Ok(());
    }
    if args == ["--build-info"] {
        println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "identity": env!("SPACETERM_SERVER_IDENTITY"),
                "version": env!("SPACETERM_SERVER_VERSION"),
                "commit": env!("SPACETERM_SERVER_COMMIT"),
                "target": env!("SPACETERM_SERVER_TARGET"),
            })
        );
        return Ok(());
    }
    if args == ["--check"] {
        return check::run();
    }
    if let [command, identity, installation] = args.as_slice() {
        if command != "service-install" || identity != env!("SPACETERM_SERVER_IDENTITY") {
            return Err(spaceterm_server::ServerError::Protocol);
        }
        let status =
            spaceterm_server::service::Service::for_identity(env!("SPACETERM_SERVER_IDENTITY"))?
                .install(std::path::Path::new(installation))?;
        println!(
            "Installed: {}; enabled: {}; running: {}; stopped: {}.",
            status.installed, status.enabled, status.running, status.stopped
        );
        return Ok(());
    }
    let [command, identity] = args.as_slice() else {
        return Err(spaceterm_server::ServerError::Protocol);
    };
    let identity = identity
        .to_str()
        .ok_or(spaceterm_server::ServerError::Protocol)?;
    if identity != env!("SPACETERM_SERVER_IDENTITY") {
        return Err(spaceterm_server::ServerError::Protocol);
    }
    if command == "service-serve" {
        check::resources()?;
        return spaceterm_server::service::run(
            identity,
            spaceterm::runtime::SharedWorkspaceHost::local()?,
        );
    } else if let Some(action) = command.to_str().and_then(|v| v.strip_prefix("service-")) {
        let service = spaceterm_server::service::Service::for_identity(identity)?;
        if action == "enable-linger" {
            service.enable_linger(spaceterm_server::service::LingerAuthorization::Terminal)?;
            println!(
                "Boot startup authorized for this account. Install or Start the server explicitly."
            );
            return Ok(());
        }
        let status = match action {
            "status" => service.status()?,
            "start" => service.start()?,
            "stop" => service.stop()?,
            "restart" => service.restart()?,
            "uninstall" => service.uninstall()?,
            _ => return Err(spaceterm_server::ServerError::Protocol),
        };
        println!(
            "Installed: {}; enabled: {}; running: {}; stopped: {}.",
            status.installed, status.enabled, status.running, status.stopped
        );
        return Ok(());
    }
    if command == "serve" || command == "service-serve" {
        check::resources()?;
        return spaceterm_server::run_local_with_panes(
            identity,
            spaceterm::runtime::SharedWorkspaceHost::local()?,
        );
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
            "Connected to the local server; protocol {}.",
            status.protocol_major
        );
        Ok(())
    }
    #[cfg(not(feature = "client"))]
    Err(spaceterm_server::ServerError::Protocol)
}
