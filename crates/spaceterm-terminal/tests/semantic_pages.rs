#![cfg(all(feature = "engine", feature = "test-support"))]

use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_terminal::{TerminalEmulator, geometry::*};

fn session() -> SessionKey {
    SessionKey {
        pane: PaneId::new(ServerId::new([7; 16]), 1).unwrap(),
        epoch: ServerEpoch::new(1),
        generation: SessionGeneration::new(1),
    }
}
fn emulator(columns: u16, rows: u16) -> TerminalEmulator {
    let mut emulator = TerminalEmulator::new(TerminalGeometry::from_grid(
        CellGridSize::new(columns, rows),
        LogicalCellSize::new(10.0, 20.0),
        BackingScale::ONE,
    ))
    .unwrap();
    emulator.set_content_session(session()).unwrap();
    emulator
}
fn stamp(emulator: &TerminalEmulator) -> QueryStamp {
    let base = emulator.content_base().unwrap();
    let fence = EffectFence::new(
        base.session,
        ControlGrant::new(
            base.session.pane,
            ClientId::new(ServerId::new([7; 16]), 1).unwrap(),
            ControlGeneration::new(1),
        )
        .unwrap(),
        ConnectionBinding::new(
            ClientId::new(ServerId::new([7; 16]), 1).unwrap(),
            ConnectionGeneration::new(1),
        ),
        ContextGeneration::new(1),
        Some(base.revision),
    )
    .unwrap();
    QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(1).unwrap(),
        RequestGeneration::new(1),
    )
    .unwrap()
}
fn page(emulator: &mut TerminalEmulator, range: ContentRange) -> ContentPageReply {
    let request = ContentPageRequest::new(
        stamp(emulator),
        range,
        None,
        PageLimits::new(256, 16_384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(reply) = emulator.content_page(&request).unwrap() else {
        panic!("expected semantic page")
    };
    reply
}

#[test]
fn active_screen_and_history_share_semantic_style_mapping() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"\x1b[31;44;1mhello\x1b[0m\r\nnext\r\nlast");
    let reply = page(
        &mut emulator,
        ContentRange::History {
            first: None,
            last: None,
        },
    );
    let row = &reply.page().parts().rows[0];
    assert_eq!(row.cells()[0].content().text(), "h");
    assert_eq!(row.cells()[0].style().foreground, TerminalColor::Palette(1));
    assert_eq!(row.cells()[0].style().background, TerminalColor::Palette(4));
    assert!(row.cells()[0].style().bold);
    assert_eq!(row.cells().len(), 8);
    assert_eq!(
        page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .rows
            .len(),
        2
    );
}

#[test]
fn style_only_and_background_only_changes_advance_row_revision() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"same");
    let before = page(&mut emulator, ContentRange::ActiveGrid);
    emulator.feed(b"\r\x1b[31msame\x1b[44m\x1b[K");
    let after = page(&mut emulator, ContentRange::ActiveGrid);
    assert_eq!(
        before.page().parts().rows[0].id(),
        after.page().parts().rows[0].id()
    );
    assert_ne!(
        before.page().parts().rows[0].revision(),
        after.page().parts().rows[0].revision()
    );
    assert_eq!(
        after.page().parts().rows[0].cells()[7].style().background,
        TerminalColor::Palette(4)
    );
}

#[test]
fn continuation_after_feed_requires_resync() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"abcdefghijk");
    let request = ContentPageRequest::new(
        stamp(&emulator),
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(1, 4, 8192).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(first) = emulator.content_page(&request).unwrap() else {
        panic!("expected fragment")
    };
    assert_eq!(first.page().parts().rows[0].cells().len(), 4);
    let continuation = first.continuation().unwrap().unwrap();
    let next = ContentPageRequest::new(
        request.stamp(),
        request.range(),
        Some(continuation),
        request.limits(),
    )
    .unwrap();
    emulator.feed(b"x");
    assert!(
        matches!(emulator.content_page(&next).unwrap(), ContentReadResult::ResyncRequired { request: original, .. } if original == request.stamp())
    );
}

#[test]
fn synchronized_output_is_not_published_partially() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"\x1b[?2026hpartial");
    let request = ContentPageRequest::new(
        stamp(&emulator),
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        emulator.content_page(&request).unwrap(),
        ContentReadResult::Rejected {
            reason: ContentReadFailure::Cancelled,
            ..
        }
    ));
    emulator.feed(b"\x1b[?2026l");
    assert_eq!(
        page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .rows[0]
            .cells()[0]
            .content()
            .text(),
        "p"
    );
}

#[test]
fn only_matching_prompt_owner_emits_restoration_confirmation_with_original_tick() {
    use spaceterm_core::observations::MonotonicTick;
    use spaceterm_terminal::{local_path::LocalPathSemantics, metadata::*};
    let context = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/initial",
        LocalMachine::new(None, Some("localhost"), None),
    )
    .with_prompt_owner(PromptOwner::fixture("owning-shell"));
    let mut emulator = TerminalEmulator::new_with_metadata_context(
        TerminalGeometry::from_grid(
            CellGridSize::new(20, 3),
            LogicalCellSize::new(10.0, 20.0),
            BackingScale::ONE,
        ),
        context,
        "",
        "xterm-256color",
        std::time::Instant::now(),
    )
    .unwrap();
    emulator.set_content_session(session()).unwrap();
    emulator
        .feed_observed(
            b"\x1b]7;file://localhost/fallback\x07\x1b]133;A\x07",
            MonotonicTick::new(10),
        )
        .unwrap();
    assert!(emulator.take_confirmed_directory().is_none());
    emulator
        .feed_observed(
            b"\x1b]7;file://localhost/first\x07\x1b]133;A;spaceterm=owning-shell\x07",
            MonotonicTick::new(20),
        )
        .unwrap();
    let first = emulator.take_confirmed_directory().unwrap();
    assert_eq!(first.directory().bytes(), b"/first");
    assert_eq!(first.observed_at(), MonotonicTick::new(20));
    emulator
        .feed_observed(
            b"\x1b]7;file://localhost/nested\x07\x1b]133;A;spaceterm=nested\x07",
            MonotonicTick::new(30),
        )
        .unwrap();
    assert!(emulator.take_confirmed_directory().is_none());
    emulator
        .feed_observed(
            b"\x1b]7;file://localhost/second\x07\x1b]133;P;spaceterm=owning-shell\x07",
            MonotonicTick::new(40),
        )
        .unwrap();
    emulator
        .feed_observed(b"unrelated output", MonotonicTick::new(50))
        .unwrap();
    let second = emulator.take_confirmed_directory().unwrap();
    assert_eq!(second.session(), session());
    assert!(second.sequence() > first.sequence());
    assert_eq!(second.observed_at(), MonotonicTick::new(40));
    assert!(!format!("{second:?}").contains("owning-shell"));
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn graphics_chunks_are_session_qualified_bounded_and_revocable() {
    use sha2::{Digest, Sha256};
    use spaceterm_terminal::content::{graphics::GraphicsStep, pages::ContentScanBudget};
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(80, 24);
    let rgba_bytes = 384 * 192 * 4;
    let output = format!(
        "\x1b_Ga=T,f=32,s=384,v=192,i=7,q=2;{}\x1b\\",
        "/".repeat(rgba_bytes / 3 * 4)
    );
    emulator.feed(output.as_bytes());
    let request = ContentPageRequest::new(
        stamp(&emulator),
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        emulator.content_page(&request).unwrap(),
        ContentReadResult::Rejected {
            reason: ContentReadFailure::Capacity,
            ..
        }
    ));
    assert!(emulator.prepare_graphics_quantum().unwrap());
    assert!(emulator.prepare_graphics_quantum().unwrap());
    let ContentReadResult::Ready(page) = emulator.content_page(&request).unwrap() else {
        panic!("expected prepared graphics manifest")
    };
    assert_eq!(page.page().parts().resources.len(), 1);
    let resource = page.page().parts().resources[0];
    assert_eq!(resource.key.session, session());
    assert_eq!(
        resource.digest,
        <[u8; 32]>::from(Sha256::digest(vec![255; rgba_bytes]))
    );
    assert_eq!(
        page.page().parts().placements[0].anchor.screen_generation,
        request.stamp().base().screen_generation
    );
    let graphics = GraphicsRequest::new(
        request.stamp(),
        ResourceTransferId::new(7).unwrap(),
        resource,
        0,
    )
    .unwrap();
    let mut job = emulator.begin_graphics(&graphics).unwrap();
    let mut count = 0;
    loop {
        match emulator
            .step_graphics(&mut job, ContentScanBudget::default())
            .unwrap()
        {
            GraphicsStep::Complete => break,
            GraphicsStep::Item(chunk) => {
                assert!(chunk.bytes().len() <= 256 * 1024);
                assert!(chunk.bytes().iter().all(|byte| *byte == 255));
                count += chunk.bytes().len();
            }
        }
    }
    assert_eq!(count, rgba_bytes);
    emulator.feed(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    let missing = GraphicsRequest::new(
        stamp(&emulator),
        ResourceTransferId::new(8).unwrap(),
        resource,
        0,
    )
    .unwrap();
    assert!(matches!(
        emulator.begin_graphics(&missing),
        Err(ContentReadFailure::MissingResource)
    ));
}

#[test]
fn row_and_byte_limits_page_without_losing_wide_or_combining_cells() {
    let mut emulator = emulator(8, 3);
    emulator.feed("ab😀e\u{301}xy\r\nnext".as_bytes());
    let whole = page(&mut emulator, ContentRange::ActiveGrid);
    let stamp = stamp(&emulator);
    let mut continuation = None;
    let mut collected = Vec::new();
    loop {
        let request = ContentPageRequest::new(
            stamp,
            ContentRange::ActiveGrid,
            continuation,
            PageLimits::new(1, 3, 8192).unwrap(),
        )
        .unwrap();
        let ContentReadResult::Ready(reply) = emulator.content_page(&request).unwrap() else {
            panic!("expected bounded fragment")
        };
        assert_eq!(reply.page().parts().rows.len(), 1);
        assert!(reply.page().parts().rows[0].cells().len() <= 3);
        assert!(reply.page().accounted_bytes() <= 8192);
        collected.extend(reply.page().parts().rows[0].cells().iter().cloned());
        continuation = reply.continuation().unwrap();
        if continuation.is_none() {
            break;
        }
    }
    let expected: Vec<_> = whole
        .page()
        .parts()
        .rows
        .iter()
        .flat_map(|row| row.cells().iter().cloned())
        .collect();
    assert_eq!(collected, expected);
    assert_eq!(expected[2].content().text(), "😀");
    assert_eq!(expected[4].content().text(), "e\u{301}");
}

#[test]
fn long_complete_grapheme_and_hyperlink_are_never_shortened() {
    let mut emulator = emulator(4, 2);
    let cluster = format!("a{}", "\u{301}".repeat(60));
    let target = format!("https://example.invalid/{}", "x".repeat(2000));
    emulator.feed(format!("\x1b]8;;{target}\x1b\\{cluster}\x1b]8;;\x1b\\").as_bytes());
    let reply = page(&mut emulator, ContentRange::ActiveGrid);
    assert_eq!(
        reply.page().parts().rows[0].cells()[0].content().text(),
        cluster
    );
    assert_eq!(reply.page().parts().hyperlinks[0].target(), target);
    let request = ContentPageRequest::new(
        stamp(&emulator),
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(1, 4, 8192).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        emulator.content_page(&request).unwrap(),
        ContentReadResult::Rejected {
            reason: ContentReadFailure::Capacity,
            ..
        }
    ));
}

#[test]
fn accessibility_uses_exact_screen_generation_cursor_and_pty_hidden_input() {
    let mut emulator = emulator(8, 2);
    emulator.feed(b"\x1b[5 qprompt");
    let request = ContentPageRequest::new(
        stamp(&emulator),
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16_384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(reply) = emulator.accessibility_page(&request).unwrap() else {
        panic!("expected accessibility facts")
    };
    assert_eq!(reply.page().parts().cursor.unwrap().shape, CursorShape::Bar);
    assert!(!reply.page().parts().modes.hidden_input);
    assert!(emulator.set_content_hidden_input(true).unwrap());
    assert!(matches!(
        emulator.accessibility_page(&request).unwrap(),
        ContentReadResult::ResyncRequired { .. }
    ));
    assert!(
        page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .modes
            .hidden_input
    );
    assert!(!emulator.set_content_hidden_input(true).unwrap());
    let primary = emulator.content_base().unwrap();
    emulator.feed(b"\x1b[?1049halternate");
    let alternate = page(&mut emulator, ContentRange::ActiveGrid);
    assert_eq!(alternate.page().parts().base.screen, Screen::Alternate);
    assert_ne!(alternate.page().parts().base, primary);
    assert!(
        alternate
            .page()
            .parts()
            .rows
            .iter()
            .all(|row| row.id().screen == Screen::Alternate)
    );
}

#[test]
fn compression_and_render_reads_preserve_semantic_row_revisions() {
    use libghostty_vt::terminal::CompressionResult;
    let mut emulator = emulator(80, 4);
    for index in 0..800 {
        emulator.feed(format!("\x1b[32m{index:04} retained styled history\x1b[0m\r\n").as_bytes());
    }
    let before = page(
        &mut emulator,
        ContentRange::History {
            first: None,
            last: None,
        },
    );
    let base = emulator.content_base().unwrap();
    let _ = emulator.snapshot().unwrap();
    for _ in 0..128 {
        if emulator.compress_scrollback().unwrap() != CompressionResult::Pending {
            break;
        }
    }
    let activity = emulator.compression_activity().unwrap();
    let after = page(
        &mut emulator,
        ContentRange::History {
            first: None,
            last: None,
        },
    );
    assert_eq!(emulator.content_base().unwrap(), base);
    assert_eq!(emulator.compression_activity().unwrap(), activity);
    assert_eq!(after.page().parts().rows, before.page().parts().rows);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn animation_without_output_invalidates_semantic_resource_requests() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 4);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\\x1b_Ga=f,t=d,f=32,i=1,s=1,v=1,z=40;BQYHCA==\x1b\\\x1b_Ga=a,i=1,r=1,z=40,s=3\x1b\\");
    emulator
        .advance_content_timers(std::time::Instant::now())
        .unwrap();
    let before = emulator.content_base().unwrap();
    let deadline = emulator.graphics_animation_deadline().unwrap();
    std::thread::sleep(
        deadline.saturating_duration_since(std::time::Instant::now())
            + std::time::Duration::from_millis(1),
    );
    emulator
        .advance_content_timers(std::time::Instant::now())
        .unwrap();
    assert!(emulator.content_base().unwrap().revision > before.revision);
}

#[cfg(feature = "kitty-graphics")]
fn graphics_page(emulator: &mut TerminalEmulator, range: ContentRange) -> ContentPageReply {
    let request = ContentPageRequest::new(
        stamp(emulator),
        range,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    for _ in 0..2048 {
        if let ContentReadResult::Ready(reply) = emulator.content_page(&request).unwrap() {
            return reply;
        }
        if !emulator.prepare_graphics_quantum().unwrap() {
            break;
        }
    }
    panic!("graphics page never became ready")
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn virtual_graphics_are_row_scoped_and_preserve_placeholder_fragments() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 3);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,p=1,s=1,v=1,U=1,c=1,r=1;AQIDBA==\x1b\\");
    // A virtual definition without a placeholder cannot poison text pages.
    assert!(
        graphics_page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .placements
            .is_empty()
    );
    emulator.feed("\x1b[38;2;0;0;1m\u{10eeee}\u{305}\u{305}\x1b[0m\r\nnext\r\nlast\r\n".as_bytes());
    assert!(
        graphics_page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .placements
            .is_empty()
    );
    let history = graphics_page(
        &mut emulator,
        ContentRange::History {
            first: None,
            last: None,
        },
    );
    assert_eq!(history.page().parts().placements.len(), 1);
    let placement = history.page().parts().placements[0];
    assert!(placement.unicode_placeholder);
    assert_eq!(
        placement.source,
        ImageRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1
        }
    );
    assert_eq!(
        (placement.destination_width, placement.destination_height),
        (10, 10)
    );
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn relative_graphics_keep_signed_columns_and_effective_cell_offsets() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 3);
    emulator.feed(b"\x1b[2;2H\x1b_Ga=T,t=d,f=32,i=1,p=1,s=1,v=1,c=2,r=1,X=9,Y=19;AQIDBA==\x1b\\\x1b_Ga=T,t=d,f=32,i=2,p=1,s=1,v=1,c=2,r=1,P=1,Q=1,H=-2;AQIDBA==\x1b\\");
    let before = graphics_page(&mut emulator, ContentRange::ActiveGrid);
    assert_eq!(before.page().parts().placements.len(), 2);
    let relative = before
        .page()
        .parts()
        .placements
        .iter()
        .find(|p| p.resource.id.get() == 3)
        .unwrap();
    assert_eq!(relative.column, -1);
    let parent = before
        .page()
        .parts()
        .placements
        .iter()
        .find(|p| p.resource.id.get() == 2)
        .unwrap();
    assert_eq!((parent.pixel_offset_x, parent.pixel_offset_y), (9, 19));
    emulator
        .resize(TerminalGeometry::from_grid(
            CellGridSize::new(8, 3),
            LogicalCellSize::new(5.0, 10.0),
            BackingScale::ONE,
        ))
        .unwrap();
    let after = graphics_page(&mut emulator, ContentRange::ActiveGrid);
    let parent = after
        .page()
        .parts()
        .placements
        .iter()
        .find(|p| p.resource.id.get() == 2)
        .unwrap();
    assert_eq!((parent.pixel_offset_x, parent.pixel_offset_y), (4, 9));
    assert_eq!(
        (parent.destination_width, parent.destination_height),
        (6, 1)
    );
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn implicit_and_explicit_graphics_placement_ids_never_alias() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 3);
    emulator.feed(
        b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\\x1b_Ga=p,i=1\x1b\\\x1b_Ga=p,i=1,p=1\x1b\\",
    );
    let reply = graphics_page(&mut emulator, ContentRange::ActiveGrid);
    let ids: std::collections::BTreeSet<_> = reply
        .page()
        .parts()
        .placements
        .iter()
        .map(|p| p.placement_id)
        .collect();
    assert_eq!(ids.len(), 3);
    emulator.feed(b"\x1b_Ga=p,i=1,p=1,c=2,r=1\x1b\\");
    assert_eq!(
        graphics_page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .placements
            .len(),
        3
    );
    emulator.feed(b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");
    assert_eq!(
        graphics_page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .placements
            .len(),
        2
    );
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn historical_graphics_do_not_consume_unrelated_page_limits() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 3);
    emulator.feed(b"\x1b_Ga=T,t=d,f=32,i=1,s=1,v=1;AQIDBA==\x1b\\\r\n");
    for _ in 0..1025 {
        emulator.feed(b"\x1b_Ga=p,i=1\x1b\\\r\n");
    }
    emulator.feed(b"\r\n\r\n\r\n");
    assert!(
        graphics_page(&mut emulator, ContentRange::ActiveGrid)
            .page()
            .parts()
            .placements
            .is_empty()
    );
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn live_graphics_manifest_cache_is_not_a_document_wide_page_limit() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(8, 3);
    for id in 1..=129 {
        emulator.feed(format!("\x1b_Ga=T,t=d,f=32,i={id},s=1,v=1;AQIDBA==\x1b\\").as_bytes());
        let reply = graphics_page(&mut emulator, ContentRange::ActiveGrid);
        assert!(
            reply
                .page()
                .parts()
                .resources
                .iter()
                .any(|resource| resource.key.id.get() == id + 1)
        );
        emulator.feed(b"\r\n\r\n\r\n");
    }
}

#[cfg(feature = "runtime")]
#[path = "support/mod.rs"]
mod support;

#[cfg(feature = "runtime")]
fn runtime_stamp(
    handle: &spaceterm_terminal::runtime::owner::RuntimeHandle,
    authority: spaceterm_core::runtime_authority::RuntimeAuthority,
    id: u64,
) -> QueryStamp {
    let probe = support::stamp(authority, 1, id, 1);
    let result = handle
        .read_topology(support::topology(probe))
        .unwrap()
        .receive_blocking()
        .unwrap();
    let current = match result {
        ContentReadResult::ResyncRequired { current, .. } => current,
        ContentReadResult::Ready(value) => value.stamp().base(),
        _ => panic!("initial topology"),
    };
    QueryStamp::new(
        EffectFence::new(
            current.session,
            probe.fence().grant(),
            probe.fence().connection(),
            probe.fence().context(),
            Some(current.revision),
        )
        .unwrap(),
        current,
        ContentRequestId::new(id + 1).unwrap(),
        RequestGeneration::new(1),
    )
    .unwrap()
}

#[cfg(feature = "runtime")]
fn real_runtime(
    factory: &spaceterm_terminal::runtime::owner::RuntimeFactory,
    request: spaceterm_terminal::runtime_contracts::RuntimeStartRequest,
    pty: &support::Pty,
) -> spaceterm_terminal::runtime::owner::RuntimeOwner {
    use spaceterm_terminal::{
        content::runtime::SemanticRuntimeEngine,
        local_filesystem::LocalFilesystemAuthority,
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, PromptOwner, TerminalMetadataContext},
    };
    let metadata = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/owning-server",
        LocalMachine::new(None, Some("localhost"), None),
    )
    .with_prompt_owner(PromptOwner::fixture("owning-shell"));
    let owner = factory
        .start(
            request,
            pty.launch(),
            SemanticRuntimeEngine::launch(metadata, LocalFilesystemAuthority::testing()),
        )
        .unwrap();
    owner.wait_until_running().unwrap();
    owner
}

#[cfg(feature = "runtime")]
fn runtime_output(pty: &support::Pty, bytes: &[u8]) {
    let written = pty.inspect(|state| state.written.len());
    let mut output = bytes.to_vec();
    output.extend_from_slice(b"\x1b[5n");
    pty.output(&output);
    pty.until(|state| state.written.len() == written + 4 || state.terminated != 0);
    assert_eq!(pty.inspect(|state| state.terminated), 0);
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_safe_paste_reaches_pty_at_the_payload_bound_without_content_changes() {
    use spaceterm_core::{native_requests::*, runtime_authority::EffectDisposition};
    use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::RuntimeCommand};
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    let request = request(1, support::grid(3, 16));
    let authority = request.authority();
    let mut owner = real_runtime(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    runtime_output(&pty, b"retained");
    let handle = owner.handle();
    let before = runtime_stamp(&handle, authority, 1);
    for text in ["safe\x03".to_owned(), "x".repeat(MAX_NATIVE_TEXT_BYTES)] {
        let written = pty.inspect(|state| state.written.len());
        let expected = text.replace('\x03', "");
        assert_eq!(
            handle
                .try_submit(
                    fence(authority),
                    RuntimeCommand::RequestPaste(NativeText::new(text).unwrap())
                )
                .unwrap()
                .receive_blocking()
                .unwrap(),
            EffectDisposition::Applied
        );
        assert_eq!(
            pty.inspect(|state| state.written[written..].to_vec()),
            expected.as_bytes()
        );
    }
    assert!(native.try_next().unwrap().is_none());
    assert_eq!(runtime_stamp(&handle, authority, 3).base(), before.base());
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_bracketed_multiline_paste_uses_current_mode_without_approval() {
    use spaceterm_core::{native_requests::*, runtime_authority::EffectDisposition};
    use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::RuntimeCommand};
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    let request = request(1, support::grid(3, 16));
    let authority = request.authority();
    let mut owner = real_runtime(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    runtime_output(&pty, b"\x1b[?2004hretained");
    let handle = owner.handle();
    let before = runtime_stamp(&handle, authority, 1);
    let written = pty.inspect(|state| state.written.len());
    assert_eq!(
        handle
            .try_submit(
                fence(authority),
                RuntimeCommand::RequestPaste(NativeText::new("one\r\ntwo".into()).unwrap())
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    assert_eq!(
        pty.inspect(|state| state.written[written..].to_vec()),
        b"\x1b[200~one\ntwo\x1b[201~"
    );
    assert!(native.try_next().unwrap().is_none());
    assert_eq!(runtime_stamp(&handle, authority, 3).base(), before.base());
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_confirmed_unsafe_paste_encodes_the_retained_sanitized_payload() {
    use spaceterm_core::{native_requests::*, runtime_authority::EffectDisposition};
    use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::RuntimeCommand};
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    let request = request(1, support::grid(3, 16));
    let authority = request.authority();
    let mut owner = real_runtime(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    runtime_output(&pty, b"retained");
    let handle = owner.handle();
    let written = pty.inspect(|state| state.written.len());
    for native_completion in [false, true] {
        assert_eq!(
            handle
                .try_submit(
                    fence(authority),
                    RuntimeCommand::RequestPaste(
                        NativeText::new("one\r\ntwo\x1b[201~".into()).unwrap()
                    )
                )
                .unwrap()
                .receive_blocking()
                .unwrap(),
            EffectDisposition::Applied
        );
        assert_eq!(
            pty.inspect(|state| state.written.len()),
            written + usize::from(native_completion) * 12
        );
        let request = native.next_blocking().unwrap().unwrap();
        let NativeRequestKind::UnsafePaste { approval, text } = request.kind() else {
            panic!("unsafe paste request");
        };
        assert_eq!(text.as_str(), "one\ntwo[201~");
        let command = if native_completion {
            RuntimeCommand::CompleteNativeRequest(
                NativeCompletion::new(
                    &request,
                    NativeCompletionKind::Paste {
                        approval: *approval,
                        decision: PasteDecision::Confirm,
                    },
                )
                .unwrap(),
            )
        } else {
            RuntimeCommand::ResolvePaste(*approval, PasteDecision::Confirm)
        };
        assert_eq!(
            handle
                .try_submit(request.fence(), command)
                .unwrap()
                .receive_blocking()
                .unwrap(),
            EffectDisposition::Applied
        );
    }
    assert_eq!(
        pty.inspect(|state| state.written[written..].to_vec()),
        b"one\rtwo[201~one\rtwo[201~"
    );
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_takeover_discards_confirmed_paste_before_the_final_pty_write() {
    use spaceterm_core::{native_requests::*, runtime_authority::EffectDisposition};
    use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::RuntimeCommand};
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    let request = request(1, support::grid(3, 16));
    let authority = request.authority();
    let mut owner = real_runtime(&factory, request, &pty);
    let mut native = owner.take_native_requests().unwrap();
    runtime_output(&pty, b"");
    let handle = owner.handle();
    assert_eq!(
        handle
            .try_submit(
                fence(authority),
                RuntimeCommand::RequestPaste(NativeText::new("old\nsuffix".into()).unwrap())
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    let request = native.next_blocking().unwrap().unwrap();
    let NativeRequestKind::UnsafePaste { approval, .. } = request.kind() else {
        panic!("unsafe paste request");
    };
    pty.update(|state| state.blocked = true);
    let paste = handle
        .try_submit(
            request.fence(),
            RuntimeCommand::ResolvePaste(*approval, PasteDecision::Confirm),
        )
        .unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !pty.inspect(|state| state.attempted_write == b"old\rsuffix") {
        assert!(
            paste.try_receive().unwrap().is_none(),
            "approved paste failed before reaching the PTY"
        );
        assert!(std::time::Instant::now() < end);
        std::thread::yield_now();
    }
    let transition = takeover(authority);
    owner
        .transition_authority(transition)
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        paste.receive_blocking().unwrap(),
        EffectDisposition::StaleAuthority
    );
    pty.update(|state| state.blocked = false);
    assert_eq!(
        handle
            .try_submit(fence(transition.next()), ime("new"))
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    assert_eq!(pty.inspect(|state| state.written.clone()), b"\x1b[0nnew");
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_title_reports_do_not_refresh_directory_or_close_work() {
    use spaceterm_core::observations::*;
    use spaceterm_terminal::runtime::{budget::RuntimeLimits, owner::RuntimeFactory};
    use std::sync::{Arc, atomic::Ordering};
    use support::*;
    let clock = Arc::new(Clock::default());
    clock.0.store(20, Ordering::Release);
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let pty = Pty::default();
    let mut owner = real_runtime(&factory, request(1, support::grid(3, 16)), &pty);
    runtime_output(&pty, b"");
    runtime_output(
        &pty,
        b"\x1b]7;file://localhost/first\x07\x1b]133;A;spaceterm=owning-shell\x07",
    );
    let directory = owner.display_directory_snapshot().unwrap();
    let work = owner.close_work_snapshot().unwrap();
    assert_eq!(work.state(), CloseWorkState::Idle);
    assert_eq!(work.observed_at(), MonotonicTick::new(20));
    clock.0.store(5000, Ordering::Release);
    runtime_output(&pty, b"\x1b]0;new title\x07");
    assert_eq!(
        &*owner.metadata_snapshot().unwrap().parts().title.value,
        "new title"
    );
    assert_eq!(owner.display_directory_snapshot(), Some(directory));
    assert_eq!(owner.close_work_snapshot(), Some(work));
    assert!(
        !owner
            .close_work_snapshot()
            .unwrap()
            .is_fresh(MonotonicTick::new(5000), 1000)
    );
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_progress_expiry_does_not_refresh_directory_or_close_work() {
    use spaceterm_core::observations::*;
    use spaceterm_terminal::{
        metadata::ProgressMetadata,
        runtime::{budget::RuntimeLimits, owner::RuntimeFactory},
    };
    use std::sync::{Arc, atomic::Ordering};
    use support::*;
    let clock = Arc::new(Clock::default());
    clock.0.store(20, Ordering::Release);
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let pty = Pty::default();
    let mut owner = real_runtime(&factory, request(1, support::grid(3, 16)), &pty);
    runtime_output(&pty, b"");
    runtime_output(
        &pty,
        b"\x1b]7;file://localhost/first\x07\x1b]133;A;spaceterm=owning-shell\x07\x1b]9;4;1;25\x07",
    );
    let metadata = owner.metadata_snapshot().unwrap();
    assert_eq!(metadata.parts().progress, ProgressMetadata::Normal(25));
    let directory = owner.display_directory_snapshot().unwrap();
    let work = owner.close_work_snapshot().unwrap();
    assert_eq!(work.state(), CloseWorkState::Idle);
    clock.0.store(50_000, Ordering::Release);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while owner.metadata_snapshot().unwrap().parts().progress != ProgressMetadata::None {
        assert!(std::time::Instant::now() < end);
        std::thread::yield_now();
    }
    assert!(owner.metadata_snapshot().unwrap().parts().sequence > metadata.parts().sequence);
    assert_eq!(owner.display_directory_snapshot(), Some(directory.clone()));
    assert_eq!(owner.close_work_snapshot(), Some(work));
    assert!(
        !owner
            .close_work_snapshot()
            .unwrap()
            .is_fresh(MonotonicTick::new(50_000), 1000)
    );
    // A new native PTY hidden-input fact has its own source tick, not the old prompt's.
    clock.0.store(50_020, Ordering::Release);
    pty.update(|state| state.hidden = true);
    while owner.close_work_snapshot().unwrap().state() != CloseWorkState::RunningWork {
        assert!(std::time::Instant::now() < end);
        std::thread::yield_now();
    }
    let running = owner.close_work_snapshot().unwrap();
    assert_eq!(running.observed_at(), MonotonicTick::new(50_020));
    assert!(running.generation() > work.generation());
    assert_eq!(owner.display_directory_snapshot(), Some(directory));
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_repeated_directory_and_prompt_reports_refresh_only_their_own_lanes() {
    use spaceterm_core::observations::*;
    use spaceterm_terminal::runtime::{budget::RuntimeLimits, owner::RuntimeFactory};
    use std::sync::{Arc, atomic::Ordering};
    use support::*;
    let clock = Arc::new(Clock::default());
    clock.0.store(20, Ordering::Release);
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let pty = Pty::default();
    let mut owner = real_runtime(&factory, request(1, support::grid(3, 16)), &pty);
    runtime_output(&pty, b"");
    runtime_output(
        &pty,
        b"\x1b]7;file://localhost/first\x07\x1b]133;A;spaceterm=owning-shell\x07",
    );
    let first_directory = owner.display_directory_snapshot().unwrap();
    let first_work = owner.close_work_snapshot().unwrap();
    clock.0.store(40, Ordering::Release);
    runtime_output(&pty, b"\x1b]7;file://localhost/first\x07");
    let directory = owner.display_directory_snapshot().unwrap();
    assert_eq!(directory.observed_at(), MonotonicTick::new(40));
    assert!(directory.sequence() > first_directory.sequence());
    assert_eq!(owner.close_work_snapshot(), Some(first_work));
    clock.0.store(60, Ordering::Release);
    runtime_output(&pty, b"\x1b]133;A;spaceterm=owning-shell\x07");
    let idle = owner.close_work_snapshot().unwrap();
    assert_eq!(idle.state(), CloseWorkState::Idle);
    assert_eq!(idle.observed_at(), MonotonicTick::new(60));
    assert_eq!(idle.generation(), first_work.generation());
    assert!(idle.sequence() > first_work.sequence());
    assert_eq!(owner.display_directory_snapshot(), Some(directory.clone()));
    clock.0.store(80, Ordering::Release);
    runtime_output(&pty, b"\x1b]133;C;spaceterm=owning-shell\x07");
    let running = owner.close_work_snapshot().unwrap();
    assert_eq!(running.state(), CloseWorkState::RunningWork);
    assert_eq!(running.observed_at(), MonotonicTick::new(80));
    assert!(running.generation() > idle.generation());
    assert_eq!(owner.display_directory_snapshot(), Some(directory));
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn oversized_directory_title_does_not_retire_real_vt_or_grant_restoration_authority() {
    use spaceterm_core::{observations::RuntimeObservationSource, runtime_authority::*};
    use spaceterm_terminal::{metadata::TitleProvenance, runtime::budget::RuntimeLimits};
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    pty.output(b"retained\x1b[5n");
    let request = request(1, support::grid(3, 16));
    let authority = request.authority();
    let mut owner = real_runtime(&factory, request, &pty);
    pty.until(|state| state.written == b"\x1b[0n");
    let handle = owner.handle();
    let before = runtime_stamp(&handle, authority, 1);
    let directory = format!("/{}", "x".repeat(257));
    pty.output(format!("\x1b]7;file://localhost{directory}\x07\x1b[5n").as_bytes());
    pty.until(|state| state.written.len() == 8 || state.terminated != 0);
    assert!(matches!(
        owner.lifecycle_snapshot(),
        RuntimeLifecycle::Running
    ));
    assert_eq!(pty.inspect(|state| state.terminated), 0);
    let metadata = owner.metadata_snapshot().unwrap();
    assert_eq!(&*metadata.parts().title.value, "x".repeat(256));
    assert_eq!(
        metadata.parts().title.provenance,
        TitleProvenance::WorkingDirectory
    );
    assert_eq!(
        owner
            .display_directory_snapshot()
            .unwrap()
            .directory()
            .bytes(),
        directory.as_bytes()
    );
    assert!(owner.confirmed_directory_snapshot().is_none());
    let current = runtime_stamp(&handle, authority, 3);
    assert_eq!(current.base().session, before.base().session);
    let ContentReadResult::Ready(reply) = handle
        .read_page(
            ContentPageRequest::new(
                current,
                ContentRange::ActiveGrid,
                None,
                PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap()
    else {
        panic!("retained page after directory title");
    };
    let text: String = reply.page().parts().rows[0]
        .cells()
        .iter()
        .map(|cell| cell.content().text())
        .collect();
    assert!(text.starts_with("retained"));
    assert_eq!(
        handle
            .try_submit(fence(authority), ime("usable"))
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    pty.until(|state| state.written.ends_with(b"usable"));
    // Only the later matching Prompt Owner marker confirms the unmodified path.
    pty.output(b"\x1b]133;A;spaceterm=owning-shell\x07\x1b[5n");
    pty.until(|state| state.written.ends_with(b"usable\x1b[0n"));
    assert_eq!(
        owner
            .confirmed_directory_snapshot()
            .unwrap()
            .directory()
            .bytes(),
        directory.as_bytes()
    );
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn public_runtime_reads_real_vt_pages_copy_find_and_hidden_input() {
    use spaceterm_terminal::{
        content::runtime::SemanticRuntimeEngine,
        local_filesystem::LocalFilesystemAuthority,
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, TerminalMetadataContext},
        runtime::budget::RuntimeLimits,
    };
    use support::*;
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    // DSR is a public PTY barrier proving the real VT consumed preceding text.
    pty.output(b"ab  cd\x1b[5n");
    let request = request(1, support::grid(3, 4));
    let authority = request.authority();
    let metadata = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/owning-server",
        LocalMachine::new(None, None, None),
    );
    let mut owner = factory
        .start(
            request,
            pty.launch(),
            SemanticRuntimeEngine::launch(metadata, LocalFilesystemAuthority::testing()),
        )
        .unwrap();
    owner.wait_until_running().unwrap();
    pty.until(|state| state.written == b"\x1b[0n");
    let handle = owner.handle();
    let stamp = runtime_stamp(&handle, authority, 1);
    let request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let ContentReadResult::Ready(reply) = handle
        .read_page(request)
        .unwrap()
        .receive_blocking()
        .unwrap()
    else {
        panic!("real VT page")
    };
    let rows = &reply.page().parts().rows;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].cells()[0].content().text(), "a");
    let cell = |row: usize, column| CellRef {
        session: stamp.base().session,
        row: rows[row].id(),
        row_revision: rows[row].revision(),
        column,
    };
    let selected = ContentSelection::new(stamp, cell(0, 0), cell(2, 3), false).unwrap();
    let copy = CopyRequest::new(
        stamp,
        selected,
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: true,
        },
    )
    .unwrap();
    let mut stream = handle.prepare_copy(copy).unwrap();
    let mut plain = Vec::new();
    let mut html = Vec::new();
    let mut completion = None;
    while let Some(item) = stream.next_blocking().unwrap() {
        match item {
            CopyStreamItem::Chunk(chunk) => {
                let output = if chunk.part() == CopyPart::Plain {
                    &mut plain
                } else {
                    &mut html
                };
                assert_eq!(chunk.offset(), output.len() as u64);
                output.extend_from_slice(chunk.bytes());
            }
            CopyStreamItem::Complete(value) => completion = Some(value),
            CopyStreamItem::Failed { .. } => panic!("real VT Copy failed"),
        }
    }
    assert_eq!(plain, b"ab  cd");
    assert!(String::from_utf8(html).unwrap().contains("ab  cd</div>"));
    let completion = completion.unwrap();
    assert_eq!(completion.plain_bytes(), 6);
    drop(stream);
    let mut repeated = handle.prepare_copy(copy).unwrap();
    let repeated_completion = loop {
        match repeated.next_blocking().unwrap() {
            Some(CopyStreamItem::Complete(value)) => break value,
            Some(CopyStreamItem::Chunk(_)) => {}
            _ => panic!("repeated Copy"),
        }
    };
    assert_ne!(completion.transfer(), repeated_completion.transfer());
    drop(repeated);
    let mut find = handle
        .find(FindRequest::new(stamp, BoundedFindQuery::new("B  c".into()).unwrap(), None).unwrap())
        .unwrap();
    let mut matches = Vec::new();
    let mut complete = false;
    while let Some(item) = find.next_blocking().unwrap() {
        match item {
            FindStreamItem::Chunk(chunk) => matches.extend_from_slice(chunk.matches()),
            FindStreamItem::Complete { .. } => complete = true,
            FindStreamItem::Failed { .. } => panic!("real VT Find failed"),
        }
    }
    assert!(complete);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start(), cell(0, 1));
    assert_eq!(matches[0].end(), cell(1, 0));
    pty.update(|state| state.hidden = true);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !owner
        .metadata_snapshot()
        .is_some_and(|value| value.parts().hidden_input)
    {
        assert!(std::time::Instant::now() < end);
        std::thread::yield_now();
    }
    let hidden = runtime_stamp(&handle, authority, 3);
    let request =
        ContentPageRequest::new(hidden, ContentRange::ActiveGrid, None, request.limits()).unwrap();
    let ContentReadResult::Ready(reply) = handle
        .read_page(request)
        .unwrap()
        .receive_blocking()
        .unwrap()
    else {
        panic!("hidden input page")
    };
    assert!(reply.page().parts().modes.hidden_input);
    // Plain input is encoded without scrolling, selection or Find routing.
    assert_eq!(
        handle
            .try_submit(fence(authority), ime("input"))
            .unwrap()
            .receive_blocking()
            .unwrap(),
        spaceterm_core::runtime_authority::EffectDisposition::Applied
    );
    pty.until(|state| state.written.ends_with(b"input"));
    assert_eq!(
        handle
            .try_submit(
                fence(authority),
                spaceterm_terminal::runtime_contracts::RuntimeCommand::CellPixelMetrics(
                    spaceterm_core::geometry::CellPixels::new(5, 10).unwrap()
                )
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        spaceterm_core::runtime_authority::EffectDisposition::Applied
    );
    assert_eq!(
        pty.inspect(|state| *state.sizes.last().unwrap()),
        spaceterm_terminal::runtime::ports::PtySize {
            rows: 3,
            columns: 4,
            pixel_width: 20,
            pixel_height: 30
        }
    );
    let resized = runtime_stamp(&handle, authority, 5);
    assert!(resized.base().revision > hidden.base().revision);
    let resized_page =
        ContentPageRequest::new(resized, ContentRange::ActiveGrid, None, request.limits()).unwrap();
    let ContentReadResult::Ready(reply) = handle
        .read_page(resized_page)
        .unwrap()
        .receive_blocking()
        .unwrap()
    else {
        panic!("resized page")
    };
    assert_eq!(reply.page().parts().rows.len(), 3);
    assert_eq!(reply.page().parts().columns, 4);
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_runtime_retains_matching_owner_confirmation_source_ticks() {
    use spaceterm_core::observations::{MonotonicTick, RuntimeObservationSource};
    use spaceterm_terminal::{
        content::runtime::SemanticRuntimeEngine,
        local_filesystem::LocalFilesystemAuthority,
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, PromptOwner, TerminalMetadataContext},
        runtime::{budget::RuntimeLimits, owner::RuntimeFactory},
    };
    use std::sync::{Arc, atomic::Ordering};
    use support::*;
    let clock = Arc::new(Clock::default());
    clock.0.store(20, Ordering::Release);
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let pty = Pty::default();
    pty.output(b"\x1b]7;file://localhost/first\x07\x1b]133;A;spaceterm=owning-shell\x07\x1b[5n");
    let request = request(1, support::grid(3, 8));
    let metadata = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/owning-server",
        LocalMachine::new(None, Some("localhost"), None),
    )
    .with_prompt_owner(PromptOwner::fixture("owning-shell"));
    let mut owner = factory
        .start(
            request,
            pty.launch(),
            SemanticRuntimeEngine::launch(metadata, LocalFilesystemAuthority::testing()),
        )
        .unwrap();
    owner.wait_until_running().unwrap();
    pty.until(|state| state.written == b"\x1b[0n");
    let first = owner.confirmed_directory_snapshot().unwrap();
    assert_eq!(first.directory().bytes(), b"/first");
    assert_eq!(first.observed_at(), MonotonicTick::new(20));
    clock.0.store(40, Ordering::Release);
    pty.output(b"\x1b]7;file://localhost/nested\x07\x1b]133;A;spaceterm=nested\x07\x1b[5n");
    pty.until(|state| state.written.len() == 8);
    assert_eq!(owner.confirmed_directory_snapshot(), Some(first.clone()));
    assert_eq!(
        owner.display_directory_snapshot().unwrap().observed_at(),
        MonotonicTick::new(40)
    );
    clock.0.store(50, Ordering::Release);
    pty.output(b"unrelated\x1b[5n");
    pty.until(|state| state.written.len() == 12);
    assert_eq!(owner.confirmed_directory_snapshot(), Some(first));
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn manifest_eviction_keeps_an_admitted_immutable_graphics_job_alive() {
    use spaceterm_terminal::content::{graphics::GraphicsStep, pages::ContentScanBudget};
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(80, 3);
    let bytes = 384 * 192 * 4;
    emulator.feed(
        format!(
            "\x1b_Ga=T,t=d,f=32,i=1,s=384,v=192;{}\x1b\\\r\n\r\n\r\n",
            "/".repeat(bytes / 3 * 4)
        )
        .as_bytes(),
    );
    for id in 2..=1030 {
        emulator.feed(
            format!("\x1b_Ga=T,t=d,f=32,i={id},s=1,v=1;AQIDBA==\x1b\\\r\n\r\n\r\n").as_bytes(),
        );
    }
    let stamp = stamp(&emulator);
    let mut request = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let mut rows = Vec::new();
    loop {
        let ContentReadResult::Ready(page) = emulator.topology_page(&request).unwrap() else {
            panic!("topology")
        };
        rows.extend(page.rows().iter().map(|row| row.row()));
        let Some(next) = page.continuation() else {
            break;
        };
        request = TopologyPageRequest::new(stamp, Some(next), request.limits()).unwrap();
    }
    let first = graphics_page(
        &mut emulator,
        ContentRange::History {
            first: Some(rows[0]),
            last: Some(rows[0]),
        },
    );
    let resource = first.page().parts().resources[0];
    let request =
        GraphicsRequest::new(stamp, ResourceTransferId::new(99).unwrap(), resource, 0).unwrap();
    let mut job = emulator.begin_graphics(&request).unwrap();
    // Warm more than the independently bounded manifest cache without any VT
    // mutation. Page-local descriptors remain valid while older cache entries go.
    for window in rows[20..].chunks(100) {
        let _ = graphics_page(
            &mut emulator,
            ContentRange::History {
                first: Some(window[0]),
                last: Some(*window.last().unwrap()),
            },
        );
    }
    assert_eq!(emulator.content_base().unwrap(), stamp.base());
    let mut received = 0;
    while let GraphicsStep::Item(chunk) = emulator
        .step_graphics(&mut job, ContentScanBudget::default())
        .unwrap()
    {
        assert!(chunk.bytes().iter().all(|byte| *byte == 255));
        received += chunk.bytes().len();
    }
    assert_eq!(received, bytes);
    // The cold reference is no longer an admission proof. Re-reading its page
    // prepares it again, without making the admitted job depend on cache residency.
    assert!(matches!(
        emulator.begin_graphics(&request),
        Err(ContentReadFailure::MissingResource)
    ));
    let refreshed = graphics_page(
        &mut emulator,
        ContentRange::History {
            first: Some(rows[0]),
            last: Some(rows[0]),
        },
    );
    assert_eq!(refreshed.page().parts().resources[0], resource);
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn graphics_descriptor_bytes_shorten_the_text_page_with_a_continuation() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut emulator = emulator(80, 3);
    for id in 1..=10 {
        emulator.feed(format!("\x1b_Ga=T,t=d,f=32,i={id},s=1,v=1;AQIDBA==\x1b\\").as_bytes());
        for _ in 0..20 {
            emulator.feed(b"\r\n");
        }
    }
    let reply = graphics_page(
        &mut emulator,
        ContentRange::History {
            first: None,
            last: None,
        },
    );
    assert!(reply.page().accounted_bytes() <= 1024 * 1024);
    assert!(reply.continuation().unwrap().is_some());
    assert!(!reply.page().parts().resources.is_empty());
    assert!(!reply.page().parts().placements.is_empty());
}

#[cfg(feature = "kitty-graphics")]
#[test]
fn relative_graphics_resolve_virtual_roots_outside_the_page_and_clip_pruned_rows() {
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let mut virtual_root = emulator(8, 3);
    virtual_root.feed(b"\x1b_Ga=T,t=d,f=32,i=1,p=1,s=1,v=1,U=1,c=1,r=1;AQIDBA==\x1b\\\x1b_Ga=T,t=d,f=32,i=2,p=1,s=1,v=1,c=2,r=1,P=1,Q=1,H=-1,V=2;AQIDBA==\x1b\\");
    virtual_root
        .feed("\x1b[38;2;0;0;1m\u{10eeee}\u{305}\u{305}\x1b[0m\r\nnext\r\nlast\r\n".as_bytes());
    let page = graphics_page(&mut virtual_root, ContentRange::ActiveGrid);
    assert_eq!(page.page().parts().placements.len(), 1);
    let child = page.page().parts().placements[0];
    assert_eq!(child.resource.id.get(), 3);
    assert_eq!(child.column, -1);
    assert!(!child.unicode_placeholder);
    assert_eq!(child.anchor, page.page().parts().rows[1].id());

    let mut clipped = emulator(8, 3);
    let pixels = "/".repeat(64);
    clipped.feed(format!("\x1b_Ga=T,t=d,f=32,i=1,p=1,s=4,v=3,c=1,r=1;{pixels}\x1b\\\x1b_Ga=T,t=d,f=32,i=2,p=1,s=4,v=3,c=2,r=3,P=1,Q=1,H=-1,V=-1;{pixels}\x1b\\").as_bytes());
    let page = graphics_page(&mut clipped, ContentRange::ActiveGrid);
    let child = page
        .page()
        .parts()
        .placements
        .iter()
        .find(|p| p.resource.id.get() == 3)
        .unwrap();
    assert_eq!(child.column, -1);
    assert_eq!(child.anchor, page.page().parts().rows[0].id());
    assert_eq!(child.rows, 2);
    assert_eq!(
        child.source,
        ImageRect {
            x: 0,
            y: 1,
            width: 4,
            height: 2
        }
    );
    assert_eq!(
        (child.destination_width, child.destination_height),
        (20, 40)
    );
}

#[cfg(feature = "runtime")]
#[test]
fn real_vt_slow_find_cancellation_releases_scratch_and_keeps_pty_input_progressing() {
    use spaceterm_terminal::{
        content::runtime::SemanticRuntimeEngine,
        local_filesystem::LocalFilesystemAuthority,
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, TerminalMetadataContext},
        runtime::budget::RuntimeLimits,
    };
    use support::*;
    let factory = factory(RuntimeLimits {
        query_scratch_bytes: 2 * 1024 * 1024,
        ..Default::default()
    });
    let pty = Pty::default();
    for _ in 0..300 {
        pty.output(format!("{}\r\n", "x".repeat(80)).as_bytes());
    }
    pty.output(b"\x1b[5n");
    let request = request(1, support::grid(3, 80));
    let authority = request.authority();
    let metadata = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/owning-server",
        LocalMachine::new(None, None, None),
    );
    let mut owner = factory
        .start(
            request,
            pty.launch(),
            SemanticRuntimeEngine::launch(metadata, LocalFilesystemAuthority::testing()),
        )
        .unwrap();
    owner.wait_until_running().unwrap();
    pty.until(|state| state.written == b"\x1b[0n");
    let handle = owner.handle();
    let stamp = runtime_stamp(&handle, authority, 1);
    // The topology reply can arrive before its sender drops the scratch lease.
    // A FIFO command acknowledgement joins that release before admitting both Finds.
    assert_eq!(
        handle
            .try_submit(
                fence(authority),
                spaceterm_terminal::runtime_contracts::RuntimeCommand::FocusReport(true),
            )
            .unwrap()
            .receive_blocking()
            .unwrap(),
        spaceterm_core::runtime_authority::EffectDisposition::Applied,
    );
    let find = |id| {
        FindRequest::new(
            QueryStamp::new(
                stamp.fence(),
                stamp.base(),
                ContentRequestId::new(id).unwrap(),
                RequestGeneration::new(1),
            )
            .unwrap(),
            BoundedFindQuery::new("x".into()).unwrap(),
            None,
        )
        .unwrap()
    };
    let mut first = handle.find(find(10)).unwrap();
    let mut second = handle.find(find(11)).unwrap();
    assert!(matches!(
        first.next_blocking().unwrap(),
        Some(FindStreamItem::Chunk(_))
    ));
    assert!(
        matches!(handle.find(find(12)), Err(error) if error.class == spaceterm_core::organization::FailureClass::ResourceLimit)
    );
    assert_eq!(
        handle
            .try_submit(fence(authority), ime("progress"))
            .unwrap()
            .receive_blocking()
            .unwrap(),
        spaceterm_core::runtime_authority::EffectDisposition::Applied
    );
    pty.until(|state| state.written.ends_with(b"progress"));
    first.cancel();
    second.cancel();
    drop(first);
    drop(second);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut replacement = loop {
        match handle.find(find(12)) {
            Ok(stream) => break stream,
            Err(error) => {
                assert_eq!(
                    error.class,
                    spaceterm_core::organization::FailureClass::ResourceLimit
                );
                assert!(std::time::Instant::now() < end);
                std::thread::yield_now();
            }
        }
    };
    assert!(matches!(
        replacement.next_blocking().unwrap(),
        Some(FindStreamItem::Chunk(_))
    ));
    replacement.cancel();
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}

#[cfg(all(feature = "runtime", feature = "kitty-graphics"))]
#[test]
fn public_runtime_streams_real_session_qualified_graphics() {
    use spaceterm_terminal::{
        content::runtime::SemanticRuntimeEngine,
        local_filesystem::LocalFilesystemAuthority,
        local_path::LocalPathSemantics,
        metadata::{LocalMachine, TerminalMetadataContext},
        runtime::budget::RuntimeLimits,
    };
    use support::*;
    let _graphics = spaceterm_terminal::graphics::test_lock();
    let factory = factory(RuntimeLimits::default());
    let pty = Pty::default();
    let total = 384 * 192 * 4;
    pty.output(
        format!(
            "\x1b_Ga=T,t=d,f=32,i=1,s=384,v=192;{}\x1b\\\x1b[5n",
            "/".repeat(total / 3 * 4)
        )
        .as_bytes(),
    );
    let request = request(1, support::grid(24, 80));
    let authority = request.authority();
    let metadata = TerminalMetadataContext::local(
        LocalPathSemantics::Posix,
        "/owning-server",
        LocalMachine::new(None, None, None),
    );
    let mut owner = factory
        .start(
            request,
            pty.launch(),
            SemanticRuntimeEngine::launch(metadata, LocalFilesystemAuthority::testing()),
        )
        .unwrap();
    owner.wait_until_running().unwrap();
    pty.until(|state| state.written.ends_with(b"\x1b[0n"));
    let handle = owner.handle();
    let stamp = runtime_stamp(&handle, authority, 1);
    let request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let reply = loop {
        match handle
            .read_page(request)
            .unwrap()
            .receive_blocking()
            .unwrap()
        {
            ContentReadResult::Ready(reply) => break reply,
            ContentReadResult::Rejected {
                reason: ContentReadFailure::Capacity,
                ..
            } => assert!(std::time::Instant::now() < end),
            _ => panic!("graphics page"),
        }
    };
    let resource = reply.page().parts().resources[0];
    assert_eq!(resource.key.session, authority.session());
    let transfer = ResourceTransferId::new(123).unwrap();
    let request = GraphicsRequest::new(stamp, transfer, resource, 0).unwrap();
    let mut stream = handle.read_graphics(request).unwrap();
    let mut duplicate = handle.read_graphics(request).unwrap();
    assert!(
        matches!(duplicate.next_blocking(), Err(error) if error.class == spaceterm_core::organization::FailureClass::InvalidRequest)
    );
    drop(duplicate);
    let mut received = 0;
    let mut complete = false;
    while let Some(chunk) = stream.next_blocking().unwrap() {
        assert_eq!(chunk.request().transfer(), transfer);
        assert_eq!(chunk.request().resource(), resource);
        assert_eq!(chunk.offset(), received as u32);
        assert!(chunk.bytes().len() <= 256 * 1024);
        assert!(chunk.bytes().iter().all(|byte| *byte == 255));
        received += chunk.bytes().len();
        complete |= chunk.complete();
    }
    assert_eq!(received, total);
    assert!(complete);
    owner
        .retire(spaceterm_core::organization::RetirementReason::Stop)
        .wait_blocking()
        .unwrap();
    owner.join().unwrap();
}
