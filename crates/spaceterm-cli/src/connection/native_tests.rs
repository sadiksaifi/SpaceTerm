//! Explicit native fixtures. The owner runs only in Penguin's isolated mount namespace.
use super::*;
use clap::Parser;
use spaceterm_server::{
    ServerConnection,
    direct::{Invitation, NetworkPolicy},
    protocol::{Operation, ReplyBody},
};
use std::io::{BufRead, Write};
const IDENTITY: &str = "spaceterm-development";
fn emit(value: serde_json::Value) {
    println!("CLI722 {value}");
    std::io::stdout().flush().unwrap();
}
fn round_trip(link: &spaceterm_server::connection::link::ServerLink) -> bool {
    link.request(Operation::ReleaseAttachment {
        attachment: spaceterm_server::attachments::Attachment {
            workspace: [0; 32],
            generation: 0,
        },
    })
    .ok()
    .and_then(|reply| reply.wait_for(Duration::from_secs(5)))
    .is_some_and(|reply| matches!(reply, Ok(ReplyBody::Done)))
}

#[test]
#[ignore = "native fixture owner; requires an isolated Linux Development data root"]
fn native_owner() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "the fixture never exposes a Mac server"
    );
    let bind = std::env::var("SPACETERM_TEST_BIND").expect("explicit fixture bind");
    let (owner, keep_alive) = std::os::unix::net::UnixStream::pair().unwrap();
    let (ready, waiting) = std::sync::mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        spaceterm_server::run_owned_development(
            spaceterm_server::panes(|sync| {
                Ok(spaceterm::runtime::Served::new(
                    spaceterm::runtime::SharedWorkspaceHost::local(sync)?,
                ))
            }),
            owner,
            move || {
                ready.send(()).unwrap();
                Ok(())
            },
        )
    });
    waiting
        .recv_timeout(Duration::from_secs(30))
        .expect("fixture server ready");
    let connection = ServerConnection::for_identity(IDENTITY).unwrap();
    connection
        .set_direct_policy(Some(NetworkPolicy::parse(&bind).unwrap()), true)
        .unwrap();
    emit(serde_json::json!({"ready":true}));
    let mut invitation: Option<Invitation> = None;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line == "invite" {
            if let Some(previous) = invitation.take() {
                connection.withdraw_invitation(previous.id()).unwrap();
            }
            let next = connection.invite_device(Some(bind.clone())).unwrap();
            emit(serde_json::json!({"invitation":next.display_text().as_str()}));
            invitation = Some(next);
        } else if let Some(code) = line.strip_prefix("approve ") {
            let invitation = invitation.as_ref().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let claims = connection.pairing_claims(Some(invitation.id())).unwrap();
                if let Some(claim) = claims.first() {
                    assert_eq!(claims.len(), 1);
                    assert_eq!(claim.comparison_code().to_string(), code);
                    connection.approve_device(claim).unwrap();
                    break;
                }
                assert!(Instant::now() < deadline, "fixture claim arrived");
                std::thread::sleep(Duration::from_millis(20));
            }
            emit(serde_json::json!({"approved":true}));
        } else if line == "devices" {
            emit(serde_json::json!({"devices":connection.paired_devices().unwrap().len()}));
        } else if line == "stop" {
            break;
        } else {
            panic!("unknown fixture command");
        }
    }
    if let Some(invitation) = invitation {
        connection.withdraw_invitation(invitation.id()).unwrap();
    }
    connection.set_direct_policy(None, false).unwrap();
    drop(connection);
    drop(keep_alive);
    server.join().unwrap().unwrap();
}

#[test]
#[ignore = "native credential and QUIC fixture client; driven through anonymous pipes"]
fn native_client() {
    let name = std::env::var("SPACETERM_TEST_CONNECTION_NAME").expect("unique fixture name");
    assert!(name.starts_with("cli722-e-native-"));
    let global = Global {
        server: None,
        agent: None,
        json: true,
        quiet: false,
        verbose: false,
        no_input: true,
        timeout: Some(Duration::from_secs(30)),
    };
    let added = execute(
        &ConnectionCommand::Add {
            name: Some(name.clone()),
            join: true,
            address: None,
        },
        &global,
        IDENTITY,
    );
    assert_eq!(
        added.render(
            (&global).into(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock()
        ),
        0,
        "native join completed"
    );
    let mut targeting = Global {
        server: Some(name.clone()),
        agent: None,
        json: true,
        quiet: false,
        verbose: false,
        no_input: true,
        timeout: None,
    };
    let route = route::from_global(&targeting, IDENTITY).expect("native route resolved");
    let base = route
        .admit(
            IDENTITY,
            None,
            &CliCredentials::new(IDENTITY),
            connections::Interaction::Quiet,
            Instant::now() + Duration::from_secs(30),
            &|| false,
        )
        .expect("native base admitted");
    targeting.agent = Some("native-cli722-agent".parse().unwrap());
    let agent = route
        .admit(
            IDENTITY,
            targeting.agent,
            &CliCredentials::new(IDENTITY),
            connections::Interaction::Quiet,
            Instant::now() + Duration::from_secs(30),
            &|| false,
        )
        .expect("native agent admitted");
    assert!(round_trip(&base.link));
    assert!(round_trip(&agent.link));
    assert_ne!(
        base.connection.admitted_client_id().unwrap(),
        agent.connection.admitted_client_id().unwrap()
    );
    assert_eq!(
        base.connection.admitted_server_id().unwrap(),
        agent.connection.admitted_server_id().unwrap()
    );
    emit(serde_json::json!({"admitted":true, "route":base.server.route}));
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let output = if line == "work" {
            let cli = crate::args::Cli::try_parse_from([
                "spaceterm",
                "--server",
                &name,
                "--json",
                "workspace",
                "create",
                "cli722-e-work",
            ])
            .unwrap();
            let output = crate::work::start::start(&cli).expect("work command dispatched");
            assert_eq!(
                output.write(
                    (&cli.global).into(),
                    &mut std::io::stdout().lock(),
                    &mut std::io::stderr().lock()
                ),
                0,
                "paired work mutation completed"
            );
            None
        } else if line == "status" {
            let cli = crate::args::Cli::try_parse_from([
                "spaceterm",
                "--server",
                &name,
                "--json",
                "server",
                "status",
            ])
            .unwrap();
            let crate::args::Command::Server(command) = &cli.command else {
                unreachable!()
            };
            let reply = crate::server::run(IDENTITY, &cli.global, command);
            assert_eq!(
                reply.write(
                    (&cli.global).into(),
                    &mut std::io::stdout().lock(),
                    &mut std::io::stderr().lock(),
                ),
                0,
                "paired service lane status completed"
            );
            None
        } else if line == "list" {
            Some(execute(&ConnectionCommand::List, &global, IDENTITY))
        } else if line == "preview" {
            Some(execute(
                &ConnectionCommand::Remove {
                    name: name.clone(),
                    confirm: None,
                },
                &global,
                IDENTITY,
            ))
        } else if let Some(token) = line.strip_prefix("remove ") {
            Some(execute(
                &ConnectionCommand::Remove {
                    name: name.clone(),
                    confirm: Some(token.into()),
                },
                &global,
                IDENTITY,
            ))
        } else if line == "probe" {
            emit(
                serde_json::json!({"revoked":!round_trip(&base.link) && !round_trip(&agent.link)}),
            );
            None
        } else if line == "stop" {
            break;
        } else {
            panic!("unknown native client command");
        };
        if let Some(output) = output {
            output.render(
                (&global).into(),
                &mut std::io::stdout().lock(),
                &mut std::io::stderr().lock(),
            );
        }
    }
    base.link.close();
    agent.link.close();
}
