//! The network boundary drops a real successful start reply. Each client runs in a fresh
//! test process, with fresh PaneControls, while the production host and its PTY stay alive.
use super::*;
use spaceterm_server::{hierarchy::Mutation, pane::PaneService as _};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    time::Instant,
};

const CHILD: &str = "terminal::server_session::claim::process_tests::client_process";
const SOCKET: &str = "SPACETERM_START_TEST_SOCKET";

fn write<T: serde::Serialize>(stream: &mut UnixStream, value: &T) {
    let bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= pane::MAX_TERMINAL_FRAME);
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}
fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Option<T> {
    let mut length = [0; 4];
    if stream.read_exact(&mut length).is_err() {
        return None;
    }
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= pane::MAX_TERMINAL_FRAME);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    Some(serde_json::from_slice(&bytes).unwrap())
}
struct Process(Child);
impl Process {
    fn start(socket: &std::path::Path, phase: &str) -> Self {
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", CHILD, "--nocapture"])
                .env(SOCKET, socket)
                .env("SPACETERM_START_TEST_PHASE", phase)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "client subprocess failed");
                return;
            }
            assert!(Instant::now() < deadline, "client subprocess did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Socket(std::path::PathBuf);
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn accept(listener: &UnixListener) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => panic!("fixture socket failed"),
        }
        assert!(
            Instant::now() < deadline,
            "client subprocess did not connect"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_fresh_client_process_observes_a_successful_start_after_its_reply_is_lost() {
    for start_by_takeover in [false, true] {
        let mut host = crate::runtime::directory_testing::terminal_host();
        if start_by_takeover {
            assert!(matches!(
                host.request(
                    [2; 32],
                    [2; 32],
                    Request::Mutate {
                        sequence: 1,
                        mutation: Mutation::CreateWorkspace {
                            name: "Start recovery".into(),
                            directory: None
                        },
                    }
                ),
                Reply::Outcome(_)
            ));
        } else {
            host.prepare_initial_organization().unwrap();
        }
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let random = u64::from_ne_bytes(random);
        let socket = Socket(std::env::temp_dir().join(format!("st-start-{random:x}.s")));
        let listener = UnixListener::bind(&socket.0).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut first = Process::start(&socket.0, "lose-start");
        let mut stream = accept(&listener);
        let mut original = None;
        let mut starts = 0;
        while let Some(request) = read::<Request>(&mut stream) {
            let produces_start = matches!(
                request,
                Request::StartRestored { .. } | Request::TakeOver { .. }
            );
            let takeover_request = matches!(request, Request::TakeOver { .. });
            let reply = host.request([1; 32], [3; 32], request);
            if produces_start {
                let Reply::Attached { lease } = reply else {
                    panic!("original start failed")
                };
                assert_eq!(takeover_request, start_by_takeover);
                starts += 1;
                original = Some(lease);
                // The PTY exists. Its successful acknowledgement never reaches the client.
                break;
            }
            write(&mut stream, &reply);
        }
        drop(stream);
        host.disconnected([3; 32]);
        first.finish();
        let original = original.expect("the first process must start a real shell");
        assert_eq!(starts, 1);
        // Another client deliberately takes control while the first process is gone.
        // The reopened client must observe that live Terminal Session before Take Over.
        if start_by_takeover {
            assert!(matches!(host.request([2; 32], [4; 32], Request::TakeOver {
                target: original.into(), size: PaneSize { columns: 80, rows: 24, cell_width: 8, cell_height: 16 }, style: None,
            }), Reply::Attached { lease } if lease.session == original.session));
        }
        let mut reopened = Process::start(&socket.0, "reopen");
        let mut stream = accept(&listener);
        let mut observed = false;
        let mut attached = false;
        let mut takeover = false;
        while let Some(request) = read::<Request>(&mut stream) {
            if !matches!(request, Request::Inventory) && !observed {
                assert!(
                    matches!(
                        request,
                        Request::SelectPane {
                            explicit: false,
                            ..
                        }
                    ),
                    "a fresh process must first observe the Pane"
                );
                observed = true;
            }
            assert!(
                !matches!(request, Request::StartRestored { .. }),
                "reopening must not submit a replacement start"
            );
            if let Request::TakeOver { target, .. } = &request {
                assert_eq!(
                    target.session, original.session,
                    "Take Over must name the existing Terminal Session"
                );
                takeover = true;
            }
            let reply = host.request([1; 32], [5; 32], request);
            if let Reply::Attached { lease } = &reply {
                assert_eq!(lease.server, original.server);
                assert_eq!(lease.session, original.session);
                attached = true;
            }
            write(&mut stream, &reply);
        }
        host.disconnected([5; 32]);
        reopened.finish();
        assert!(observed && attached);
        assert_eq!(takeover, start_by_takeover);
        let Reply::Inventory { inventory, .. } = host.request([1; 32], [6; 32], Request::Inventory)
        else {
            panic!("inventory missing")
        };
        assert_eq!(
            inventory.close.unwrap().sessions[0].session,
            Some(original.session)
        );
        host.shutdown().unwrap();
    }
}

#[test]
#[ignore = "child-process entry point for successful-start/lost-acknowledgement recovery"]
fn client_process() {
    let mut stream =
        UnixStream::connect(std::env::var_os(SOCKET).expect("fixture socket missing")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write(&mut stream, &Request::Inventory);
    let Some(Reply::Inventory { inventory, .. }) = read(&mut stream) else {
        panic!("inventory missing")
    };
    let pane = inventory.workspaces[0].tabs[0].panes[0].id;
    // No parent process control or recovery state is inherited by this executable.
    let controls = PaneControls::default();
    let control = controls.pane(pane);
    let presenter = control.reserve().activate(&AtomicBool::new(true));
    let mut state = DevelopmentPaneState::new();
    state.connection_recovery = Arc::clone(&control.recovery);
    state.select();
    let result = ClaimWorker {
        state: &state,
        pane_id: pane,
        alive: &AtomicBool::new(true),
        attachment: &None,
        size: &Mutex::new(PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }),
        appearance: &Mutex::new(TerminalAppearanceUpdate::builtin().unwrap()),
        directory_recovery: true,
        attached_session: &mut false,
        takeover: &mut false,
        paste_unconfirmed: &mut false,
    }
    .run(
        |request| {
            presenter.send(request, |request| {
                exchange(&state, pane, request, |request| {
                    write(&mut stream, request);
                    let reply = read::<Reply>(&mut stream).ok_or(ServerError::Protocol)?;
                    if matches!(reply, Reply::Takeover { .. }) {
                        state.take_over();
                    }
                    match reply {
                        Reply::Refused(refusal) => Err(refusal.into()),
                        reply => Ok(reply),
                    }
                })
            })
        },
        || {},
    );
    if std::env::var("SPACETERM_START_TEST_PHASE").unwrap() == "lose-start" {
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(state.start_unconfirmed());
    } else {
        assert!(result.unwrap().is_some());
        assert!(!state.start_unconfirmed());
    }
}
