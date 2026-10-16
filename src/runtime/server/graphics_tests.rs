use super::tests::{attach, command, host, size};
use super::*;
use pane::graphics::{CHUNK_BYTES, Error, ImageKey};
use std::time::Instant;

fn image_screen(host: &mut DevelopmentPaneHost, lease: Lease) -> Box<Screen> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match host.request([1; 32], lease.connection, Request::Poll { lease }) {
            Reply::Screen(screen)
                if screen
                    .graphics
                    .as_ref()
                    .is_some_and(|g| g.as_ref().is_ok_and(|g| !g.images.is_empty())) =>
            {
                return screen;
            }
            Reply::Screen(_) | Reply::Unchanged => {}
            other => panic!("graphics screen refused: {other:?}"),
        }
        assert!(Instant::now() < deadline, "graphics screen did not arrive");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn chunk(host: &mut DevelopmentPaneHost, lease: Lease, image: ImageKey, offset: usize) -> Reply {
    host.request(
        [1; 32],
        lease.connection,
        Request::GraphicsChunk {
            lease,
            image,
            offset,
        },
    )
}

#[test]
fn retained_graphics_use_measured_pixels_and_fence_takeover_reconnect_and_replacement() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[H\\033_Ga=T,t=d,f=32,i=7,p=3,s=1,v=1,c=2,r=1,C=1;AQIDBA==\\033\\\\'",
    );
    let first = image_screen(&mut host, lease);
    let graphics = first.graphics.unwrap().unwrap();
    assert_eq!(first.cell_pixels, Some((8, 18)));
    assert_eq!(graphics.placements[0].destination_width, 16);
    let key = graphics.images[0].key;
    assert!(
        matches!(chunk(&mut host, lease, key, 0), Reply::GraphicsChunk(Ok(c)) if c.bytes == [1,2,3,4])
    );
    assert!(matches!(
        chunk(&mut host, lease, key, usize::MAX),
        Reply::GraphicsChunk(Err(Error::Malformed))
    ));
    assert!(matches!(
        chunk(
            &mut host,
            lease,
            ImageKey {
                image_id: 999,
                ..key
            },
            0
        ),
        Reply::GraphicsChunk(Err(Error::Missing))
    ));
    host.disconnected(lease.connection);
    assert!(matches!(
        chunk(&mut host, lease, key, 0),
        Reply::Refused(Refusal::Stale)
    ));
    let resumed = attach(&mut host, 2, false);
    let resumed_screen = image_screen(&mut host, resumed);
    assert_eq!(resumed_screen.graphics.unwrap().unwrap(), graphics);
    assert_eq!(resumed.session, lease.session);
    let Reply::Attached { lease: taken } = host.request(
        [2; 32],
        [3; 32],
        Request::TakeOver {
            target: resumed.into(),
            size: size(),
            style: None,
        },
    ) else {
        panic!("takeover failed")
    };
    assert!(matches!(
        chunk(&mut host, resumed, key, 0),
        Reply::Refused(Refusal::Stale)
    ));
    // A new control grant may read the existing image only through its own lease.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(host.request([2;32], [3;32], Request::GraphicsChunk { lease: taken, image: key, offset: 0 }), Reply::GraphicsChunk(Ok(c)) if c.bytes == [1,2,3,4])
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    host.request([2; 32], [3; 32], Request::Close { lease: taken });
    let replacement = attach(&mut host, 4, true);
    assert_ne!(replacement.session, taken.session);
    assert!(matches!(
        chunk(&mut host, replacement, key, 0),
        Reply::GraphicsChunk(Err(Error::Missing))
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [3; 32],
            Request::GraphicsChunk {
                lease: taken,
                image: key,
                offset: 0
            }
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}

#[test]
fn graphics_transfers_are_pulled_in_bounded_chunks_without_retaining_a_transfer_queue() {
    let _guard = crate::terminal::graphics::test_lock();
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    command(
        &mut host,
        1,
        lease,
        "stty -echo; printf '\\033[2J\\033[H\\033_Ga=T,t=d,f=32,i=9,s=384,v=256,C=1;'; head -c 393216 /dev/zero | base64 | tr -d '\\n'; printf '\\033\\\\'",
    );
    let screen = image_screen(&mut host, lease);
    let key = screen.graphics.unwrap().unwrap().images[0].key;
    let Reply::GraphicsChunk(Ok(first)) = chunk(&mut host, lease, key, 0) else {
        panic!("missing chunk")
    };
    assert_eq!(first.bytes, vec![0; CHUNK_BYTES]);
    host.disconnected(lease.connection);
    let resumed = attach(&mut host, 2, false);
    assert!(matches!(
        chunk(&mut host, lease, key, CHUNK_BYTES),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(
        matches!(
            chunk(&mut host, resumed, key, CHUNK_BYTES),
            Reply::GraphicsChunk(Err(Error::Missing))
        ),
        "a resumed lease cannot continue a previous connection's pinned source"
    );
    // Restart from the authoritative image after the old connection's transfer was discarded.
    assert!(matches!(
        chunk(&mut host, resumed, key, 0),
        Reply::GraphicsChunk(Ok(_))
    ));
    let Reply::GraphicsChunk(Ok(last)) = chunk(&mut host, resumed, key, CHUNK_BYTES) else {
        panic!("missing final chunk")
    };
    assert_eq!(last.bytes, vec![0; 393216 - CHUNK_BYTES]);
    assert!(
        serde_json::to_vec(&Reply::GraphicsChunk(Ok(first)))
            .unwrap()
            .len()
            < pane::MAX_TERMINAL_FRAME
    );
    host.shutdown().unwrap();
}

#[cfg(feature = "desktop")]
#[test]
fn existing_visual_fixtures_keep_pixels_and_placements_through_the_server_adapter() {
    use crate::terminal::emulator::TerminalEmulator;
    use crate::terminal::server_graphics::Graphics;
    let _guard = crate::terminal::graphics::test_lock();
    for scenario in [
        "basics",
        "placeholders",
        "relative",
        "animation",
        "lifecycle",
        "screens",
    ] {
        let output = std::process::Command::new("python3")
            .args(["mise-tasks/smoke/kitty-graphics.py", scenario, "--dump"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let initial = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 13,
            cell_height: 27,
        };
        let mut local = TerminalEmulator::new(geometry(initial)).unwrap();
        let mut server = TerminalEmulator::new(geometry(initial)).unwrap();
        server.restrict_client_effects().unwrap();
        local.feed(&output.stdout);
        server.feed(&output.stdout);
        let mut client = Graphics::default();
        for step in 0..3 {
            let local_frame = local.snapshot().unwrap().unwrap();
            let server_frame = server.snapshot().unwrap().unwrap();
            let mut wire: Screen =
                serde_json::from_slice(&serde_json::to_vec(&encode_screen(&server_frame)).unwrap())
                    .unwrap();
            assert_eq!(
                wire.cell_pixels,
                Some((
                    if step == 2 { 17 } else { 13 },
                    if step == 2 { 31 } else { 27 }
                ))
            );
            client.update(wire.graphics.take(), wire.alternate).unwrap();
            while client
                .poll(
                    Lease {
                        server: [1; 32],
                        session: [2; 32],
                        control: [3; 32],
                        connection: [4; 32],
                    },
                    &mut |request| {
                        let Request::GraphicsChunk { image, offset, .. } = request else {
                            panic!("unexpected request")
                        };
                        let source = server_frame
                            .graphics
                            .images
                            .iter()
                            .find(|source| source.key == *image)
                            .unwrap();
                        Ok(Reply::GraphicsChunk(Ok(pane::graphics::Chunk {
                            image: *image,
                            offset: *offset,
                            bytes: source.rgba
                                [*offset..source.rgba.len().min(offset + CHUNK_BYTES)]
                                .to_vec(),
                        })))
                    },
                )
                .unwrap()
                .unwrap()
            {}
            let presented = client.snapshot();
            assert_eq!(presented.placements, server_frame.graphics.placements);
            let normalized = |placements: &[crate::terminal::ImagePlacementSnapshot]| {
                placements
                    .iter()
                    .cloned()
                    .map(|mut p| {
                        p.image.generation = 0;
                        p
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                normalized(&presented.placements),
                normalized(&local_frame.graphics.placements),
                "{scenario} step {step}"
            );
            assert_eq!(
                presented
                    .images
                    .iter()
                    .map(|image| (
                        image.key.image_id,
                        image.width,
                        image.height,
                        image.rgba.as_ref()
                    ))
                    .collect::<Vec<_>>(),
                local_frame
                    .graphics
                    .images
                    .iter()
                    .map(|image| (
                        image.key.image_id,
                        image.width,
                        image.height,
                        image.rgba.as_ref()
                    ))
                    .collect::<Vec<_>>(),
                "{scenario} step {step}"
            );
            drop(local_frame);
            drop(server_frame);
            // Force independent scroll and measured cell-pixel resize snapshots.
            if step == 0 {
                local.feed(b"\r\nscroll\r\nscroll\r\nscroll\r\nscroll\r\nscroll\r\n");
                server.feed(b"\r\nscroll\r\nscroll\r\nscroll\r\nscroll\r\nscroll\r\n");
                local.scroll_scrollback(ScrollbackMovement::Top);
                server.scroll_scrollback(ScrollbackMovement::Top);
            } else if step == 1 {
                let resized = PaneSize {
                    columns: 90,
                    rows: 30,
                    cell_width: 17,
                    cell_height: 31,
                };
                local.resize(geometry(resized)).unwrap();
                server.resize(geometry(resized)).unwrap();
            }
        }
    }
}

#[cfg(feature = "desktop")]
#[test]
fn running_multichunk_animation_completes_coherent_frames_while_the_server_advances() {
    use crate::terminal::server_graphics::Graphics;
    let _guard = crate::terminal::graphics::test_lock();
    let mut host = host();
    let lease = attach(&mut host, 1, true);
    use base64::Engine as _;
    let red = base64::engine::general_purpose::STANDARD.encode([255, 0, 0, 255].repeat(512 * 512));
    let blue = base64::engine::general_purpose::STANDARD.encode([0, 0, 255, 255].repeat(512 * 512));
    let output = format!(
        "\x1b[2J\x1b[H\x1b_Ga=T,t=d,f=32,i=10,s=512,v=512,c=8,r=8,C=1,q=2;{red}\x1b\\\x1b_Ga=f,t=d,f=32,i=10,s=512,v=512,z=40,q=2;{blue}\x1b\\\x1b_Ga=a,i=10,r=1,z=40,s=3,v=1,q=2\x1b\\"
    );
    let path = std::env::temp_dir().join(format!(
        "spaceterm-animation-{:x}",
        u128::from_ne_bytes(nonce().unwrap()[..16].try_into().unwrap())
    ));
    std::fs::write(&path, output).unwrap();
    let quoted = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    assert!(matches!(
        command(&mut host, 1, lease, &format!("stty -echo; cat {quoted}")),
        Reply::Unchanged
    ));
    let mut graphics = Graphics::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut completed = 0;
    let mut generations = std::collections::HashSet::new();
    while Instant::now() < deadline && completed < 3 {
        let reply = host.request([1; 32], lease.connection, Request::Poll { lease });
        assert!(
            !matches!(reply, Reply::Refused(_)),
            "animation runtime refused its poll: {reply:?}"
        );
        if let Reply::Screen(mut screen) = reply {
            if let Some(Ok(manifest)) = &screen.graphics {
                generations.extend(manifest.images.iter().map(|image| image.key));
            }
            graphics
                .update(screen.graphics.take(), screen.alternate)
                .unwrap();
        }
        if graphics
            .poll(lease, &mut |request| {
                Ok(host.request([1; 32], lease.connection, request.clone()))
            })
            .unwrap()
            .unwrap()
        {
            completed += 1;
            let snapshot = graphics.snapshot();
            assert_eq!(snapshot.images.len(), 1);
            let rgba = &snapshot.images[0].rgba;
            assert!(
                rgba.as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel.as_slice() == &rgba[..4]),
                "a displayed frame cannot mix chunks from different generations"
            );
            assert!(!snapshot.placements.is_empty());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    host.shutdown().unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        generations.len() >= 2,
        "the animation must keep advancing during download: {} keys, {completed} completions",
        generations.len()
    );
    assert_eq!(
        completed, 3,
        "running animation frames must eventually paint"
    );
}
