//! Golden vectors pin the wire bytes. Round trips cover every operation and every reused payload
//! type, including optional fields in both states, because postcard has no field names.
use super::frame::{Assembler, DecodeError, FrameReader, Kind, encode_client, encode_server};
use super::interim::{self, Interim, InterimReply};
use super::screen::{
    Cells, Cursor, CursorPosition, Geometry, History, Row, Run, ScrollShift, Style,
    StyleDefinition, StyleRef,
};
use super::*;
use crate::hierarchy;
use crate::pane::{self, AttachmentClaim, FocusTarget, PaneSize, PaneTarget, directories};
use std::path::PathBuf;

fn client_bytes(message: &ClientMessage) -> Vec<u8> {
    let wire = encode_client(message).unwrap();
    assert_eq!(wire.frames.len(), 1);
    wire.frames.into_iter().next().unwrap()
}

fn server_bytes(message: &ServerMessage) -> Vec<u8> {
    let wire = encode_server(message).unwrap();
    assert_eq!(wire.frames.len(), 1);
    wire.frames.into_iter().next().unwrap()
}

fn client_round_trip(message: ClientMessage) {
    let wire = encode_client(&message).unwrap();
    let mut assembler = Assembler::default();
    let mut decoded = None;
    for frame in &wire.frames {
        assert!(decoded.is_none());
        decoded = assembler.client(RawFrame::parse(frame).unwrap()).unwrap();
    }
    assert_eq!(encode_client(&decoded.unwrap()).unwrap(), wire);
}

fn server_round_trip(message: ServerMessage) {
    let wire = encode_server(&message).unwrap();
    let mut assembler = Assembler::default();
    let mut decoded = None;
    for frame in &wire.frames {
        assert!(decoded.is_none());
        decoded = assembler.server(RawFrame::parse(frame).unwrap()).unwrap();
    }
    assert_eq!(encode_server(&decoded.unwrap()).unwrap(), wire);
}

fn request(operation: Operation) -> ClientMessage {
    ClientMessage::Request(Box::new(Request {
        id: RequestId(7),
        operation,
    }))
}

fn reply(body: ReplyBody) -> ServerMessage {
    ServerMessage::Reply(Box::new(Reply {
        id: RequestId(7),
        result: Ok(body),
    }))
}

fn size() -> PaneSize {
    PaneSize {
        columns: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    }
}

fn update() -> ScreenUpdate {
    ScreenUpdate {
        cursor: Cursor {
            position: Some(CursorPosition {
                column: 3,
                row: 0,
                width: 1,
            }),
            visible: true,
            blinking: false,
            shape: 0,
        },
        history: History { epoch: 1, total: 0 },
        shift: Some(ScrollShift {
            top: 0,
            bottom: 24,
            lines: 1,
        }),
        styles: vec![StyleDefinition {
            id: 1,
            style: Style {
                foreground: pane::Color::Palette(1),
                background: pane::Color::Default,
                underline_color: pane::Color::Default,
                flags: 1,
                underline: 0,
            },
        }],
        rows: vec![Row {
            index: 23,
            wrapped: false,
            runs: vec![Run {
                style: StyleRef::Id(1),
                link: None,
                text: "$ ".into(),
                cells: Cells::Narrow,
            }],
        }],
        ..ScreenUpdate::default()
    }
}

#[test]
fn golden_client_vectors() {
    assert_eq!(
        client_bytes(&ClientMessage::Ping(Ping { nonce: 1 })),
        [0, 0, 0, 2, 0x08, 1]
    );
    assert_eq!(
        client_bytes(&ClientMessage::FrameAck(FrameAck {
            channel: Channel(3),
            frame: 300,
        })),
        [0, 0, 0, 4, 0x03, 3, 0xac, 0x02]
    );
    assert_eq!(
        client_bytes(&request(Operation::Release {
            channel: Channel(2)
        })),
        [0, 0, 0, 4, 0x01, 7, 2, 2]
    );
    assert_eq!(
        client_bytes(&ClientMessage::Input(Input {
            channel: Channel(1),
            serial: 1,
            generation: 2,
            events: vec![
                InputEvent::Focus(FocusTarget::Terminal),
                InputEvent::CopyOrForward { modifiers: 1 },
            ],
        })),
        [0, 0, 0, 9, 0x02, 1, 1, 2, 2, 0, 2, 6, 1]
    );
    assert_eq!(
        client_bytes(&ClientMessage::View(View {
            channel: Channel(1),
            visible: true,
            max_hz: 120,
        })),
        [0, 0, 0, 4, 0x04, 1, 1, 120]
    );
}

#[test]
fn golden_server_vectors() {
    assert_eq!(
        server_bytes(&reply(ReplyBody::Done)),
        [0, 0, 0, 4, 0x41, 7, 0, 0]
    );
    assert_eq!(
        server_bytes(&ServerMessage::Reply(Box::new(Reply {
            id: RequestId(7),
            result: Err(Refusal::Stale),
        }))),
        [0, 0, 0, 4, 0x41, 7, 1, 2]
    );
    assert_eq!(
        server_bytes(&ServerMessage::Goodbye(Fatal::Stopping)),
        [0, 0, 0, 2, 0x48, 4]
    );
    assert_eq!(
        server_bytes(&ServerMessage::Frame(Box::new(Frame {
            channel: Channel(1),
            frame: 2,
            base: Some(1),
            input: 5,
            update: update(),
        }))),
        [
            0, 0, 0, 46, 0x43, // header
            1, 2, 1, 1, 5, // channel, frame, base, input
            1, 3, 0, 1, 1, 0, 0, // cursor
            1, 0, // history
            0, 0, 0, // geometry, modes, colors
            1, 0, 24, 2, // shift
            1, 1, 1, 1, 0, 0, 1, 0, // one style definition
            0, // links
            1, 23, 0, 1, 1, 1, 0, 2, b'$', b' ', 0, // one row with one run in style id 1
            0, 0, 0, 0, // graphics, accessibility, generation, reading
        ]
    );
}

#[test]
fn every_operation_round_trips() {
    let attachment = crate::attachments::Attachment {
        workspace: [1; 32],
        generation: 2,
    };
    let target = directories::Target {
        boot: [3; 32],
        pane: [4; 32],
        original: PathBuf::from("/home/a"),
    };
    let claims = [
        AttachmentClaim::Select { explicit: true },
        AttachmentClaim::Start { server: [5; 32] },
        AttachmentClaim::Repair {
            target: target.clone(),
            replacement: Some(PathBuf::from("/tmp")),
        },
        AttachmentClaim::Repair {
            target,
            replacement: None,
        },
        AttachmentClaim::TakeOver {
            target: PaneTarget {
                server: [6; 32],
                session: [7; 32],
            },
        },
    ];
    let style = pane::PresentationStyle {
        light: false,
        foreground: [1, 2, 3],
        background: [4, 5, 6],
        palette: [[7, 8, 9]; 16],
        cursor: [1, 1, 1],
        cursor_text: None,
        bold_as_bright: true,
    };
    let channel = Channel(9);
    let mut operations: Vec<Operation> = claims
        .into_iter()
        .map(|claim| {
            Operation::Claim(Claim {
                channel,
                attachment,
                pane: [8; 32],
                claim,
                size: size(),
                style: Some(style.clone()),
            })
        })
        .collect();
    operations.extend([
        Operation::ReleaseAttachment { attachment },
        Operation::Release { channel },
        Operation::Close { channel },
        Operation::ClearScrollback { channel },
        Operation::Style {
            channel,
            style: style.clone(),
        },
        Operation::Mutate {
            sequence: 3,
            mutation: hierarchy::Mutation::Close {
                target: hierarchy::CloseTarget::Pane {
                    workspace: [1; 32],
                    tab: [2; 32],
                    pane: [3; 32],
                },
                confirmation: hierarchy::CloseConfirmation {
                    boot: [4; 32],
                    panes: vec![hierarchy::ConfirmedPane {
                        id: [3; 32],
                        session: None,
                    }],
                    removes_tab: true,
                    removes_workspace: false,
                    own_controls: None,
                },
            },
        },
        Operation::Mutate {
            sequence: 4,
            mutation: hierarchy::Mutation::EndAllSessions {
                confirmation: inventory().close.unwrap(),
            },
        },
        Operation::Mutate {
            sequence: 5,
            mutation: hierarchy::Mutation::PinWorkspace {
                workspace: [1; 32],
                version: 2,
                directory: Some(PathBuf::from("/srv")),
            },
        },
        Operation::Resolve { sequence: 3 },
        Operation::ScrollbackPage {
            channel,
            epoch: 1,
            start: 1000,
            rows: limits::FIRST_PAGE_ROWS,
        },
        Operation::Graphics {
            channel,
            image: pane::graphics::ImageKey {
                image_id: 1,
                generation: 2,
            },
            offset: 4096,
        },
        Operation::Accessibility {
            channel,
            revision: Some(3),
            offset: 0,
            base: None,
        },
        Operation::AccessibilitySelection {
            channel,
            focus_generation: 2,
            selection: None,
        },
        Operation::EndAccessibility { channel },
        Operation::DirectoryRead(directories::Operation::Match {
            path: PathBuf::from("/a"),
            candidates: vec![PathBuf::from("/a/b")],
        }),
        Operation::DirectoryCreate(directories::Creation {
            path: PathBuf::from("/a/new"),
        }),
        Operation::WorktreeRead(crate::worktree::Read::Location {
            root: PathBuf::from("/repo"),
            location: "../wt".into(),
        }),
        Operation::Direct(crate::direct::Management::Status),
        Operation::Direct(crate::direct::Management::Invite),
        Operation::Shutdown,
        Operation::Interim(Interim::Poll { channel }),
        Operation::Interim(Interim::PollChanges {
            channel,
            base: Some(4),
        }),
        Operation::Interim(Interim::NativeEffects {
            channel,
            generation: 1,
        }),
        Operation::Interim(Interim::PresentationDemand {
            channel,
            visible: true,
        }),
        Operation::Interim(Interim::Read {
            channel,
            operation: pane::reading::Operation::Query {
                generation: 1,
                text: "needle".into(),
            },
        }),
        Operation::Interim(Interim::ReadingStatus { channel }),
        Operation::Interim(Interim::SelectionChunk {
            channel,
            generation: 1,
            offset: 0,
        }),
    ]);
    for operation in operations {
        client_round_trip(request(operation));
    }
}

#[test]
fn every_client_message_round_trips() {
    let key = pane::Key {
        physical: 36,
        action: 1,
        logical: "Enter".into(),
        text: Some("\r".into()),
        unshifted: Some('\r'),
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    };
    let pointer = pane::Pointer {
        screen_generation: 1,
        phase: pane::PointerPhase::Press,
        button: Some(pane::PointerButton::Left),
        x: 1.5,
        y: 2.5,
        modifiers: 0,
        shift_selection: false,
    };
    let wheel = pane::Wheel {
        screen_generation: 1,
        horizontal_steps: 0,
        vertical_steps: -3,
        phase: pane::WheelPhase::MomentumChanged,
        x: 0.0,
        y: 0.0,
        modifiers: 0,
        shift_selection: false,
    };
    client_round_trip(ClientMessage::Input(Input {
        channel: Channel(1),
        serial: 9,
        generation: 3,
        events: vec![
            InputEvent::Focus(FocusTarget::PasteConfirmation { id: 4 }),
            InputEvent::Key(key),
            InputEvent::Pointer(pointer),
            InputEvent::Wheel(wheel),
            InputEvent::Paste {
                text: "a\u{0}b".into(),
                last: true,
            },
            InputEvent::ResolvePaste {
                id: 4,
                confirm: false,
            },
            InputEvent::CopyOrForward { modifiers: 8 },
            InputEvent::Resize(size()),
        ],
    }));
    client_round_trip(ClientMessage::EffectDone(EffectDone {
        channel: Channel(1),
        generation: 2,
        effect: 3,
        completion: pane::effects::Completion::Text("clip".into()),
    }));
    client_round_trip(ClientMessage::Acknowledge(Acknowledge { sequence: 3 }));
    client_round_trip(ClientMessage::Cancel(Cancel { id: RequestId(2) }));
    client_round_trip(ClientMessage::Pong(Pong { nonce: u64::MAX }));
}

fn inventory() -> hierarchy::Inventory {
    use crate::repository::{
        DirectoryFacts, GitToolStatus, PinCheckState, Repository, RepositoryHead,
        RepositoryReadError, Snapshot,
    };
    let pane = |current: Option<PathBuf>| hierarchy::Pane {
        id: [3; 32],
        source: None,
        axis: hierarchy::Axis::Right,
        starting_directory: PathBuf::from("/home/a"),
        current_directory: current,
        repository_directory: None,
        presentation: None,
        confirmed_directory: Some(PathBuf::from("/home/a")),
        creator: Some([9; 32]),
    };
    hierarchy::Inventory {
        repositories: Some(Snapshot {
            boot: [1; 32],
            revision: 2,
            git: GitToolStatus::NotFound,
            directories: vec![DirectoryFacts {
                directory: PathBuf::from("/repo"),
                repository: Some(Repository {
                    root: PathBuf::from("/repo"),
                    head: RepositoryHead::Branch("main".into()),
                    commit: None,
                    upstream: None,
                    operation: None,
                    changes: Err(RepositoryReadError::Unavailable),
                    counting: None,
                    pull_request: None,
                }),
                worktrees: None,
                failure: None,
            }],
            pinned_worktrees: vec![],
            pin_check: PinCheckState::Checking,
        }),
        revision: 4,
        workspaces: vec![hierarchy::Workspace {
            id: [1; 32],
            name: "SpaceTerm".into(),
            name_version: 1,
            pin_version: 0,
            pinned_directory: None,
            root_tab: [2; 32],
            tabs: vec![hierarchy::Tab {
                worktree_membership_fixed: false,
                worktree: Some(PathBuf::from("/repo")),
                worktree_repository: None,
                id: [2; 32],
                root_pane: [3; 32],
                panes: vec![pane(None), pane(Some(PathBuf::from("/tmp")))],
            }],
        }],
        close: Some(hierarchy::CloseInventory {
            boot: [5; 32],
            sessions: vec![hierarchy::ConfirmedPane {
                id: [3; 32],
                session: Some([6; 32]),
            }],
            controls: None,
        }),
    }
}

#[test]
fn every_server_message_round_trips() {
    let restored = Restored {
        fresh: true,
        session_ended: false,
        recovery: None,
        directory_unavailable: false,
        server: [1; 32],
        workspace: [2; 32],
        tab: [3; 32],
        pane: [4; 32],
    };
    let takeover = Takeover {
        session_ended: false,
        target: PaneTarget {
            server: [1; 32],
            session: [2; 32],
        },
        controller: pane::control::Controller::Client {
            name: "Build Mac".into(),
            this_client: false,
        },
    };
    let lease = pane::Lease {
        server: [1; 32],
        session: [2; 32],
        control: [3; 32],
        connection: [4; 32],
    };
    let bodies = [
        ReplyBody::Claim(ClaimReply::Attached { lease }),
        ReplyBody::Claim(ClaimReply::Takeover(takeover.clone())),
        ReplyBody::Claim(ClaimReply::Restored(Box::new(restored))),
        ReplyBody::Outcome(hierarchy::RetainedOutcome {
            sequence: 3,
            mutation: hierarchy::Mutation::CreateTab {
                workspace: [1; 32],
                source: None,
            },
            outcome: hierarchy::Outcome::Accepted { target: [2; 32] },
        }),
        ReplyBody::Page(Page {
            epoch: 1,
            start: 200,
            rows: update().rows,
            styles: update().styles,
            links: vec![super::screen::LinkDefinition {
                id: 0,
                link: pane::effects::Hyperlink::Url("https://example.com".into()),
            }],
        }),
        ReplyBody::Graphics(Ok(pane::graphics::Chunk {
            image: pane::graphics::ImageKey {
                image_id: 1,
                generation: 1,
            },
            offset: 0,
            bytes: vec![0xff; 300 * 1024],
        })),
        ReplyBody::Graphics(Err(pane::graphics::Error::Missing)),
        ReplyBody::Accessibility(Err(pane::accessibility::Error::Pending)),
        ReplyBody::Directory(Ok(directories::Value::Listing {
            names: vec!["src".into()],
            truncated: false,
        })),
        ReplyBody::Directory(Ok(directories::Value::Match { index: None })),
        ReplyBody::Worktree(Ok(crate::worktree::ReadReply::Settings(
            crate::worktree::Settings::default(),
        ))),
        ReplyBody::Worktree(Ok(crate::worktree::ReadReply::ServerFacts(
            crate::worktree::ServerFacts::new("penguin", "Linux", "6.1").unwrap(),
        ))),
        ReplyBody::Direct(Ok(crate::direct::ManagementReply::Invitation(
            "spaceterm://pair/x".into(),
        ))),
        ReplyBody::Direct(Err(crate::direct::DirectError::Revoked)),
        ReplyBody::Interim(InterimReply::NativeEffects {
            request: Some(pane::effects::Request {
                id: 1,
                generation: 2,
                remaining_ms: 3,
                effect: pane::effects::Effect::ClipboardWrite {
                    target: pane::effects::ClipboardTarget::Standard,
                    text: "x".into(),
                },
            }),
            hidden_input: true,
        }),
        ReplyBody::Interim(InterimReply::Screen(Box::new(interim::ScreenFrame {
            frame: 2,
            base: Some(1),
            update: update(),
        }))),
        ReplyBody::Interim(InterimReply::ReadingReady {
            generation: 1,
            selection_revision: 2,
            selection_present: true,
        }),
        ReplyBody::Done,
    ];
    for body in bodies {
        server_round_trip(reply(body));
    }
    let events = [
        PaneEvent::Takeover(takeover),
        PaneEvent::Ended,
        PaneEvent::SessionEnded,
        PaneEvent::Failed,
        PaneEvent::Effect(pane::effects::Request {
            id: 1,
            generation: 2,
            remaining_ms: 3,
            effect: pane::effects::Effect::CommandFinished {
                exit_status: Some(-1),
                duration_ms: 5,
            },
        }),
        PaneEvent::Input {
            serial: 4,
            outcome: InputOutcome::Discarded(Refusal::Stale),
        },
        PaneEvent::Input {
            serial: 5,
            outcome: InputOutcome::Paste(pane::PasteOutcome::Rejected(
                pane::PasteRejection::TooLarge { limit: 1 },
            )),
        },
        PaneEvent::Accessibility { revision: 3 },
        PaneEvent::HiddenInput(true),
    ];
    for event in events {
        server_round_trip(ServerMessage::Pane(PaneMessage {
            channel: Channel(2),
            event,
        }));
    }
    let mut keyframe = update();
    keyframe.shift = None;
    keyframe.geometry = Some(Geometry {
        columns: 80,
        rows: 24,
        cell_pixels: Some((8, 16)),
    });
    keyframe.modes = Some(super::screen::Modes::default());
    keyframe.colors = Some(super::screen::Colors {
        foreground: [1; 3],
        background: [2; 3],
        cursor: [3; 3],
        cursor_text: [4; 3],
        palette: vec![[5; 3]; 256],
        foreground_overridden: false,
        background_overridden: true,
        cursor_overridden: false,
        palette_overrides: [0, 1, 0, u64::MAX],
    });
    keyframe.graphics = Some(Err(pane::graphics::Error::Capacity));
    keyframe.rows[0].runs.push(Run {
        style: StyleRef::Default,
        link: Some(0),
        text: "界a".into(),
        cells: Cells::Explicit(vec![3, 0, 1]),
    });
    server_round_trip(ServerMessage::Frame(Box::new(Frame {
        channel: Channel(1),
        frame: 1,
        base: None,
        input: 0,
        update: keyframe,
    })));
    server_round_trip(ServerMessage::Inventory(Box::new(InventoryRevision {
        inventory: inventory(),
        next_sequence: 9,
    })));
    server_round_trip(ServerMessage::Ping(Ping { nonce: 1 }));
    server_round_trip(ServerMessage::Pong(Pong { nonce: 1 }));
}

#[test]
fn large_messages_continue_across_frames_and_bulk_replies_use_small_chunks() {
    let wire = encode_server(&reply(ReplyBody::Graphics(Ok(pane::graphics::Chunk {
        image: pane::graphics::ImageKey {
            image_id: 1,
            generation: 1,
        },
        offset: 0,
        bytes: vec![1; 200 * 1024],
    }))))
    .unwrap();
    assert_eq!(wire.kind, Kind::BulkReply);
    assert_eq!(wire.frames.len(), 4);
    assert!(
        wire.frames
            .iter()
            .all(|frame| frame.len() <= limits::HEADER_BYTES + limits::BULK_CHUNK_BYTES)
    );

    let mut keyframe = update();
    keyframe.rows = (0..200)
        .map(|index| Row {
            index,
            wrapped: false,
            // 512 cells of 16 bytes each, such as long grapheme clusters.
            runs: vec![Run {
                style: StyleRef::Default,
                link: None,
                text: "x".repeat(8 * 1024),
                cells: Cells::Explicit(vec![16; 512]),
            }],
        })
        .collect();
    keyframe.shift = None;
    let wire = encode_server(&ServerMessage::Frame(Box::new(Frame {
        channel: Channel(1),
        frame: 1,
        base: None,
        input: 0,
        update: keyframe,
    })))
    .unwrap();
    assert_eq!(wire.frames.len(), 2);
    assert!(
        wire.frames
            .iter()
            .all(|frame| frame.len() <= limits::MAX_FRAME_BYTES)
    );

    // Frames of other kinds may interleave between continued frames, as L0 does.
    let mut reader = FrameReader::default();
    reader.feed(&wire.frames[0]);
    reader.feed(&server_bytes(&ServerMessage::Ping(Ping { nonce: 5 })));
    reader.feed(&wire.frames[1][..10]);
    let mut assembler = Assembler::default();
    let first = reader.next_frame().unwrap().unwrap();
    assert!(assembler.server(first).unwrap().is_none());
    let ping = reader.next_frame().unwrap().unwrap();
    assert!(matches!(
        assembler.server(ping).unwrap(),
        Some(ServerMessage::Ping(Ping { nonce: 5 }))
    ));
    assert!(reader.next_frame().unwrap().is_none());
    assert!(reader.partial());
    reader.feed(&wire.frames[1][10..]);
    let last = reader.next_frame().unwrap().unwrap();
    assert!(matches!(
        assembler.server(last).unwrap(),
        Some(ServerMessage::Frame(_))
    ));
}

#[test]
fn decoding_refuses_malformed_oversized_misdirected_and_out_of_bounds_frames() {
    let parse = |bytes: &[u8]| RawFrame::parse(bytes);
    assert_eq!(parse(&[0, 0, 0, 0, 1]), Err(DecodeError::Malformed));
    assert_eq!(parse(&[0, 0, 0, 2, 0x30, 1]), Err(DecodeError::UnknownKind));
    assert_eq!(parse(&[0, 0, 0, 3, 0x08, 1]), Err(DecodeError::Malformed));
    assert_eq!(parse(&[0, 0xff, 0, 0, 0x43]), Err(DecodeError::Oversized));
    assert_eq!(
        parse(&[0, 1, 0, 2, 0x42]),
        Err(DecodeError::Oversized),
        "a bulk reply frame is bounded by its chunk size"
    );

    let mut assembler = Assembler::default();
    let ping = RawFrame::parse(&server_bytes(&ServerMessage::Ping(Ping { nonce: 1 }))).unwrap();
    assert_eq!(
        assembler.client(ping).unwrap_err(),
        DecodeError::WrongDirection
    );
    let trailing = RawFrame {
        kind: Kind::ClientPing as u8,
        body: vec![1, 0],
    };
    assert_eq!(
        assembler.client(trailing).unwrap_err(),
        DecodeError::Malformed
    );

    let input = |serial, events: Vec<InputEvent>| {
        RawFrame::parse(&client_bytes(&ClientMessage::Input(Input {
            channel: Channel(1),
            serial,
            generation: 1,
            events,
        })))
        .unwrap()
    };
    let key = InputEvent::CopyOrForward { modifiers: 0 };
    for frame in [
        input(0, vec![key.clone()]),
        input(1, vec![]),
        input(1, vec![key.clone(); limits::MAX_INPUT_EVENTS + 1]),
        input(1, vec![key, InputEvent::Focus(FocusTarget::Terminal)]),
    ] {
        assert_eq!(
            assembler.client(frame).unwrap_err(),
            DecodeError::OutOfBounds
        );
    }
    let view = RawFrame::parse(&client_bytes(&ClientMessage::View(View {
        channel: Channel(1),
        visible: true,
        max_hz: 61,
    })))
    .unwrap();
    assert_eq!(
        assembler.client(view).unwrap_err(),
        DecodeError::OutOfBounds
    );
    let page = RawFrame::parse(&client_bytes(&request(Operation::ScrollbackPage {
        channel: Channel(1),
        epoch: 0,
        start: 0,
        rows: limits::MAX_PAGE_ROWS + 1,
    })))
    .unwrap();
    assert_eq!(
        assembler.client(page).unwrap_err(),
        DecodeError::OutOfBounds
    );

    let mut continued =
        RawFrame::parse(&client_bytes(&ClientMessage::Ping(Ping { nonce: 1 }))).unwrap();
    continued.kind |= 0x80;
    let mut assembler = Assembler::default();
    let mut result = Ok(None);
    for _ in 0..=limits::MAX_CONTROL_BYTES {
        result = assembler.client(continued.clone());
        if result.is_err() {
            break;
        }
    }
    assert_eq!(
        result.unwrap_err(),
        DecodeError::Oversized,
        "continued frames are bounded by the message limit of their kind"
    );
}

#[test]
fn screen_updates_refuse_inconsistent_tables_cells_and_cursors() {
    let refused = |change: &dyn Fn(&mut ScreenUpdate)| {
        let mut update = update();
        change(&mut update);
        let frame = RawFrame::parse(&server_bytes(&ServerMessage::Frame(Box::new(Frame {
            channel: Channel(1),
            frame: 2,
            base: Some(1),
            input: 0,
            update,
        }))))
        .unwrap();
        match Assembler::default().server(frame) {
            Ok(_) => false,
            Err(error) => error == DecodeError::OutOfBounds,
        }
    };
    assert!(!refused(&|_| {}));
    assert!(refused(&|update| {
        let duplicate = update.styles[0];
        update.styles.push(duplicate);
    }));
    assert!(refused(&|update| update.styles[0].style.flags = 1 << 9));
    assert!(refused(&|update| update.styles[0].style.underline = 6));
    assert!(refused(&|update| update.styles[0].id = 0));
    assert!(refused(&|update| {
        update.rows[0].runs[0].style = StyleRef::Id(limits::MAX_STYLES as u16 + 1)
    }));
    assert!(refused(&|update| {
        let mut style = update.styles[0].style;
        style.flags = u16::MAX;
        update.rows[0].runs[0].style = StyleRef::Inline(style);
    }));
    assert!(refused(&|update| {
        update.rows[0].runs[0] = Run {
            style: StyleRef::Default,
            link: None,
            text: "ab".into(),
            cells: Cells::Explicit(vec![1]),
        }
    }));
    assert!(refused(&|update| update.cursor.shape = 4));
    assert!(refused(&|update| {
        update.cursor.position = Some(CursorPosition {
            column: 0,
            row: 0,
            width: 3,
        })
    }));
    assert!(refused(&|update| {
        update.links = vec![super::screen::LinkDefinition {
            id: 0,
            link: pane::effects::Hyperlink::Url("x".repeat(limits::MAX_LINK_BYTES + 1)),
        }]
    }));
    assert!(refused(&|update| {
        update.rows[0].runs[0] = Run {
            style: StyleRef::Default,
            link: None,
            text: "界".into(),
            cells: Cells::Explicit(vec![2, 1]),
        }
    }));
    assert!(refused(&|update| {
        update.rows[0].runs[0] = Run {
            style: StyleRef::Default,
            link: None,
            text: "x".repeat(limits::MAX_CELL_BYTES + 1),
            cells: Cells::Explicit(vec![limits::MAX_CELL_BYTES as u16 + 1]),
        }
    }));
    assert!(refused(&|update| {
        update.rows[0].runs[0] = Run {
            style: StyleRef::Default,
            link: None,
            text: "x".repeat(usize::from(pane::MAX_COLUMNS) + 1),
            cells: Cells::Narrow,
        }
    }));
}

#[test]
fn a_scroll_shift_moves_its_region_and_blanks_the_rows_it_vacates() {
    let mut rows = vec![0, 1, 2, 3, 4, 5];
    let up = ScrollShift {
        top: 1,
        bottom: 5,
        lines: 2,
    };
    up.apply(&mut rows, || -1).unwrap();
    assert_eq!(rows, [0, 3, 4, -1, -1, 5]);
    let down = ScrollShift { lines: -1, ..up };
    down.apply(&mut rows, || -2).unwrap();
    assert_eq!(rows, [0, -2, 3, 4, -1, 5]);
    let outside = ScrollShift {
        top: 4,
        bottom: 7,
        lines: 1,
    };
    assert!(outside.apply(&mut rows, || 0).is_none());
    let spacer = Run {
        style: StyleRef::Default,
        link: None,
        text: "界a".into(),
        cells: Cells::Explicit(vec![3, 0, 1]),
    };
    assert_eq!(spacer.cells().collect::<Vec<_>>(), [0..3, 3..3, 3..4]);
    assert_eq!(spacer.width(), 3);
}

#[test]
fn a_row_with_too_many_cells_is_refused_before_its_lengths_are_walked() {
    // A frame of zero-length cells costs one byte per cell. The column bound must reject it from
    // the length of the list alone, without building anything per cell.
    let mut update = update();
    update.rows[0].runs[0] = Run {
        style: StyleRef::Default,
        link: None,
        text: String::new(),
        cells: Cells::Explicit(vec![0; 4 * 1024 * 1024]),
    };
    assert_eq!(update.validate(), Err(Fatal::ProtocolViolation));
    update.rows[0].runs[0].cells = Cells::Explicit(vec![0; usize::from(pane::MAX_COLUMNS)]);
    assert_eq!(update.validate(), Ok(()));
}
