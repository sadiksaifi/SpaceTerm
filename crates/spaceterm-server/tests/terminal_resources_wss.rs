//! Real PTYs, semantic pages and graphics over authenticated production WSS.
#![cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "support/authenticated_routes.rs"]
mod route_fixture;
use route_fixture::*;
use sha2::{Digest, Sha256};
use spaceterm_core::content::GraphicsResourceRef;

fn page(payload: &Payload) -> Option<(ContentPageReply, bool, u32)> {
    macro_rules! decode {
        ($value:expr, $p:ident) => {{
            let value = $value;
            let Some($p::ContentReadResponse {
                result: Some($p::content_read_response::Result::Page(page)),
            }) = &value.response
            else {
                return None;
            };
            let stamp = content::$p::decode_stamp(page.stamp.as_ref().unwrap()).unwrap();
            let request = ContentPageRequest::new(
                stamp,
                ContentRange::ActiveGrid,
                None,
                PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
            )
            .unwrap();
            Some((
                content::$p::decode_page(page, &request).unwrap(),
                value.complete,
                value.index,
            ))
        }};
    }
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::ContentPublication(value)),
        }) => decode!(value, v1),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::ContentPublication(value)),
        }) => decode!(value, v2),
        _ => None,
    }
}
async fn create(client: &mut Client, name: &str) -> ControlGrant {
    let outcome = client
        .command(Command::CreateWorkspace {
            server: client.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: client.stream.server(),
            },
            name: NameDraft::Custom(NameText::new(name.into()).unwrap()),
        })
        .await;
    let Outcome::CreatedWithControl(value) = outcome else {
        panic!("creation grants control: {outcome:?}")
    };
    value.control().grant()
}
fn resource_request(
    major: u32,
    stamp: QueryStamp,
    resource: GraphicsResourceRef,
    transfer: u64,
) -> Payload {
    macro_rules! encode {
        ($p:ident, $major:ident) => {
            Payload::$major($p::Message {
                kind: Some($p::message::Kind::ResourceRequest($p::ResourceRequest {
                    fence: Some(identity::encode_fence(stamp.fence())),
                    resource: Some($p::ResourceId {
                        session: Some(identity::encode_session(resource.key.session)),
                        local_id: resource.key.id.get(),
                        generation: resource.key.generation,
                    }),
                    transfer,
                    offset: 0,
                    stamp: Some(Box::new(content::$p::encode_stamp(stamp))),
                    width: resource.width,
                    height: resource.height,
                    rgba_bytes: resource.rgba_bytes,
                    digest: resource.digest.to_vec(),
                })),
            })
        };
    }
    if major == 1 {
        encode!(v1, Major1)
    } else {
        encode!(v2, Major2)
    }
}

#[cfg(feature = "graphics")]
async fn graphics(major: u32, chunk_bytes: usize, transfers: u64) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    routes.enable_wss(major).await;
    let limits = spaceterm_protocol::limits::NegotiatedLimits {
        resource_chunk_bytes: chunk_bytes,
        concurrent_transfers: transfers as usize,
        ..Default::default()
    };
    let mut client = Client::connect_limits(&mut routes, 0, major, limits).await;
    assert_eq!(client.codec.limits().resource_chunk_bytes, chunk_bytes);
    client.sequence = 2;
    client.reconcile(&fixture.handle).await;
    let grant = create(&mut client, "graphics").await;
    let session = client.start(&fixture.handle, grant).await;
    let fence = client.fence(session, grant);
    client.subscribe(fence).await;
    // Cat the APC through the real shell and PTY, never inject an emulator or
    // manufacture a resource. 294,912 bytes requires at least two wire chunks.
    let size = 384 * 192 * 4;
    fs::write(
        fixture._root.path().join("image.apc"),
        format!(
            "\x1b_Ga=T,f=32,s=384,v=192,i=7,q=2;{}\x1b\\",
            "/".repeat(size / 3 * 4)
        ),
    )
    .unwrap();
    client.line(fence, "cat image.apc").await;
    let (stamp, resource) = tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            let payload = client.recv().await;
            if let Some((page, _, _)) = page(&payload)
                && let Some(resource) = page.page().parts().resources.first()
            {
                break (page.stamp(), *resource);
            }
        }
    })
    .await
    .expect("real PTY image produces a semantic graphics reference");
    assert_eq!(resource.rgba_bytes as usize, size);
    assert_eq!(
        resource.digest,
        <[u8; 32]>::from(Sha256::digest(vec![255; size]))
    );
    // Every requested transfer is in flight before reading a resource response.
    for transfer in 1..=transfers {
        client
            .send(resource_request(major, stamp, resource, transfer))
            .await;
    }
    let mut bytes = BTreeMap::<u64, Vec<u8>>::new();
    let mut chunks = BTreeMap::<u64, usize>::new();
    let mut complete = std::collections::BTreeSet::new();
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        while complete.len() < transfers as usize {
            let payload = client.recv().await;
            macro_rules! accept {
                ($value:expr, $p:ident) => {{
                    let value = $value;
                    assert!((1..=transfers).contains(&value.transfer));
                    assert!(value.data.len() <= chunk_bytes);
                    assert!(!value.data.is_empty());
                    assert_eq!(value.format, $p::ResourceFormat::Rgba8 as i32);
                    assert_eq!(value.total_bytes, size as u64);
                    assert_eq!(value.digest, resource.digest);
                    assert_eq!(
                        content::$p::decode_stamp(value.stamp.as_ref().unwrap()).unwrap(),
                        stamp
                    );
                    let target = bytes.entry(value.transfer).or_default();
                    assert_eq!(value.offset, target.len() as u64);
                    target.extend_from_slice(&value.data);
                    *chunks.entry(value.transfer).or_default() += 1;
                    if value.complete {
                        assert!(complete.insert(value.transfer));
                    }
                }};
            }
            match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ResourceChunk(value)),
                }) => accept!(value, v1),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ResourceChunk(value)),
                }) => accept!(value, v2),
                other => {
                    assert!(
                        page(&other).is_some(),
                        "unexpected resource reply: {other:?}"
                    );
                }
            }
        }
    })
    .await
    .expect("four graphics streams complete over WSS");
    for transfer in 1..=transfers {
        assert_eq!(chunks[&transfer], size.div_ceil(chunk_bytes));
        assert_eq!(bytes[&transfer], vec![255; size]);
    }
    client.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}

async fn pages_and_slow_reader(major: u32) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    routes.enable_wss(major).await;
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.sequence = 2;
    a.reconcile(&fixture.handle).await;
    let mut b = Client::connect(&mut routes, 1, major).await;
    b.reconcile(&fixture.handle).await;
    let grant_a = create(&mut a, "slow reader").await;
    let grant_b = create(&mut b, "live reader").await;
    let (session_a, session_b) = tokio::join!(
        a.start_grid(&fixture.handle, grant_a, 300, 100),
        b.start(&fixture.handle, grant_b),
    );
    let fence_a = a.fence(session_a, grant_a);
    let fence_b = b.fence(session_b, grant_b);
    a.subscribe_limits(fence_a, PageLimits::new(256, 16384, 1024 * 1024).unwrap())
        .await;
    b.subscribe(fence_b).await;
    let mut pages = 0;
    let mut cells = 0;
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            let payload = a.recv().await;
            if let Some((page, complete, index)) = page(&payload) {
                if index == 0 {
                    pages = 0;
                    cells = 0;
                }
                let rows = &page.page().parts().rows;
                assert!(rows.len() <= 256);
                let count: usize = rows.iter().map(|row| row.cells().len()).sum();
                assert!(count <= 16384);
                cells += count;
                pages += 1;
                if complete {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(cells, 300 * 100);
    assert!(
        pages > 1,
        "large active grids must be paged, never oversized"
    );
    // Four 4 MiB graphics streams saturate a non-reading WSS connection.
    // The shell's later file write proves progress independently of egress;
    // generating millions of text rows would measure VT throughput instead.
    #[cfg(feature = "graphics")]
    {
        let size = 1024 * 1024 * 4;
        // Include base64 padding for a raster length not divisible by three.
        let encoded = format!("{}/w==", "/".repeat(size / 3 * 4));
        fs::write(
            fixture._root.path().join("slow-image.apc"),
            format!("\x1b_Ga=T,f=32,s=1024,v=1024,i=8,q=2;{encoded}\x1b\\"),
        )
        .unwrap();
        a.line(
            fence_a,
            "cat slow-image.apc; while [ ! -f continue-pty ]; do sleep 0.01; done; printf done > pty-progress",
        )
        .await;
        let (stamp, resource) = tokio::time::timeout(HARNESS_TIMEOUT, async {
            loop {
                let payload = a.recv().await;
                if let Some((page, _, _)) = page(&payload)
                    && let Some(resource) = page.page().parts().resources.first()
                {
                    break (page.stamp(), *resource);
                }
            }
        })
        .await
        .expect("real PTY creates the slow client's graphics resource");
        assert_eq!(resource.rgba_bytes as usize, size);
        for transfer in 1..=4 {
            a.send(resource_request(major, stamp, resource, transfer))
                .await;
        }
        // The native shell cannot write completion before the slow peer has
        // requested its graphics backlog. Only this isolated fixture owns the gate.
        fs::write(fixture._root.path().join("continue-pty"), b"continue").unwrap();
    }
    #[cfg(not(feature = "graphics"))]
    a.line(fence_a, "i=0; while [ $i -lt 60000 ]; do printf '%0100d\\n' $i; i=$((i+1)); done; printf done > pty-progress").await;
    b.line(fence_b, "printf 'OTHER-CLIENT-RESPONSIVE\\n'").await;
    b.output("OTHER-CLIENT-RESPONSIVE").await;
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        while fs::read(fixture._root.path().join("pty-progress"))
            .ok()
            .as_deref()
            != Some(b"done")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("slow WSS egress cannot stall the PTY");
    assert!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .any(|v| v.pane == grant_a.pane() && matches!(v.phase, SessionPhase::Running { .. }))
    );
    a.route.close().await;
    b.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
#[cfg(feature = "graphics")]
#[tokio::test]
async fn major_1_four_graphics_transfers_use_bounded_chunks() {
    graphics(1, 256 * 1024, 4).await
}
#[cfg(feature = "graphics")]
#[tokio::test]
async fn major_2_four_graphics_transfers_use_bounded_chunks() {
    graphics(2, 256 * 1024, 4).await
}
#[tokio::test]
async fn major_1_semantic_pages_and_slow_client_do_not_stall_live_work() {
    pages_and_slow_reader(1).await
}
#[tokio::test]
async fn major_2_semantic_pages_and_slow_client_do_not_stall_live_work() {
    pages_and_slow_reader(2).await
}

#[cfg(feature = "graphics")]
#[tokio::test]
async fn major_1_graphics_honor_smaller_negotiated_chunks() {
    graphics(1, 64 * 1024, 2).await
}
#[cfg(feature = "graphics")]
#[tokio::test]
async fn major_2_graphics_honor_smaller_negotiated_chunks() {
    graphics(2, 64 * 1024, 2).await
}
