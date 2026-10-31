use super::tests::{host, identity, screen, size};
use super::*;
use spaceterm_server::protocol::PaneEvent;

#[test]
fn explicit_directory_creation_is_recursive_idempotent_and_does_not_edit_workspaces() {
    use pane::directories::{Creation, Value};
    use spaceterm_server::protocol::ReplyBody;
    let root = std::env::temp_dir().join(format!(
        "spaceterm-create-{:x}",
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("parent/child");
    let mut host = SharedWorkspaceHost::new(host());
    let before = host.request([1; 32], [2; 32], Request::Inventory);
    for id in [1, 2] {
        let creation = HostRead::Creation(Creation { path: path.clone() });
        let Ok(ReplyBody::Directory(Ok(Value::Directory(actual)))) =
            super::tests::read(&mut host, [2; 32], id, creation)
        else {
            panic!("directory creation did not complete");
        };
        assert_eq!(actual, path);
        assert!(actual.is_dir());
    }
    let after = host.request([1; 32], [2; 32], Request::Inventory);
    let (
        Reply::Inventory {
            inventory: before, ..
        },
        Reply::Inventory {
            inventory: after, ..
        },
    ) = (before, after)
    else {
        panic!("inventory unavailable")
    };
    assert_eq!(before, after);
    host.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_recovery_poll_retires_a_worker_directory_failure_before_any_tick() {
    let root = std::env::temp_dir().join(format!(
        "spaceterm-poll-directory-{:x}",
        u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let directory = local_filesystem().validate_directory(&root).unwrap();
    let mut host = host();
    host.identity = Some(identity(root.clone()));
    // The owning host validated this object, but it disappears before worker preparation.
    std::fs::remove_dir(&root).unwrap();
    let lease = host
        .start_directory([1; 32], [1; 32], size(), None, directory)
        .unwrap();
    let cleanup = host
        .pane
        .as_mut()
        .unwrap()
        .runtime
        .shutdown_and_wait(Duration::from_secs(5));
    assert!(matches!(
        screen(&mut host, [1; 32], lease),
        Err(PaneEvent::DirectoryUnavailable)
    ));
    assert!(
        cleanup.is_ok(),
        "preparation failed before any process was started"
    );
    assert!(host.pane.is_none());
    assert_eq!(
        host.identity.as_ref().unwrap().startup_directory(),
        root.as_path()
    );
    assert!(matches!(
        host.select([1; 32], [2; 32], false, size(), None),
        Ok(Reply::Restored {
            directory_unavailable: true,
            recovery: Some(_),
            ..
        })
    ));
}

/// A directory read is one request: the tick replies when it settles, and a cancelled one replies
/// `Stale` at once.
#[test]
fn a_directory_read_replies_when_it_settles_without_being_polled() {
    use spaceterm_server::connection::outbox::Outbox;
    use spaceterm_server::connection::service::{Dispatch, PaneService as _, Peer};
    use spaceterm_server::protocol::{
        Operation, RawFrame, ReplyBody, RequestId, ServerMessage, frame::Assembler,
    };
    use std::time::Instant;
    const PEER: Peer = Peer {
        client: [1; 32],
        connection: [1; 32],
        pairing: None,
    };
    let mut served = super::dispatch::Served::new(SharedWorkspaceHost::new(host()));
    let outbox = Outbox::new(Arc::new(|| {}));
    served.admitted(&PEER, "Test".into(), outbox.clone());
    let replies = |outbox: &Outbox| {
        let mut assembler = Assembler::default();
        let mut replies = Vec::new();
        while let Some((frame, _)) = outbox.next_frame() {
            if let Some(ServerMessage::Reply(reply)) =
                assembler.server(RawFrame::parse(&frame).unwrap()).unwrap()
            {
                replies.push(*reply);
            }
        }
        replies
    };
    replies(&outbox);
    for (id, cancel) in [(RequestId(1), false), (RequestId(2), true)] {
        outbox.begin(id).unwrap();
        let operation = Operation::DirectoryRead(pane::directories::Operation::Home);
        let settled = match served.request(&PEER, id, operation) {
            Dispatch::Reply(result) => Some(*result),
            Dispatch::Deferred => None,
            Dispatch::After(_) => panic!("a read needs no sync"),
        };
        if cancel && settled.is_none() {
            served.cancel(&PEER, id);
            let [reply] = replies(&outbox).try_into().ok().unwrap();
            assert_eq!(reply.id, id);
            assert!(matches!(reply.result, Err(Refusal::Stale)));
            continue;
        }
        let result = settled.unwrap_or_else(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                served.tick(Instant::now());
                if let [reply] = replies(&outbox).as_slice() {
                    assert_eq!(reply.id, id);
                    break reply.result.clone();
                }
                assert!(Instant::now() < deadline, "the read never replied");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        assert!(matches!(
            result,
            Ok(ReplyBody::Directory(Ok(
                pane::directories::Value::Directory(_)
            )))
        ));
    }
    served.shutdown().unwrap();
}
