#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use spaceterm_host::ipc::{IpcListener, IpcTransport, PrivateIpcDirectory};
use spaceterm_host::transport::{AdmissionLimits, MessageLimit, MessageTransport, TransportError};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

fn directory() -> (tempfile::TempDir, PrivateIpcDirectory) {
    let temporary = tempfile::Builder::new()
        .prefix("st-ipc-")
        .tempdir_in("/private/tmp")
        .or_else(|_| {
            tempfile::Builder::new()
                .prefix("st-ipc-")
                .tempdir_in("/tmp")
        })
        .unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateIpcDirectory::open(temporary.path()).unwrap();
    (temporary, directory)
}

fn admission() -> AdmissionLimits {
    AdmissionLimits::new(2, Duration::from_secs(1)).unwrap()
}

fn directory_entries(path: &Path) -> Vec<(OsString, u64, u64, u32)> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            (
                entry.file_name(),
                metadata.dev(),
                metadata.ino(),
                metadata.mode(),
            )
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[tokio::test]
async fn ipc_connect_reports_a_missing_endpoint_as_unavailable_without_mutating_entries() {
    let (temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    fs::write(temporary.path().join("retained"), b"untouched").unwrap();
    let before = directory_entries(temporary.path());

    let error = IpcTransport::connect(&endpoint, MessageLimit::envelope())
        .await
        .err();

    assert_eq!(directory_entries(temporary.path()), before);
    assert_eq!(error, Some(TransportError::Unavailable));
}

#[tokio::test]
async fn ipc_connect_reports_a_refused_endpoint_as_unavailable_without_mutating_entries() {
    let (temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    fs::write(temporary.path().join("retained"), b"untouched").unwrap();
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
    socket
        .bind(&socket2::SockAddr::unix(endpoint.path()).unwrap())
        .unwrap();
    let before = directory_entries(temporary.path());

    let error = IpcTransport::connect(&endpoint, MessageLimit::envelope())
        .await
        .err();

    assert_eq!(directory_entries(temporary.path()), before);
    assert_eq!(error, Some(TransportError::Unavailable));
}

#[path = "support/cleanup_race.rs"]
mod cleanup_race;

#[tokio::test]
async fn ipc_cleanup_preserves_a_foreign_socket_substituted_after_the_native_inode_check() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;

    let (temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener =
        IpcListener::bind(endpoint.clone(), MessageLimit::envelope(), admission()).unwrap();
    let foreign_path = temporary.path().join("foreign");
    let foreign = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
    foreign
        .bind(&socket2::SockAddr::unix(&foreign_path).unwrap())
        .unwrap();
    let bound = fs::symlink_metadata(&foreign_path).unwrap();
    let race = cleanup_race::CleanupRace::arm(endpoint.path(), &foreign_path);
    drop(listener);
    race.restore_preserved_entry();
    let retained = fs::symlink_metadata(endpoint.path())
        .expect("the checked pathname was replaced with a live foreign socket before removal");
    assert_eq!((retained.dev(), retained.ino()), (bound.dev(), bound.ino()));
    foreign.listen(8).unwrap();
    let mut client = std::os::unix::net::UnixStream::connect(endpoint.path()).unwrap();
    client.write_all(b"retained").unwrap();
    let (accepted, _) = foreign.accept().unwrap();
    let mut accepted = std::os::unix::net::UnixStream::from(accepted);
    accepted
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut message = [0; 8];
    accepted.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"retained");
}

#[tokio::test]
async fn ipc_round_trip_preserves_whole_messages_and_only_admits_the_local_account() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let limit = MessageLimit::new(64).unwrap();
    let listener = IpcListener::bind(endpoint.clone(), limit, admission()).unwrap();
    let client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    let pending = listener.accept().await.unwrap();
    assert_eq!(
        pending.peer_facts().local_uid,
        Some(unsafe { libc::geteuid() })
    );
    let server = pending.finish_authentication().unwrap();
    client.send(b"one").await.unwrap();
    client.send(b"two").await.unwrap();
    assert_eq!(server.recv().await.unwrap(), Some(b"one".to_vec()));
    assert_eq!(server.recv().await.unwrap(), Some(b"two".to_vec()));
    server.send(b"reply").await.unwrap();
    assert_eq!(client.recv().await.unwrap(), Some(b"reply".to_vec()));
    client.close().await.unwrap();
    assert_eq!(server.recv().await.unwrap(), None);
}

#[tokio::test]
async fn ipc_rejects_an_oversized_prefix_without_waiting_for_its_body() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener =
        IpcListener::bind(endpoint.clone(), MessageLimit::new(8).unwrap(), admission()).unwrap();
    let mut client = UnixStream::connect(endpoint.path()).await.unwrap();
    let server = listener
        .accept()
        .await
        .unwrap()
        .finish_authentication()
        .unwrap();
    client.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), server.recv())
            .await
            .unwrap(),
        Err(TransportError::MessageTooLarge)
    );
}

#[tokio::test]
async fn cancelled_ipc_send_disconnects_the_peer_instead_of_leaving_a_partial_live_frame() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener =
        IpcListener::bind(endpoint.clone(), MessageLimit::envelope(), admission()).unwrap();
    let client = IpcTransport::connect(&endpoint, MessageLimit::envelope())
        .await
        .unwrap();
    let server = listener
        .accept()
        .await
        .unwrap()
        .finish_authentication()
        .unwrap();
    let message = vec![1; 2 * 1024 * 1024];
    assert!(
        tokio::time::timeout(Duration::from_millis(20), client.send(&message))
            .await
            .is_err()
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), server.recv())
            .await
            .unwrap(),
        Err(TransportError::InvalidFrame)
    );
}

#[tokio::test]
async fn ipc_cleanup_does_not_unlink_a_replacement_socket() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener =
        IpcListener::bind(endpoint.clone(), MessageLimit::envelope(), admission()).unwrap();
    fs::rename(endpoint.path(), endpoint.path().with_file_name("old")).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
    drop(listener);
    assert!(endpoint.path().exists());
    drop(replacement);
}

#[tokio::test]
async fn ipc_receive_cancellation_preserves_fragmented_prefix_and_body() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener = IpcListener::bind(
        endpoint.clone(),
        MessageLimit::new(16).unwrap(),
        admission(),
    )
    .unwrap();
    let mut client = UnixStream::connect(endpoint.path()).await.unwrap();
    let server = listener
        .accept()
        .await
        .unwrap()
        .finish_authentication()
        .unwrap();
    client.write_all(&[0, 0]).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), server.recv())
            .await
            .is_err()
    );
    client.write_all(&[0, 5, b'h', b'e']).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), server.recv())
            .await
            .is_err()
    );
    client.write_all(b"llo").await.unwrap();
    assert_eq!(server.recv().await.unwrap(), Some(b"hello".to_vec()));
}

#[tokio::test]
async fn ipc_admission_deadline_closes_idle_peers_and_releases_the_bounded_slot() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let limit = MessageLimit::new(16).unwrap();
    let listener = IpcListener::bind(
        endpoint.clone(),
        limit,
        AdmissionLimits::new(1, Duration::from_millis(60)).unwrap(),
    )
    .unwrap();
    let first_client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    let pending = listener.accept().await.unwrap();
    let _second_client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), first_client.recv())
            .await
            .unwrap()
            .unwrap(),
        None
    );
    assert_eq!(
        pending.finish_authentication().err(),
        Some(TransportError::HandshakeExpired)
    );
    let second = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
    second.finish_authentication().unwrap();
}

#[tokio::test]
async fn dropping_an_unauthenticated_ipc_connection_disconnects_it_and_releases_its_slot() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let limit = MessageLimit::new(16).unwrap();
    let listener = IpcListener::bind(
        endpoint.clone(),
        limit,
        AdmissionLimits::new(1, Duration::from_secs(1)).unwrap(),
    )
    .unwrap();
    let client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    drop(listener.accept().await.unwrap());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), client.recv())
            .await
            .unwrap()
            .unwrap(),
        None
    );
    let _other_client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn dropping_pending_ipc_holds_admission_until_the_cancelled_timer_is_polled() {
    use futures_util::FutureExt;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let limit = MessageLimit::new(16).unwrap();
    let listener = runtime.block_on(async {
        IpcListener::bind(
            endpoint.clone(),
            limit,
            AdmissionLimits::new(1, Duration::from_secs(1)).unwrap(),
        )
        .unwrap()
    });
    let client = runtime
        .block_on(IpcTransport::connect(&endpoint, limit))
        .unwrap();
    let pending = runtime.block_on(listener.accept()).unwrap();
    let _other_client = std::os::unix::net::UnixStream::connect(endpoint.path()).unwrap();
    drop(pending);

    // This root future returns without letting the runtime poll the cancelled timer.
    assert!(
        runtime
            .block_on(async { listener.accept().now_or_never() })
            .is_none(),
        "admission must wait for the abandoned transport to be destroyed"
    );
    runtime.block_on(async {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), client.recv())
                .await
                .unwrap()
                .unwrap(),
            None
        );
        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .finish_authentication()
            .unwrap();
    });
}

#[tokio::test]
async fn ipc_can_send_while_another_task_waits_to_receive_and_close_wakes_that_task() {
    let (_temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let limit = MessageLimit::new(16).unwrap();
    let listener = IpcListener::bind(endpoint.clone(), limit, admission()).unwrap();
    let client = IpcTransport::connect(&endpoint, limit).await.unwrap();
    let server = listener
        .accept()
        .await
        .unwrap()
        .finish_authentication()
        .unwrap();
    let (received, ()) = tokio::join!(server.recv(), async {
        server.send(b"duplex").await.unwrap();
        assert_eq!(client.recv().await.unwrap(), Some(b"duplex".to_vec()));
        server.close().await.unwrap();
    });
    assert_eq!(received.unwrap(), None);
}

#[test]
fn ipc_rejects_public_or_symlink_directories_and_invalid_or_long_endpoints() {
    let (temporary, directory) = directory();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        PrivateIpcDirectory::open(temporary.path()).err(),
        Some(TransportError::UnsafeEndpoint)
    );
    assert_eq!(
        directory.endpoint("s").err(),
        Some(TransportError::UnsafeEndpoint)
    );
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let alias = temporary.path().with_extension("alias");
    std::os::unix::fs::symlink(temporary.path(), &alias).unwrap();
    assert_eq!(
        PrivateIpcDirectory::open(&alias).err(),
        Some(TransportError::UnsafeEndpoint)
    );
    fs::remove_file(alias).unwrap();
    for name in ["", "../s", "a/s", "abcdefghijklmnopq", "a\0b"] {
        assert_eq!(
            directory.endpoint(name).err(),
            Some(TransportError::InvalidConfiguration)
        );
    }
    assert_eq!(
        PrivateIpcDirectory::open(std::path::Path::new(&format!("/{}", "x".repeat(108)))).err(),
        Some(TransportError::InvalidConfiguration)
    );
}

#[tokio::test]
async fn ipc_rejects_zero_length_and_truncated_frames() {
    for bytes in [vec![0, 0, 0, 0], vec![0, 0], vec![0, 0, 0, 5, 1]] {
        let (_temporary, directory) = directory();
        let endpoint = directory.endpoint("s").unwrap();
        let listener = IpcListener::bind(
            endpoint.clone(),
            MessageLimit::new(16).unwrap(),
            admission(),
        )
        .unwrap();
        let mut client = UnixStream::connect(endpoint.path()).await.unwrap();
        let server = listener
            .accept()
            .await
            .unwrap()
            .finish_authentication()
            .unwrap();
        client.write_all(&bytes).await.unwrap();
        client.shutdown().await.unwrap();
        assert_eq!(server.recv().await, Err(TransportError::InvalidFrame));
    }
}

#[tokio::test]
async fn ipc_listener_cleanup_preserves_entries_in_both_a_moved_and_replacement_directory() {
    let (temporary, directory) = directory();
    let endpoint = directory.endpoint("s").unwrap();
    let listener =
        IpcListener::bind(endpoint.clone(), MessageLimit::envelope(), admission()).unwrap();
    let moved = temporary.path().with_extension("old");
    fs::rename(temporary.path(), &moved).unwrap();
    fs::create_dir(temporary.path()).unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
    drop(listener);
    assert!(endpoint.path().exists());
    assert!(moved.join("s").exists());
    drop(replacement);
    fs::remove_dir_all(moved).unwrap();
}
