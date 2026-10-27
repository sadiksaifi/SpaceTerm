use super::*;
use crate::{
    pane::{self, PaneConnection, Reply, Request, SharedPaneConnection},
    protocol::Wire,
    transport::Transport,
};
use std::{
    io::{self, Read, Write},
    os::fd::RawFd,
    sync::{Barrier, mpsc},
    thread,
};

// Watchdogs bound broken fixtures. They are not latency acceptance thresholds.
const WATCHDOG: Duration = Duration::from_secs(10);

fn checked<T, E>(result: Result<T, E>, stage: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{stage}"),
    }
}

fn bridge_pair() -> (wss::Bridge, wss::Bridge) {
    let fixture = Fixture::new();
    let listener = checked(TcpListener::bind("127.0.0.1:0"), "bind fixture listener");
    let address = checked(listener.local_addr(), "read fixture listener address");
    checked(listener.set_nonblocking(true), "configure fixture listener");
    let (certificate, key, pin) = {
        let state = fixture.state.lock().unwrap();
        let (certificate, key) = state.certificate();
        (
            certificate.to_vec(),
            zeroize::Zeroizing::new(key.to_vec()),
            state.pin.clone(),
        )
    };
    let server = thread::spawn(move || {
        let deadline = Instant::now() + WATCHDOG;
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "fixture accept watchdog");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(_) => panic!("accept fixture socket"),
            }
        };
        checked(
            wss::Bridge::start(checked(
                wss::accept(socket, &certificate, &key),
                "accept fixture WSS",
            )),
            "start fixture server bridge",
        )
    });
    let client = checked(
        wss::Bridge::start(checked(
            wss::connect(&format!("wss://{address}/spaceterm"), &pin),
            "connect fixture WSS",
        )),
        "start fixture client bridge",
    );
    (client, checked(server.join(), "join fixture TLS handshake"))
}

// The receiver must consume the prefix before its peer writes the body. Both sides
// still use production Wire and the real bridge/TLS/TCP path; no ACK is simulated.
struct SplitFrame {
    bridge: wss::Bridge,
    prefix_read: mpsc::SyncSender<()>,
    peer_prefix_read: mpsc::Receiver<()>,
    read_remaining: usize,
    write_remaining: usize,
    waiting: bool,
}

impl SplitFrame {
    fn begin(&mut self) {
        self.read_remaining = 4;
        self.write_remaining = 4;
        self.waiting = true;
    }
}

impl Read for SplitFrame {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.bridge.read(bytes)?;
        if self.read_remaining != 0 {
            self.read_remaining = self.read_remaining.saturating_sub(count);
            if self.read_remaining == 0 {
                self.prefix_read
                    .try_send(())
                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
            }
        }
        Ok(count)
    }
}

impl Write for SplitFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.write_remaining != 0 {
            let end = bytes.len().min(self.write_remaining);
            let count = self.bridge.write(&bytes[..end])?;
            self.write_remaining -= count;
            return Ok(count);
        }
        if self.waiting {
            self.peer_prefix_read
                .recv_timeout(WATCHDOG)
                .map_err(|_| io::ErrorKind::TimedOut)?;
            self.waiting = false;
        }
        self.bridge.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.bridge.flush()
    }
}

impl Transport for SplitFrame {
    fn read_fd(&self) -> RawFd {
        self.bridge.read_fd()
    }

    fn write_fd(&self) -> RawFd {
        self.bridge.write_fd()
    }
}

fn split_frame_round_trips(samples: u8) -> Vec<Duration> {
    let (client, server) = bridge_pair();
    let (client_read, client_prefix) = mpsc::sync_channel(1);
    let (server_read, server_prefix) = mpsc::sync_channel(1);
    let peer = thread::spawn(move || {
        let mut stream = SplitFrame {
            bridge: server,
            prefix_read: server_read,
            peer_prefix_read: client_prefix,
            read_remaining: 0,
            write_remaining: 0,
            waiting: false,
        };
        for index in 0..samples {
            stream.begin();
            let mut wire = checked(Wire::terminal(&mut stream), "open split server frame");
            let received = checked(wire.read_frame(), "read split request");
            assert!(received == [index, 17, 239], "split request bytes changed");
            checked(wire.write_frame(&[239, 17, index]), "write split reply");
        }
        checked(stream.bridge.finish_reply(), "drain split final reply");
    });
    let mut stream = SplitFrame {
        bridge: client,
        prefix_read: client_read,
        peer_prefix_read: server_prefix,
        read_remaining: 0,
        write_remaining: 0,
        waiting: false,
    };
    let mut elapsed = Vec::with_capacity(usize::from(samples));
    for index in 0..samples {
        stream.begin();
        let started = Instant::now();
        let mut wire = checked(Wire::terminal(&mut stream), "open split client frame");
        checked(wire.write_frame(&[index, 17, 239]), "write split request");
        let received = checked(wire.read_frame(), "read split reply");
        elapsed.push(started.elapsed());
        assert!(received == [239, 17, index], "split reply bytes changed");
    }
    checked(peer.join(), "join split frame peer");
    elapsed
}

#[test]
fn split_prefix_and_body_preserve_small_frames_and_the_drained_final_reply() {
    split_frame_round_trips(8);
}

fn lease() -> pane::Lease {
    pane::Lease {
        server: [1; 32],
        session: [2; 32],
        control: [3; 32],
        connection: [4; 32],
    }
}

fn screen(generation: u64) -> pane::Screen {
    let cell = pane::Cell {
        hyperlink: None,
        text: "x".into(),
        foreground: pane::Color::Rgb([17, 97, 239]),
        background: pane::Color::Rgb([239, 97, 17]),
        underline_color: pane::Color::Default,
        flags: 0,
        underline: 0,
    };
    pane::Screen {
        accessibility_revision: 0,
        graphics: None,
        cell_pixels: Some((8, 16)),
        reading: None,
        hyperlinks: Vec::new(),
        generation,
        mouse_tracking: false,
        alternate_scroll: false,
        columns: 120,
        rows: vec![vec![cell; 120]; 40],
        wrapped: vec![false; 40],
        foreground: [17, 97, 239],
        background: [239, 97, 17],
        palette: vec![[0; 3]; 16],
        reversed: false,
        foreground_overridden: false,
        background_overridden: false,
        cursor_overridden: false,
        palette_overrides: vec![false; 16],
        cursor: Some((0, 0, 1)),
        cursor_visible: true,
        cursor_blinking: false,
        cursor_shape: 0,
        cursor_color: [255; 3],
        cursor_text: [0; 3],
        alternate: false,
    }
}

struct Sample {
    operation: &'static str,
    index: usize,
    queue: Duration,
    exchange: Duration,
}

impl Sample {
    fn completed(operation: &'static str, index: usize, connection: &PaneConnection) -> Self {
        let timing = connection.last_exchange_timing().expect("exchange timing");
        Self {
            operation,
            index,
            queue: timing.queue_wait,
            exchange: timing.exchange,
        }
    }

    fn emit(&self) {
        eprintln!(
            "DIAGNOSTIC wss-transport operation={} sample={} queue_us={} exchange_us={}",
            self.operation,
            self.index,
            self.queue.as_micros(),
            self.exchange.as_micros(),
        );
    }
}

// The peer models application replies only. Wire, admission and both bridges are
// production code. This does not stand in for PTY execution or native visibility.
fn mixed_workload(screens: usize, keys: usize, inventories: usize, emit: bool) {
    if emit {
        let first_screen_bytes = checked(
            serde_json::to_vec(&Reply::Screen(Box::new(screen(1)))),
            "size fixture screen reply",
        )
        .len();
        eprintln!(
            "DIAGNOSTIC wss-transport-workload screens={screens} keys={keys} inventories={inventories} first_screen_json_bytes={first_screen_bytes}"
        );
    }
    let (client, mut server) = bridge_pair();
    let peer = thread::spawn(move || {
        let mut accepted = Vec::with_capacity(keys);
        let mut screen_count = 0;
        let mut inventory_count = 0;
        let mut writes = Vec::with_capacity(screens + keys + inventories);
        let mut bulk_at_input = Vec::with_capacity(keys);
        for _ in 0..screens + keys + inventories {
            let mut wire = checked(Wire::terminal(&mut server), "open mixed server exchange");
            let (id, request): (u64, Request) = checked(wire.receive(), "read mixed request");
            let (operation, reply) = match request {
                Request::Poll { lease: target } => {
                    assert!(target == lease(), "mixed screen lease changed");
                    screen_count += 1;
                    (
                        "screen",
                        Reply::Screen(Box::new(screen(screen_count as u64))),
                    )
                }
                Request::Keys {
                    lease: target,
                    generation,
                    keys: batch,
                } => {
                    assert!(
                        target == lease() && generation == 1,
                        "mixed input authority changed"
                    );
                    assert!(batch.len() == 1, "mixed input batch changed");
                    accepted.push(batch[0].text.clone());
                    bulk_at_input.push(screen_count);
                    ("key", Reply::Unchanged)
                }
                Request::Inventory => {
                    inventory_count += 1;
                    (
                        "inventory",
                        Reply::Inventory {
                            inventory: crate::hierarchy::State::empty().inventory,
                            next_sequence: 1,
                        },
                    )
                }
                _ => panic!("unexpected mixed request"),
            };
            let started = Instant::now();
            checked(wire.send(id, &reply), "write mixed reply");
            writes.push((operation, started.elapsed()));
        }
        let expected: Vec<_> = (0..keys)
            .map(|index| Some(format!("k{index:04}")))
            .collect();
        assert!(
            accepted == expected,
            "mixed input was lost, reordered or repeated"
        );
        assert!(
            screen_count == screens && inventory_count == inventories,
            "mixed work incomplete"
        );
        checked(server.finish_reply(), "drain mixed final reply");
        (writes, bulk_at_input)
    });

    let shared = SharedPaneConnection::default();
    let mut bulk = checked(
        shared.connect(|| Ok(PaneConnection::new(client, false))),
        "open mixed shared transport",
    );
    let mut input = checked(
        shared.connect(|| panic!("mixed input opened another transport")),
        "share mixed input transport",
    );
    let mut observer = checked(
        shared.connect(|| panic!("mixed inventory opened another transport")),
        "share mixed inventory transport",
    );
    let start = Arc::new(Barrier::new(4));
    let bulk_start = Arc::clone(&start);
    let bulk = thread::spawn(move || {
        let mut samples = Vec::with_capacity(screens);
        bulk_start.wait();
        for index in 0..screens {
            let reply = checked(
                bulk.exchange(&Request::Poll { lease: lease() }),
                "mixed screen exchange",
            );
            samples.push(Sample::completed("screen", index, &bulk));
            let Reply::Screen(actual) = reply else {
                panic!("mixed screen reply missing");
            };
            let expected = screen(index as u64 + 1);
            assert!(
                actual.generation == expected.generation
                    && actual.columns == expected.columns
                    && actual.rows == expected.rows
                    && actual.wrapped == expected.wrapped,
                "mixed screen bytes or generation changed"
            );
        }
        samples
    });
    let input_start = Arc::clone(&start);
    let input = thread::spawn(move || {
        let mut samples = Vec::with_capacity(keys);
        input_start.wait();
        for index in 0..keys {
            let reply = checked(
                input.exchange(&Request::Keys {
                    lease: lease(),
                    generation: 1,
                    keys: vec![pane::Key {
                        physical: 0,
                        action: 0,
                        logical: "text-input".into(),
                        text: Some(format!("k{index:04}")),
                        unshifted: None,
                        modifiers: 0,
                        consumed: 0,
                        option_as_alt: false,
                    }],
                }),
                "mixed input exchange",
            );
            samples.push(Sample::completed("key", index, &input));
            assert!(
                matches!(reply, Reply::Unchanged),
                "mixed input was not accepted"
            );
        }
        samples
    });
    let observer_start = Arc::clone(&start);
    let observer = thread::spawn(move || {
        let mut samples = Vec::with_capacity(inventories);
        observer_start.wait();
        for index in 0..inventories {
            let reply = checked(
                observer.exchange(&Request::Inventory),
                "mixed inventory exchange",
            );
            samples.push(Sample::completed("inventory", index, &observer));
            assert!(
                matches!(reply, Reply::Inventory { .. }),
                "mixed inventory reply missing"
            );
        }
        samples
    });
    start.wait();
    let bulk = checked(bulk.join(), "join mixed screen worker");
    let input = checked(input.join(), "join mixed input worker");
    let observer = checked(observer.join(), "join mixed inventory worker");
    let (writes, bulk_at_input) = checked(peer.join(), "join mixed server worker");
    if emit {
        for sample in bulk.iter().chain(&input).chain(&observer) {
            sample.emit();
        }
        for (index, (operation, elapsed)) in writes.iter().enumerate() {
            eprintln!(
                "DIAGNOSTIC wss-transport-server operation={operation} sample={index} serialize_write_us={}",
                elapsed.as_micros()
            );
        }
        for (index, screens) in bulk_at_input.iter().enumerate() {
            eprintln!(
                "DIAGNOSTIC wss-transport-fairness sample={index} screens_before_input={screens}"
            );
        }
    }
}

#[test]
fn shared_wss_stream_preserves_ordered_inputs_while_large_replies_complete() {
    mixed_workload(4, 16, 4, false);
}

#[test]
fn a_small_request_crosses_the_shared_wss_stream_while_a_large_reply_is_outstanding() {
    const ROUNDS: usize = 8;
    let (client, mut server) = bridge_pair();
    let (outstanding, large_reply_outstanding) = mpsc::sync_channel(ROUNDS);
    let peer = thread::spawn(move || {
        for round in 0..ROUNDS {
            let mut wire = checked(Wire::terminal(&mut server), "open outstanding exchange");
            let (poll, request): (u64, Request) =
                checked(wire.receive(), "read outstanding screen request");
            assert!(
                matches!(request, Request::Poll { .. }),
                "screen request changed"
            );
            checked(outstanding.send(()), "announce outstanding screen");
            // Under one-call-at-a-time admission this read waits until the watchdog.
            let (key, request): (u64, Request) =
                checked(wire.receive(), "read input behind outstanding screen");
            assert!(
                matches!(request, Request::Keys { .. }),
                "input request changed"
            );
            checked(
                wire.send(poll, &Reply::Screen(Box::new(screen(round as u64 + 1)))),
                "write outstanding screen",
            );
            checked(wire.send(key, &Reply::Unchanged), "write input reply");
        }
        checked(server.finish_reply(), "drain outstanding final reply");
    });
    let shared = SharedPaneConnection::default();
    let mut bulk = checked(
        shared.connect(|| Ok(PaneConnection::new(client, false))),
        "open outstanding shared transport",
    );
    let mut input = checked(
        shared.connect(|| panic!("input opened another transport")),
        "share outstanding input transport",
    );
    let bulk = thread::spawn(move || {
        for round in 0..ROUNDS {
            let reply = checked(
                bulk.exchange(&Request::Poll { lease: lease() }),
                "outstanding screen exchange",
            );
            assert!(
                matches!(reply, Reply::Screen(actual) if actual.generation == round as u64 + 1),
                "outstanding screen reply changed"
            );
        }
    });
    for index in 0..ROUNDS {
        checked(
            large_reply_outstanding.recv_timeout(WATCHDOG),
            "wait for outstanding screen",
        );
        let reply = checked(
            input.exchange(&Request::Keys {
                lease: lease(),
                generation: 1,
                keys: vec![pane::Key {
                    physical: 0,
                    action: 0,
                    logical: "text-input".into(),
                    text: Some(format!("k{index:04}")),
                    unshifted: None,
                    modifiers: 0,
                    consumed: 0,
                    option_as_alt: false,
                }],
            }),
            "input behind outstanding screen",
        );
        assert!(matches!(reply, Reply::Unchanged), "input was not accepted");
    }
    checked(bulk.join(), "join outstanding screen worker");
    checked(peer.join(), "join outstanding server worker");
}

#[test]
#[ignore = "supporting latency samples require a parent-owned isolated pipeline"]
fn measure_production_wss_small_frames_and_shared_large_replies() {
    const WARMUP: usize = 16;
    const SAMPLES: usize = 64;
    let (mut client, mut server) = bridge_pair();
    let peer = thread::spawn(move || {
        for _ in 0..WARMUP + SAMPLES {
            let mut wire = checked(Wire::terminal(&mut server), "open small server frame");
            let bytes = checked(wire.read_frame(), "read small request");
            assert!(bytes == [17, 97, 239], "small request changed");
            checked(wire.write_frame(&[239, 97, 17]), "write small reply");
        }
        checked(server.finish_reply(), "drain small final reply");
    });
    let mut elapsed = Vec::with_capacity(SAMPLES);
    for index in 0..WARMUP + SAMPLES {
        let started = Instant::now();
        let mut wire = checked(Wire::terminal(&mut client), "open small client frame");
        checked(wire.write_frame(&[17, 97, 239]), "write small request");
        let bytes = checked(wire.read_frame(), "read small reply");
        let duration = started.elapsed();
        assert!(bytes == [239, 97, 17], "small reply changed");
        if index >= WARMUP {
            elapsed.push(duration);
        }
    }
    checked(peer.join(), "join small frame peer");
    drop(client);
    for (index, duration) in elapsed.iter().enumerate() {
        eprintln!(
            "DIAGNOSTIC wss-transport-small sample={index} round_trip_us={}",
            duration.as_micros()
        );
    }
    for (index, duration) in split_frame_round_trips(SAMPLES as u8).iter().enumerate() {
        eprintln!(
            "DIAGNOSTIC wss-transport-split sample={index} round_trip_us={}",
            duration.as_micros()
        );
    }
    // Freeze latency bounds only after the unchanged baseline reproduces the cause.
    mixed_workload(16, 64, 16, true);
}

#[test]
#[ignore = "requires an isolated pipeline on the native host and --release"]
fn split_wss_round_trips_meet_isolated_release_latency_budget() {
    const WARMUP: u8 = 16;
    const SAMPLES: u8 = 128;
    const BUDGET: Duration = Duration::from_millis(40);

    let mut elapsed = split_frame_round_trips(WARMUP + SAMPLES);
    let mut samples = elapsed.split_off(usize::from(WARMUP));
    for (index, duration) in samples.iter().enumerate() {
        eprintln!(
            "DIAGNOSTIC wss-split-budget sample={index} round_trip_us={}",
            duration.as_micros()
        );
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
    eprintln!(
        "DIAGNOSTIC wss-split-budget samples={} p95_us={} budget_us={}",
        samples.len(),
        p95.as_micros(),
        BUDGET.as_micros()
    );
    assert!(
        p95 < BUDGET,
        "isolated Release split-frame p95 must stay below the budget: samples={} p95_us={} budget_us={}",
        samples.len(),
        p95.as_micros(),
        BUDGET.as_micros()
    );
}

#[test]
#[ignore = "requires an isolated pipeline on the native host and --release"]
fn large_wss_replies_cross_the_bridge_within_isolated_release_budget() {
    const WARMUP: usize = 4;
    const SAMPLES: usize = 32;
    const REPLY_BYTES: usize = 1024 * 1024;
    // A 1 MiB reply is about one 160x48 full screen. The former bridge moved at most 8 KiB per
    // 2 ms idle sleep on macOS, about 3.6 MiB/s or 280 ms per reply. Two loopback TLS bridges
    // move it in a few milliseconds; the budget stays below one 30 ms visible poll interval.
    const BUDGET: Duration = Duration::from_millis(25);

    let (mut client, mut server) = bridge_pair();
    let reply: Vec<u8> = (0..REPLY_BYTES).map(|i| (i % 251) as u8).collect();
    let expected = reply.clone();
    let peer = thread::spawn(move || {
        for _ in 0..WARMUP + SAMPLES {
            let mut wire = checked(Wire::terminal(&mut server), "open large server frame");
            let request = checked(wire.read_frame(), "read large request");
            assert!(request == [17, 97, 239], "large request changed");
            checked(wire.write_frame(&reply), "write large reply");
        }
        checked(server.finish_reply(), "drain large final reply");
    });
    let mut samples = Vec::with_capacity(SAMPLES);
    for index in 0..WARMUP + SAMPLES {
        let started = Instant::now();
        let mut wire = checked(Wire::terminal(&mut client), "open large client frame");
        checked(wire.write_frame(&[17, 97, 239]), "write large request");
        let bytes = checked(wire.read_frame(), "read large reply");
        let duration = started.elapsed();
        assert!(bytes == expected, "large reply bytes changed");
        if index >= WARMUP {
            samples.push(duration);
        }
    }
    checked(peer.join(), "join large reply peer");
    for (index, duration) in samples.iter().enumerate() {
        eprintln!(
            "DIAGNOSTIC wss-large-budget sample={index} round_trip_us={}",
            duration.as_micros()
        );
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
    eprintln!(
        "DIAGNOSTIC wss-large-budget samples={} reply_bytes={REPLY_BYTES} p95_us={} budget_us={}",
        samples.len(),
        p95.as_micros(),
        BUDGET.as_micros()
    );
    assert!(
        p95 < BUDGET,
        "isolated Release 1 MiB reply p95 must stay below the budget: samples={} p95_us={} budget_us={}",
        samples.len(),
        p95.as_micros(),
        BUDGET.as_micros()
    );
}
