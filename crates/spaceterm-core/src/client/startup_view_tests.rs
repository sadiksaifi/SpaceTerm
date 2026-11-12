use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

struct SavedClient {
    root: std::path::PathBuf,
    paths: Paths,
}
impl SavedClient {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-startup-view-{:x}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("d"),
            runtime: root.join("r"),
        };
        Directory::open(&paths.data, true)
            .unwrap()
            .read_or_create()
            .unwrap();
        Self { root, paths }
    }
    fn connection(&self) -> MachineConnection {
        MachineConnection::with_paths(self.paths.clone()).unwrap()
    }
    fn directory(&self) -> Directory {
        self.connection().client_directory().unwrap()
    }
    fn bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.paths.data.join("clients/desktop").join(name)).unwrap()
    }
}
impl Drop for SavedClient {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn attachments_contention_does_not_poison_the_relaunched_handle() {
    let saved = SavedClient::new();
    let core = saved.connection();
    let held = saved.directory().lock("attachments.lock", true).unwrap();
    assert!(matches!(
        core.attachments(),
        Err(CoreError::StorageUnavailable)
    ));
    drop(held);
    assert!(
        core.attachments().is_ok(),
        "contention must end with the previous holder"
    );
}

#[test]
fn startup_attachments_wait_for_the_previous_holder_then_keep_its_saved_choices() {
    let saved = SavedClient::new();
    let previous = saved.connection();
    let attachments = previous.attachments().unwrap();
    attachments
        .initialize(
            &crate::hierarchy::Inventory {
                revision: 1,
                workspaces: Vec::new(),
                close: None,
                repositories: None,
            },
            None,
        )
        .unwrap();
    drop(attachments);
    drop(previous);
    let bytes = saved.bytes("attachments.json");
    let held = saved.directory().lock("attachments.lock", true).unwrap();
    let core = saved.connection();
    let (answer, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        answer
            .send(core.attachments_cancellable(&|| false))
            .unwrap();
    });
    let pending = matches!(
        result.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    drop(held);
    let restored = if pending {
        result.recv_timeout(Duration::from_secs(2)).unwrap()
    } else {
        Err(CoreError::StorageUnavailable)
    };
    worker.join().unwrap();
    assert!(
        pending,
        "a previous process's attachments lock is temporary"
    );
    let restored = restored.unwrap().unwrap();
    assert!(
        !restored
            .initialize(
                &crate::hierarchy::Inventory {
                    revision: 1,
                    workspaces: Vec::new(),
                    close: None,
                    repositories: None,
                },
                None
            )
            .unwrap()
    );
    assert_eq!(saved.bytes("attachments.json"), bytes);
}

#[test]
fn startup_workspace_order_waits_for_the_previous_holder_without_replacing_it() {
    use crate::workspace_order::{OwnerId, WorkspaceOrder, WorkspaceReference};
    let saved = SavedClient::new();
    let core = saved.connection();
    let a = WorkspaceReference {
        owner: OwnerId::from_bytes([1; 32]),
        workspace: [7; 32],
    };
    let b = WorkspaceReference {
        owner: OwnerId::from_bytes([2; 32]),
        workspace: [8; 32],
    };
    let mut order = WorkspaceOrder::default();
    order.record(&[b, a]).unwrap();
    core.workspace_order_store().unwrap().save(order).unwrap();
    let bytes = saved.bytes("workspace-order.json");
    let held = saved
        .directory()
        .lock("workspace-order.lock", true)
        .unwrap();
    let (answer, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        answer
            .send(core.workspace_order_store_cancellable(&|| false))
            .unwrap();
    });
    let pending = matches!(
        result.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    drop(held);
    let restored = if pending {
        result.recv_timeout(Duration::from_secs(2)).unwrap()
    } else {
        Err(CoreError::StorageUnavailable)
    };
    worker.join().unwrap();
    assert!(
        pending,
        "a previous process's Workspace order lock is temporary"
    );
    assert_eq!(restored.unwrap().unwrap().order().restore(&[a, b]), [b, a]);
    assert_eq!(saved.bytes("workspace-order.json"), bytes);
}

#[test]
fn startup_saved_view_waits_cancel_without_acquiring_after_close() {
    for name in ["attachments.lock", "workspace-order.lock"] {
        let saved = SavedClient::new();
        let held = saved.directory().lock(name, true).unwrap();
        let core = saved.connection();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let (answer, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let stopped = match name {
                "attachments.lock" => core
                    .attachments_cancellable(&|| cancel.load(Ordering::Acquire))
                    .map(|store| store.is_none()),
                _ => core
                    .workspace_order_store_cancellable(&|| cancel.load(Ordering::Acquire))
                    .map(|store| store.is_none()),
            };
            answer.send(stopped).unwrap();
        });
        let pending = matches!(
            result.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        cancelled.store(true, Ordering::Release);
        let stopped = if pending {
            result.recv_timeout(Duration::from_secs(1)).unwrap()
        } else {
            Err(CoreError::StorageUnavailable)
        };
        worker.join().unwrap();
        drop(held);
        assert!(pending, "{name} must wait before cancellation");
        assert_eq!(stopped, Ok(true));
        assert!(saved.directory().try_lock(name).unwrap().is_some());
    }
}

#[test]
fn startup_unreadable_saved_choices_and_order_fail_without_retry_or_replacement() {
    for name in [c"attachments.json", c"workspace-order.json"] {
        let saved = SavedClient::new();
        let directory = saved.directory();
        directory.write_json(name, &"not a saved record").unwrap();
        let bytes = saved.bytes(name.to_str().unwrap());
        let core = saved.connection();
        let (answer, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let error = if name == c"attachments.json" {
                core.attachments_cancellable(&|| false).err()
            } else {
                core.workspace_order_store_cancellable(&|| false).err()
            };
            answer.send(error).unwrap();
        });
        assert_eq!(
            result.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(CoreError::UnsafeStorage)
        );
        worker.join().unwrap();
        assert_eq!(saved.bytes(name.to_str().unwrap()), bytes);
    }
}
