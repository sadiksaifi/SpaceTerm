//! Explicit-use and measured-start conversions. Geometry never creates an intent.

use super::identity::*;
use crate::ProtocolError;
use spaceterm_core::{control::*, geometry::*, operations::*};

fn pixels(width: u32, height: u32) -> Result<CellPixels, ProtocolError> {
    CellPixels::new(
        u16::try_from(width).map_err(|_| ProtocolError::Malformed)?,
        u16::try_from(height).map_err(|_| ProtocolError::Malformed)?,
    )
    .map_err(|_| ProtocolError::Malformed)
}
macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            pub fn encode_grid(value: GridSize) -> wire::Geometry {
                wire::Geometry {
                    rows: u32::from(value.rows()),
                    columns: u32::from(value.columns()),
                    cell_width_px: u32::from(value.cell_pixels().width()),
                    cell_height_px: u32::from(value.cell_pixels().height()),
                }
            }
            pub fn decode_grid(value: &wire::Geometry) -> Result<GridSize, ProtocolError> {
                GridSize::new(
                    value.rows,
                    value.columns,
                    pixels(value.cell_width_px, value.cell_height_px)?,
                    SUPPORTED_GRID_LIMITS,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_measured(value: MeasuredGrid) -> wire::MeasuredGrid {
                wire::MeasuredGrid {
                    rows: u32::from(value.rows()),
                    columns: u32::from(value.columns()),
                    cell_width_px: u32::from(value.cell_pixels().width()),
                    cell_height_px: u32::from(value.cell_pixels().height()),
                }
            }
            pub fn decode_measured(
                value: &wire::MeasuredGrid,
            ) -> Result<MeasuredGrid, ProtocolError> {
                MeasuredGrid::new(
                    value.rows,
                    value.columns,
                    pixels(value.cell_width_px, value.cell_height_px)?,
                    SUPPORTED_GRID_LIMITS,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_request(value: &Command) -> Result<wire::StartupIntent, ProtocolError> {
                let (id, fence, source, tab, measured, measured_grid) = match value {
                    Command::BeginStartup(intent) => {
                        let (source, tab) = match intent.source() {
                            StartupSource::ExplicitSelection => (1, None),
                            StartupSource::TabOpening { tab } => (2, Some(encode_tab(tab))),
                            StartupSource::StartSession => (3, None),
                        };
                        (intent.id(), intent.fence(), source, tab, None, None)
                    }
                    Command::CompleteStartup(value) => (
                        value.intent,
                        value.fence,
                        0,
                        None,
                        Some(encode_grid(value.grid)),
                        None,
                    ),
                    Command::CompleteMeasuredStartup(value) => (
                        value.intent,
                        value.fence,
                        0,
                        None,
                        None,
                        Some(encode_measured(value.grid)),
                    ),
                    _ => return Err(ProtocolError::UnexpectedMessage),
                };
                // Startup's frozen wire fence has no content-comparison field.
                // Reject an unrepresentable command instead of changing its digest.
                if fence.content().is_some() {
                    return Err(ProtocolError::Malformed);
                }
                Ok(wire::StartupIntent {
                    fence: Some(encode_fence(fence)),
                    intent: id.sequence().get(),
                    source,
                    measured,
                    cancel: false,
                    original_intent: Some(encode_operation(id)),
                    tab,
                    measured_grid,
                })
            }
            pub fn decode_request(
                value: &wire::StartupIntent,
                operation: OperationId,
            ) -> Result<Command, ProtocolError> {
                if value.cancel || value.measured.is_some() && value.measured_grid.is_some() {
                    return Err(ProtocolError::Malformed);
                }
                let id = decode_operation(required(&value.original_intent)?)?;
                if value.intent != id.sequence().get() || id.stream() != operation.stream() {
                    return Err(ProtocolError::Malformed);
                }
                let fence = decode_fence(required(&value.fence)?, None)?;
                if value.measured.is_some() || value.measured_grid.is_some() {
                    if value.source != 0 || value.tab.is_some() {
                        return Err(ProtocolError::Malformed);
                    }
                    return match (&value.measured, &value.measured_grid) {
                        (Some(grid), None) => Ok(Command::CompleteStartup(StartupMeasurement {
                            intent: id,
                            fence,
                            grid: decode_grid(grid)?,
                        })),
                        (None, Some(grid)) => Ok(Command::CompleteMeasuredStartup(
                            MeasuredStartupMeasurement {
                                intent: id,
                                fence,
                                grid: decode_measured(grid)?,
                            },
                        )),
                        _ => Err(ProtocolError::Malformed),
                    };
                }
                if id != operation {
                    return Err(ProtocolError::Malformed);
                }
                let source = match (value.source, &value.tab) {
                    (1, None) => StartupSource::ExplicitSelection,
                    (2, Some(tab)) => StartupSource::TabOpening {
                        tab: decode_tab(tab)?,
                    },
                    (3, None) => StartupSource::StartSession,
                    _ => return Err(ProtocolError::Malformed),
                };
                StartupIntent::new(id, fence, source)
                    .map(Command::BeginStartup)
                    .map_err(|_| ProtocolError::Malformed)
            }
        }
    };
}
profile!(v1);
profile!(v2);

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{authority::*, identity::*};
    macro_rules! cases {
        ($name:ident,$p:ident) => {
            #[test]
            fn $name() {
                let server = ServerId::new([1; 16]);
                let client = ClientId::new(server, 2).unwrap();
                let pane = PaneId::new(server, 3).unwrap();
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server, 4).unwrap(),
                    PairingId::new(server, 5).unwrap(),
                    client,
                )
                .unwrap();
                let id = OperationId::new(stream, OperationSequence::new(1).unwrap());
                let fence = EffectFence::new(
                    SessionKey {
                        epoch: ServerEpoch::new(6),
                        pane,
                        generation: SessionGeneration::new(0),
                    },
                    ControlGrant::new(pane, client, ControlGeneration::new(7)).unwrap(),
                    ConnectionBinding::new(client, ConnectionGeneration::new(8)),
                    ContextGeneration::new(9),
                    None,
                )
                .unwrap();
                let begin = Command::BeginStartup(
                    StartupIntent::new(id, fence, StartupSource::ExplicitSelection).unwrap(),
                );
                let payload = match stringify!($p) {
                    "v1" => crate::codec::Payload::Major1(crate::wire::v1::Message {
                        kind: Some(crate::wire::v1::message::Kind::StartupIntent(
                            v1::encode_request(&begin).unwrap(),
                        )),
                    }),
                    _ => crate::codec::Payload::Major2(crate::wire::v2::Message {
                        kind: Some(crate::wire::v2::message::Kind::StartupIntent(
                            v2::encode_request(&begin).unwrap(),
                        )),
                    }),
                };
                payload
                    .encode()
                    .expect("initial generation must pass actual admission");
                assert_eq!(
                    $p::decode_request(&$p::encode_request(&begin).unwrap(), id),
                    Ok(begin)
                );
                let measured = MeasuredGrid::new(
                    40,
                    100,
                    CellPixels::new(1000, 12).unwrap(),
                    SUPPORTED_GRID_LIMITS,
                )
                .unwrap();
                let complete = Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: id,
                    fence,
                    grid: measured,
                });
                let payload = match stringify!($p) {
                    "v1" => crate::codec::Payload::Major1(crate::wire::v1::Message {
                        kind: Some(crate::wire::v1::message::Kind::StartupIntent(
                            v1::encode_request(&complete).unwrap(),
                        )),
                    }),
                    _ => crate::codec::Payload::Major2(crate::wire::v2::Message {
                        kind: Some(crate::wire::v2::message::Kind::StartupIntent(
                            v2::encode_request(&complete).unwrap(),
                        )),
                    }),
                };
                payload
                    .encode()
                    .expect("initial measured generation must pass actual admission");
                let runtime = match stringify!($p) {
                    "v1" => crate::codec::Payload::Major1(crate::wire::v1::Message {
                        kind: Some(crate::wire::v1::message::Kind::RuntimeCommand(
                            crate::wire::v1::RuntimeCommand {
                                fence: Some(encode_fence(fence)),
                                request: 1,
                                action: Some(
                                    crate::wire::v1::runtime_command::Action::InputMethodText(
                                        "x".into(),
                                    ),
                                ),
                            },
                        )),
                    }),
                    _ => crate::codec::Payload::Major2(crate::wire::v2::Message {
                        kind: Some(crate::wire::v2::message::Kind::RuntimeCommand(
                            crate::wire::v2::RuntimeCommand {
                                fence: Some(encode_fence(fence)),
                                request: 1,
                                action: Some(
                                    crate::wire::v2::runtime_command::Action::InputMethodText(
                                        "x".into(),
                                    ),
                                ),
                            },
                        )),
                    }),
                };
                assert_eq!(runtime.encode(), Err(ProtocolError::Malformed));
                let operation = OperationId::new(stream, OperationSequence::new(2).unwrap());
                assert_eq!(
                    $p::decode_request(&$p::encode_request(&complete).unwrap(), operation),
                    Ok(complete)
                );
                assert_eq!(measured.pty_pixels(), (0, 480));
                let mut substituted = $p::encode_request(&Command::BeginStartup(
                    StartupIntent::new(id, fence, StartupSource::StartSession).unwrap(),
                ))
                .unwrap();
                substituted.intent = 2;
                assert_eq!(
                    $p::decode_request(&substituted, id),
                    Err(ProtocolError::Malformed)
                );
                let content_bound = EffectFence::new(
                    fence.session(),
                    fence.grant(),
                    fence.connection(),
                    fence.context(),
                    Some(ContentRevision::new(1)),
                )
                .unwrap();
                let begin = Command::BeginStartup(
                    StartupIntent::new(id, content_bound, StartupSource::StartSession).unwrap(),
                );
                assert_eq!($p::encode_request(&begin), Err(ProtocolError::Malformed));
            }
        };
    }
    cases!(
        major_1_startup_requires_original_explicit_intent_and_preserves_measured_pixels,
        v1
    );
    cases!(
        major_2_startup_requires_original_explicit_intent_and_preserves_measured_pixels,
        v2
    );
}
