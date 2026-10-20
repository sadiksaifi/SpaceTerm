//! Explicit disposable-guest evidence with the account-local client without native credential adapters.
use super::*;
use crate::{
    hierarchy::{Mutation, Outcome},
    pane::{Key, Lease, PaneConnection, PaneSize, Reply, Request},
};

#[derive(Serialize, Deserialize)]
struct Witness {
    pane: [u8; 32],
    server: [u8; 32],
    session: [u8; 32],
    pin: [u8; 32],
}
fn command(connection: &mut PaneConnection, lease: Lease, text: &str) {
    for key in [
        Key {
            physical: 0,
            action: 0,
            logical: "text-input".into(),
            text: Some(text.into()),
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
        Key {
            physical: 58,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
    ] {
        connection.exchange(&Request::Key { lease, key }).unwrap();
    }
}
fn await_marker(connection: &mut PaneConnection, lease: Lease, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Reply::Screen(screen) = connection.exchange(&Request::Poll { lease }).unwrap() {
            let found = screen.rows.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    == marker
            });
            if found {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture command did not complete"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
#[ignore = "Explicit prepare/resume/restored evidence only in the disposable stvm guest"]
fn installed_service_preserves_sessions_and_restores_dormant_organization() {
    // SAFETY: geteuid has no preconditions. The guard prevents accidental physical-host mutation.
    assert_eq!(
        unsafe { libc::geteuid() },
        20000,
        "disposable stvm guest required"
    );
    let phase =
        std::env::var("SPACETERM_VM_SERVICE_PHASE").expect("explicit fixture phase required");
    assert!(matches!(
        phase.as_str(),
        "prepare" | "resume" | "restored" | "stopped"
    ));
    let service = Service::for_identity("spaceterm-preflight").unwrap();
    let paths = service.paths.clone();
    assert_eq!(
        paths.data,
        Path::new("/home/stvm/.local/share/spaceterm-preflight/server")
    );
    let native = crate::LocalServer::for_identity("spaceterm-preflight").unwrap();
    if phase == "stopped" {
        assert!(service.status().unwrap().stopped);
        assert!(matches!(
            crate::LocalStartup::for_identity("spaceterm-preflight")
                .unwrap()
                .run(|| panic!("stopped service cannot provision"), || false)
                .unwrap(),
            crate::StartupOutcome::Stopped
        ));
        assert!(native.connect_pane().is_err());
        let status = service.status().unwrap();
        assert!(status.stopped && !status.running);
        println!("Stop survived normal launch and reconnect without starting a server.");
        return;
    }
    assert!(service.status().unwrap().running);
    native.setup().unwrap();
    let pin = Directory::open(&paths.data, false)
        .unwrap()
        .read()
        .unwrap()
        .server;
    let mut connection = native.connect_pane().unwrap();
    let witness_path = Path::new("/home/stvm/artifacts/service-witness.json");
    let size = PaneSize {
        columns: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 18,
    };
    if phase == "prepare" {
        let Reply::Inventory { next_sequence, .. } =
            connection.exchange(&Request::Inventory).unwrap()
        else {
            panic!("inventory unavailable");
        };
        let Reply::Outcome(outcome) = connection
            .exchange(&Request::Mutate {
                sequence: next_sequence,
                mutation: Mutation::CreateWorkspace {
                    name: "Disposable lifecycle fixture".into(),
                    directory: None,
                },
            })
            .unwrap()
        else {
            panic!("creation unavailable");
        };
        let Outcome::Accepted { target } = outcome.outcome else {
            panic!("creation refused");
        };
        connection
            .exchange(&Request::Acknowledge {
                sequence: next_sequence,
            })
            .unwrap();
        let Reply::Inventory { inventory, .. } = connection.exchange(&Request::Inventory).unwrap()
        else {
            panic!("inventory unavailable");
        };
        let pane = inventory
            .workspaces
            .iter()
            .find(|v| v.id == target)
            .unwrap()
            .tabs[0]
            .root_pane;
        let Reply::Attached { lease } = connection
            .exchange(&Request::SelectPane {
                pane,
                explicit: true,
                size,
                style: None,
            })
            .unwrap()
        else {
            panic!("selection refused");
        };
        command(
            &mut connection,
            lease,
            "stty -echo; printf '\\033[2J\\033[HVM-BEGIN\\n'; sleep 7; printf '\\nVM-CONTINUED\\n'; touch /home/stvm/artifacts/server-command-continued",
        );
        await_marker(&mut connection, lease, "VM-BEGIN");
        let witness = Witness {
            pane,
            server: lease.server,
            session: lease.session,
            pin,
        };
        std::fs::write(witness_path, serde_json::to_vec(&witness).unwrap()).unwrap();
        println!("Fixture client disconnected while the server-owned command remained running.");
    } else {
        let witness: Witness =
            serde_json::from_slice(&std::fs::read(witness_path).unwrap()).unwrap();
        assert_eq!(pin, witness.pin, "server identity changed");
        let mut reply = connection
            .exchange(&Request::SelectPane {
                pane: witness.pane,
                explicit: phase == "resume",
                size,
                style: None,
            })
            .unwrap();
        if phase == "resume"
            && let Reply::Takeover { target, .. } = reply
        {
            assert_eq!(
                (target.server, target.session),
                (witness.server, witness.session)
            );
            reply = connection
                .exchange(&Request::TakeOver {
                    target,
                    size,
                    style: None,
                })
                .unwrap();
        }
        if phase == "restored" {
            assert!(
                matches!(reply, Reply::Restored { .. }),
                "a restored Pane started a shell incidentally"
            );
            println!(
                "Reboot retained the authenticated identity and organization with dormant Panes."
            );
        } else {
            let Reply::Attached { lease } = reply else {
                panic!("reattachment refused");
            };
            assert_eq!(
                (lease.server, lease.session),
                (witness.server, witness.session),
                "running Session was replaced"
            );
            await_marker(&mut connection, lease, "VM-CONTINUED");
            assert!(Path::new("/home/stvm/artifacts/server-command-continued").is_file());
            println!(
                "Logout and reconnect preserved the same server, Session, and running command."
            );
        }
    }
}
