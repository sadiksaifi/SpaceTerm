//! Explicit disposable-guest evidence with the account-local client without native credential adapters.
use super::*;
use crate::{
    attachments::Attachment,
    connection::link::{ChannelEvent, ChannelHandle, LinkEvent, ServerLink},
    hierarchy::{Id, Inventory, Mutation, Outcome},
    pane::{AttachmentClaim, FocusTarget, Key, PaneSize},
    protocol::{ClaimReply, Frame, InputEvent, Operation, ReplyBody},
};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};

#[derive(Serialize, Deserialize)]
struct Witness {
    pane: [u8; 32],
    server: [u8; 32],
    session: [u8; 32],
    pin: [u8; 32],
}
fn request(link: &ServerLink, operation: Operation) -> ReplyBody {
    link.request(operation).unwrap().wait().unwrap()
}
/// The newest pushed inventory. It holds every outcome whose reply already arrived.
fn inventory(link: &ServerLink) -> (Inventory, u64) {
    let (_, revision) = link.next_inventory(0).expect("inventory unavailable");
    (revision.inventory.clone(), revision.next_sequence)
}
/// Claims `pane` on a new channel under attachment generation 1 of its Workspace, and returns
/// the frames the server pushes on it. The fixture releases no attachment, so the server holds no
/// newer generation.
fn claim(
    link: &ServerLink,
    pane: Id,
    claim: AttachmentClaim,
    size: PaneSize,
) -> (ChannelHandle, ClaimReply, Receiver<Box<Frame>>) {
    let workspace = inventory(link)
        .0
        .workspaces
        .iter()
        .find(|workspace| {
            workspace
                .tabs
                .iter()
                .any(|tab| tab.panes.iter().any(|candidate| candidate.id == pane))
        })
        .expect("fixture Pane missing")
        .id;
    let (sender, frames) = mpsc::channel();
    let channel = link
        .open_channel(Arc::new(move |event: ChannelEvent| {
            if let ChannelEvent::Frame(frame) = event {
                let _ = sender.send(frame);
            }
        }))
        .unwrap();
    let attachment = Attachment {
        workspace,
        generation: 1,
    };
    let reply = channel
        .claim(attachment, pane, claim, size, None)
        .unwrap()
        .wait()
        .unwrap();
    let ReplyBody::Claim(reply) = reply else {
        panic!("claim reply unavailable");
    };
    (channel, reply, frames)
}
fn command(channel: &ChannelHandle, text: &str) {
    let key = |physical, logical: &str, text: Option<&str>| Key {
        physical,
        action: 0,
        logical: logical.into(),
        text: text.map(Into::into),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    };
    channel
        .input(
            1,
            vec![
                InputEvent::Focus(FocusTarget::Terminal),
                // The protocol's physical-key codes for Unidentified and Enter.
                InputEvent::Key(key(0, "text-input", Some(text))),
                InputEvent::Key(key(58, "enter", None)),
            ],
        )
        .unwrap();
}
/// Applies pushed frames to the rows' text, acknowledging each, until a row reads `marker`.
fn await_marker(channel: &ChannelHandle, frames: &Receiver<Box<Frame>>, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut grid = None;
    let mut rows: Vec<String> = Vec::new();
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let frame = frames
            .recv_timeout(wait)
            .expect("fixture command did not complete");
        let update = &frame.update;
        // A keyframe and a new grid size start from blank rows.
        if let Some(geometry) = update.geometry
            && (frame.base.is_none() || grid != Some((geometry.columns, geometry.rows)))
        {
            grid = Some((geometry.columns, geometry.rows));
            rows = vec![String::new(); usize::from(geometry.rows)];
        }
        if let Some(shift) = update.shift {
            shift
                .apply(&mut rows, String::new)
                .expect("frame shift outside the screen");
        }
        for row in &update.rows {
            rows[usize::from(row.index)] = row.runs.iter().map(|run| run.text.as_str()).collect();
        }
        channel.acknowledge(frame.frame).unwrap();
        if rows.iter().any(|row| row.trim_end() == marker) {
            return;
        }
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
    let native = crate::ServerConnection::for_identity("spaceterm-preflight").unwrap();
    if phase == "stopped" {
        assert!(service.status().unwrap().stopped);
        assert!(matches!(
            crate::LocalStartup::for_identity("spaceterm-preflight")
                .unwrap()
                .run(|| panic!("stopped service cannot provision"), || false)
                .unwrap(),
            crate::StartupOutcome::Stopped
        ));
        assert!(native.connect(Arc::new(|_: LinkEvent| {})).is_err());
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
    let link = native.connect(Arc::new(|_: LinkEvent| {})).unwrap();
    let witness_path = Path::new("/home/stvm/artifacts/service-witness.json");
    let size = PaneSize {
        columns: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 18,
    };
    if phase == "prepare" {
        let (_, next_sequence) = inventory(&link);
        let ReplyBody::Outcome(outcome) = request(
            &link,
            Operation::Mutate {
                sequence: next_sequence,
                mutation: Mutation::CreateWorkspace {
                    name: "Disposable lifecycle fixture".into(),
                    directory: None,
                },
            },
        ) else {
            panic!("creation unavailable");
        };
        let Outcome::Accepted { target } = outcome.outcome else {
            panic!("creation refused");
        };
        link.acknowledge(next_sequence).unwrap();
        let (inventory, _) = inventory(&link);
        let pane = inventory
            .workspaces
            .iter()
            .find(|v| v.id == target)
            .unwrap()
            .tabs[0]
            .root_pane;
        let (channel, ClaimReply::Attached { lease }, frames) = claim(
            &link,
            pane,
            AttachmentClaim::Select { explicit: true },
            size,
        ) else {
            panic!("selection refused");
        };
        command(
            &channel,
            "stty -echo; printf '\\033[2J\\033[HVM-BEGIN\\n'; sleep 7; printf '\\nVM-CONTINUED\\n'; touch /home/stvm/artifacts/server-command-continued",
        );
        await_marker(&channel, &frames, "VM-BEGIN");
        let witness = Witness {
            pane,
            server: lease.server,
            session: lease.session,
            pin,
        };
        std::fs::write(witness_path, serde_json::to_vec(&witness).unwrap()).unwrap();
        // Keep the Pane's control, as a client that loses its connection does.
        channel.detach();
        link.close();
        println!("Fixture client disconnected while the server-owned command remained running.");
    } else {
        let witness: Witness =
            serde_json::from_slice(&std::fs::read(witness_path).unwrap()).unwrap();
        assert_eq!(pin, witness.pin, "server identity changed");
        let select = AttachmentClaim::Select {
            explicit: phase == "resume",
        };
        let (mut channel, mut reply, mut frames) = claim(&link, witness.pane, select, size);
        if phase == "resume"
            && let ClaimReply::Takeover(takeover) = &reply
        {
            let target = takeover.target;
            assert_eq!(
                (target.server, target.session),
                (witness.server, witness.session)
            );
            // A claim that did not attach leaves its channel unbound. Each claim needs a channel.
            channel.detach();
            (channel, reply, frames) = claim(
                &link,
                witness.pane,
                AttachmentClaim::TakeOver { target },
                size,
            );
        }
        if phase == "restored" {
            assert!(
                matches!(reply, ClaimReply::Restored(_)),
                "a restored Pane started a shell incidentally"
            );
            channel.detach();
            println!(
                "Reboot retained the authenticated identity and organization with dormant Panes."
            );
        } else {
            let ClaimReply::Attached { lease } = reply else {
                panic!("reattachment refused");
            };
            assert_eq!(
                (lease.server, lease.session),
                (witness.server, witness.session),
                "running Session was replaced"
            );
            await_marker(&channel, &frames, "VM-CONTINUED");
            channel.detach();
            assert!(Path::new("/home/stvm/artifacts/server-command-continued").is_file());
            println!(
                "Logout and reconnect preserved the same server, Session, and running command."
            );
        }
    }
}
