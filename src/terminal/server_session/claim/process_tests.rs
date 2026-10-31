//! The network boundary drops a real successful start reply. Each client runs in a fresh
//! test process, with fresh PaneControls, while the production host and its PTY stay alive.
use super::*;
use crate::runtime::HostCommands as _;
use spaceterm_server::hierarchy::Mutation;
use spaceterm_server::protocol;
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    time::Instant,
};

const CHILD: &str = "terminal::server_session::claim::process_tests::client_process";
const SOCKET: &str = "SPACETERM_START_TEST_SOCKET";

/// The Pane commands this fixture carries between its processes. `Request` stays in process, so
/// the fixture names the few a claim sends.
#[derive(serde::Serialize, serde::Deserialize)]
enum Carried {
    Inventory,
    SelectPane {
        pane: spaceterm_server::hierarchy::Id,
        explicit: bool,
        size: PaneSize,
        style: Option<pane::PresentationStyle>,
    },
    StartRestored {
        server: [u8; 32],
        pane: [u8; 32],
        size: PaneSize,
        style: Option<pane::PresentationStyle>,
    },
    RepairDirectory {
        target: pane::directories::Target,
        replacement: Option<std::path::PathBuf>,
        size: PaneSize,
        style: Option<pane::PresentationStyle>,
    },
    TakeOver {
        target: pane::PaneTarget,
        size: PaneSize,
        style: Option<pane::PresentationStyle>,
    },
}
impl Carried {
    fn of(request: &Request) -> Self {
        match request.clone() {
            Request::Inventory => Self::Inventory,
            Request::SelectPane {
                pane,
                explicit,
                size,
                style,
            } => Self::SelectPane {
                pane,
                explicit,
                size,
                style,
            },
            Request::StartRestored {
                server,
                pane,
                size,
                style,
            } => Self::StartRestored {
                server,
                pane,
                size,
                style,
            },
            Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => Self::RepairDirectory {
                target,
                replacement,
                size,
                style,
            },
            Request::TakeOver {
                target,
                size,
                style,
            } => Self::TakeOver {
                target,
                size,
                style,
            },
            _ => panic!("a claim sent a command the fixture does not carry"),
        }
    }
    fn request(self) -> Request {
        match self {
            Self::Inventory => Request::Inventory,
            Self::SelectPane {
                pane,
                explicit,
                size,
                style,
            } => Request::SelectPane {
                pane,
                explicit,
                size,
                style,
            },
            Self::StartRestored {
                server,
                pane,
                size,
                style,
            } => Request::StartRestored {
                server,
                pane,
                size,
                style,
            },
            Self::RepairDirectory {
                target,
                replacement,
                size,
                style,
            } => Request::RepairDirectory {
                target,
                replacement,
                size,
                style,
            },
            Self::TakeOver {
                target,
                size,
                style,
            } => Request::TakeOver {
                target,
                size,
                style,
            },
        }
    }
}
/// The replies to [`Carried`] commands.
#[derive(serde::Serialize, serde::Deserialize)]
enum CarriedReply {
    Inventory(spaceterm_server::hierarchy::Inventory),
    Claim(Result<protocol::ClaimReply, pane::Refusal>),
}
impl CarriedReply {
    fn of(reply: Reply) -> Self {
        match reply {
            Reply::Inventory { inventory, .. } => Self::Inventory(inventory),
            Reply::Attached { lease } => Self::Claim(Ok(protocol::ClaimReply::Attached { lease })),
            Reply::Takeover {
                session_ended,
                target,
                controller,
            } => Self::Claim(Ok(protocol::ClaimReply::Takeover(protocol::Takeover {
                session_ended,
                target,
                controller,
            }))),
            Reply::Restored {
                fresh,
                session_ended,
                recovery,
                directory_unavailable,
                server,
                workspace,
                tab,
                pane,
            } => Self::Claim(Ok(protocol::ClaimReply::Restored(Box::new(
                protocol::Restored {
                    fresh,
                    session_ended,
                    recovery,
                    directory_unavailable,
                    server,
                    workspace,
                    tab,
                    pane,
                },
            )))),
            Reply::Refused(refusal) => Self::Claim(Err(refusal)),
            _ => panic!("the host sent a reply the fixture does not carry"),
        }
    }
    fn reply(self) -> Reply {
        match self {
            Self::Inventory(inventory) => Reply::Inventory {
                inventory,
                next_sequence: 1,
            },
            Self::Claim(Ok(protocol::ClaimReply::Attached { lease })) => Reply::Attached { lease },
            Self::Claim(Ok(protocol::ClaimReply::Takeover(takeover))) => Reply::Takeover {
                session_ended: takeover.session_ended,
                target: takeover.target,
                controller: takeover.controller,
            },
            Self::Claim(Ok(protocol::ClaimReply::Restored(restored))) => Reply::Restored {
                fresh: restored.fresh,
                session_ended: restored.session_ended,
                recovery: restored.recovery,
                directory_unavailable: restored.directory_unavailable,
                server: restored.server,
                workspace: restored.workspace,
                tab: restored.tab,
                pane: restored.pane,
            },
            Self::Claim(Err(refusal)) => Reply::Refused(refusal),
        }
    }
}

fn write<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).unwrap();
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)
}
fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> std::io::Result<Option<T>> {
    let mut length = [0; 4];
    // EOF before a frame is the intentional lost acknowledgement. A timeout, partial frame
    // or other transport failure must fail the fixture instead of masquerading as that EOF.
    match stream.read_exact(&mut length[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    stream.read_exact(&mut length[1..])?;
    let length = u32::from_be_bytes(length) as usize;
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
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
                // Darwin inherits the listener's nonblocking mode. Only accept is polled;
                // framed exchanges must block until bytes arrive or their timeout expires.
                stream.set_nonblocking(false).unwrap();
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

fn prove_shell_started(host: &mut crate::runtime::SharedWorkspaceHost, lease: Lease) {
    // Attached can precede the worker's PTY startup. Execute a shell command and observe its
    // output before dropping that acknowledgement, so the test proves a successful shell start.
    for key in [
        KeyInput::text_input("stty -echo; printf '\\033[2J\\033[HST-START-SUCCEEDED\\n'"),
        KeyInput {
            physical_key: PhysicalKey::Enter,
            logical_key: "enter".into(),
            text: None,
            ..KeyInput::text_input("")
        },
    ] {
        assert!(matches!(
            crate::runtime::directory_testing::type_keys(host, lease, vec![encode_key(key)]),
            Reply::Unchanged
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if crate::terminal::server_session::screen_decoder::observed_lines(host, lease)
            .iter()
            .any(|row| row.trim_end() == "ST-START-SUCCEEDED")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the original shell did not execute its command"
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
                            directory: None,
                            control: spaceterm_server::hierarchy::CreateControl::Creator,
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
        while let Some(request) = read::<Carried>(&mut stream).unwrap() {
            let request = request.request();
            let produces_start = matches!(
                request,
                Request::StartRestored { .. }
                    | Request::RepairDirectory { .. }
                    | Request::SelectPane { explicit: true, .. }
                    | Request::TakeOver { .. }
            );
            let takeover_request = matches!(request, Request::TakeOver { .. });
            let reply = host.request([1; 32], [3; 32], request);
            if produces_start {
                let Reply::Attached { lease } = reply else {
                    panic!("original start failed")
                };
                assert_eq!(takeover_request, start_by_takeover);
                starts += 1;
                prove_shell_started(&mut host, lease);
                original = Some(lease);
                // The PTY exists. Its successful acknowledgement never reaches the client.
                break;
            }
            write(&mut stream, &CarriedReply::of(reply)).unwrap();
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
        let mut session_observed = false;
        let mut attached = false;
        let mut takeover = false;
        while let Some(request) = read::<Carried>(&mut stream).unwrap() {
            let request = request.request();
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
                !matches!(
                    request,
                    Request::StartRestored { .. }
                        | Request::RepairDirectory { .. }
                        | Request::SelectPane { explicit: true, .. }
                ),
                "reopening must not submit a replacement start"
            );
            if let Request::TakeOver { target, .. } = &request {
                assert!(
                    session_observed,
                    "Take Over must follow observation of the existing Terminal Session"
                );
                assert_eq!(
                    target.session, original.session,
                    "Take Over must name the existing Terminal Session"
                );
                takeover = true;
            }
            let reply = host.request([1; 32], [5; 32], request);
            match &reply {
                Reply::Attached { lease } => {
                    assert_eq!(lease.server, original.server);
                    assert_eq!(lease.session, original.session);
                    session_observed = true;
                    attached = true;
                }
                Reply::Takeover { target, .. } => {
                    assert_eq!(target.server, original.server);
                    assert_eq!(target.session, original.session);
                    session_observed = true;
                }
                Reply::Restored { .. } => {
                    panic!("reopening must observe the original live Terminal Session")
                }
                _ => {}
            }
            write(&mut stream, &CarriedReply::of(reply)).unwrap();
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
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write(&mut stream, &Carried::Inventory).unwrap();
    let Some(CarriedReply::Inventory(inventory)) = read(&mut stream).unwrap() else {
        panic!("inventory missing")
    };
    let pane = inventory.workspaces[0].tabs[0].panes[0].id;
    // No parent process control or recovery state is inherited by this executable.
    let controls = PaneControls::default();
    let control = controls.pane(pane);
    let presenter = control.reserve().activate(&AtomicBool::new(true));
    let mut state = DevelopmentPaneState::new();
    state.connection_recovery = Arc::clone(&control.recovery);
    let losing_start = std::env::var("SPACETERM_START_TEST_PHASE").unwrap() == "lose-start";
    let mut pane_observed = false;
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
            // Connecting is also the initial status, before the server has reported control.
            // Model first use only after ClaimWorker has processed that initial observation.
            match state.status() {
                DevelopmentPaneStatus::Connecting if losing_start && pane_observed => {
                    state.select()
                }
                DevelopmentPaneStatus::Restored if losing_start => state.start_restored(),
                DevelopmentPaneStatus::Takeover => state.take_over(),
                _ => {}
            }
            let result = presenter.send(request, |request| {
                exchange(&state, pane, request, |request| {
                    write(&mut stream, &Carried::of(request)).map_err(|_| ServerError::Protocol)?;
                    let reply = read::<CarriedReply>(&mut stream)
                        .map_err(|_| ServerError::Protocol)?
                        .ok_or(ServerError::Protocol)?
                        .reply();
                    match reply {
                        Reply::Refused(refusal) => Err(refusal.into()),
                        reply => Ok(reply),
                    }
                })
            });
            pane_observed |= result.is_ok();
            result
        },
        || {},
    );
    if losing_start {
        assert!(matches!(result, Err(ServerError::Protocol)));
        assert!(state.start_unconfirmed());
    } else {
        assert!(result.unwrap().is_some());
        assert!(!state.start_unconfirmed());
    }
}
