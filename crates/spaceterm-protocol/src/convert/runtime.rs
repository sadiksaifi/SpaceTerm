//! Checked, transient terminal input. Native scan codes and local authority stay off wire.

use super::identity::{decode_fence, encode_fence, required};
use crate::ProtocolError;
use spaceterm_core::{
    content::{CellRef, Rgb},
    content_queries::QueryStamp,
    geometry::{CellPixels, MeasuredGrid, SUPPORTED_GRID_LIMITS},
};
use spaceterm_terminal::{
    InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey,
    appearance::{Appearance, AppearanceGeneration},
    pointer_input::{PointerButton, PointerPhase},
    runtime_contracts::*,
};

fn modifiers(value: u32) -> Result<InputModifiers, ProtocolError> {
    if value > 1023 {
        return Err(ProtocolError::Malformed);
    }
    Ok(InputModifiers {
        shift: value & 1 != 0,
        alt: value & 2 != 0,
        control: value & 4 != 0,
        platform: value & 8 != 0,
        caps_lock: value & 16 != 0,
        num_lock: value & 32 != 0,
        shift_right: value & 64 != 0,
        alt_right: value & 128 != 0,
        control_right: value & 256 != 0,
        platform_right: value & 512 != 0,
    })
}
fn modifier_bits(value: InputModifiers) -> u32 {
    [
        value.shift,
        value.alt,
        value.control,
        value.platform,
        value.caps_lock,
        value.num_lock,
        value.shift_right,
        value.alt_right,
        value.control_right,
        value.platform_right,
    ]
    .into_iter()
    .enumerate()
    .fold(0, |bits, (i, on)| bits | (u32::from(on) << i))
}
pub fn rgb(value: u32) -> Result<Rgb, ProtocolError> {
    if value > 0xff_ffff {
        return Err(ProtocolError::Malformed);
    }
    Ok(Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    })
}
pub fn rgb_bits(value: Rgb) -> u32 {
    (u32::from(value.r) << 16) | (u32::from(value.g) << 8) | u32::from(value.b)
}
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
            fn encode_cell(cell: CellRef) -> wire::CellRef {
                wire::CellRef {
                    session: Some(super::super::identity::encode_session(cell.session)),
                    screen_generation: cell.row.screen_generation,
                    row_id: cell.row.node_serial,
                    row_revision: cell.row_revision,
                    column: u32::from(cell.column),
                    physical_row: Some(super::super::content::$p::encode_row_id(cell.row)),
                }
            }
            fn decode_cell(
                value: &wire::CellRef,
                stamp: QueryStamp,
            ) -> Result<CellRef, ProtocolError> {
                let cell = CellRef {
                    session: super::super::identity::decode_session(required(&value.session)?)?,
                    row: super::super::content::$p::decode_row_id(required(&value.physical_row)?)?,
                    row_revision: value.row_revision,
                    column: u16::try_from(value.column).map_err(|_| ProtocolError::Malformed)?,
                };
                if cell.session != stamp.base().session
                    || !stamp.base().contains(cell.row)
                    || value.screen_generation != cell.row.screen_generation
                    || value.row_id != cell.row.node_serial
                {
                    return Err(ProtocolError::Malformed);
                }
                Ok(cell)
            }
            pub fn decode_command(
                value: &wire::RuntimeCommand,
            ) -> Result<FencedRuntimeCommand, ProtocolError> {
                if value.request == 0 {
                    return Err(ProtocolError::Malformed);
                }
                let stamp = match required(&value.action)? {
                    wire::runtime_command::Action::ApplicationPointer(mouse) => Some(
                        super::super::content::$p::decode_stamp(required(&mouse.stamp)?)?,
                    ),
                    wire::runtime_command::Action::ApplicationWheel(mouse) => Some(
                        super::super::content::$p::decode_stamp(required(&mouse.stamp)?)?,
                    ),
                    _ => None,
                };
                let fence = decode_fence(
                    required(&value.fence)?,
                    stamp.map(|stamp| stamp.base().revision),
                )?;
                if stamp.is_some_and(|stamp| stamp.fence() != fence) {
                    return Err(ProtocolError::Malformed);
                }
                let command = match required(&value.action)? {
                    wire::runtime_command::Action::Key(value) => {
                        if value.logical_key.len() > 1024
                            || value
                                .logical_key
                                .len()
                                .saturating_add(value.text.as_ref().map_or(0, String::len))
                                > 65536
                        {
                            return Err(ProtocolError::LimitExceeded);
                        }
                        if value
                            .text
                            .as_ref()
                            .is_some_and(|text| text.chars().any(char::is_control))
                        {
                            return Err(ProtocolError::Malformed);
                        }
                        let input = KeyInput {
                            action: match value.action {
                                1 => KeyAction::Press,
                                2 => KeyAction::Repeat,
                                3 => KeyAction::Release,
                                _ => return Err(ProtocolError::Malformed),
                            },
                            physical_key: PhysicalKey::try_from(value.code)
                                .map_err(|_| ProtocolError::Malformed)?,
                            native_key_code: None,
                            logical_key: value.logical_key.clone(),
                            text: value.text.clone(),
                            unshifted_codepoint: if value.unshifted_codepoint == 0 {
                                None
                            } else {
                                Some(
                                    char::from_u32(value.unshifted_codepoint)
                                        .ok_or(ProtocolError::Malformed)?,
                                )
                            },
                            modifiers: modifiers(value.modifiers)?,
                            consumed_modifiers: modifiers(value.consumed_modifiers)?,
                            option_as_alt: if value.option_as_alt {
                                OptionAsAltPolicy::Both
                            } else {
                                OptionAsAltPolicy::None
                            },
                        };
                        input.validate().map_err(|_| ProtocolError::Malformed)?;
                        RuntimeCommand::Key(input)
                    }
                    wire::runtime_command::Action::InputMethodText(value) => {
                        RuntimeCommand::InputMethodCommit(
                            BoundedInputMethodText::new(value.clone())
                                .map_err(|_| ProtocolError::LimitExceeded)?,
                        )
                    }
                    wire::runtime_command::Action::Paste(value) => {
                        // Approval uses the exact NativeCompletion contract, never opaque bytes.
                        if !value.approval.is_empty() {
                            return Err(ProtocolError::Malformed);
                        }
                        if value.text.len() > crate::limits::MAX_PASTE_BYTES {
                            return Err(ProtocolError::LimitExceeded);
                        }
                        RuntimeCommand::RequestPaste(
                            BoundedPaste::new(
                                std::str::from_utf8(&value.text)
                                    .map_err(|_| ProtocolError::Malformed)?
                                    .to_owned(),
                            )
                            .map_err(|_| ProtocolError::LimitExceeded)?,
                        )
                    }
                    wire::runtime_command::Action::Resize(value) => RuntimeCommand::Resize(
                        MeasuredGrid::new(
                            value.rows,
                            value.columns,
                            pixels(value.cell_width_px, value.cell_height_px)?,
                            SUPPORTED_GRID_LIMITS,
                        )
                        .map_err(|_| ProtocolError::LimitExceeded)?,
                    ),
                    wire::runtime_command::Action::MeasuredResize(value) => RuntimeCommand::Resize(
                        MeasuredGrid::new(
                            value.rows,
                            value.columns,
                            pixels(value.cell_width_px, value.cell_height_px)?,
                            SUPPORTED_GRID_LIMITS,
                        )
                        .map_err(|_| ProtocolError::LimitExceeded)?,
                    ),
                    wire::runtime_command::Action::Appearance(value) => {
                        let palette: [Rgb; 256] = value
                            .palette
                            .iter()
                            .copied()
                            .map(rgb)
                            .collect::<Result<Vec<_>, _>>()?
                            .try_into()
                            .map_err(|_| ProtocolError::Malformed)?;
                        RuntimeCommand::Appearance(Box::new(RuntimeAppearance {
                            generation: AppearanceGeneration::new(value.generation),
                            foreground: rgb(value.foreground)?,
                            background: rgb(value.background)?,
                            palette,
                            bold_as_bright: value.bold_as_bright,
                            appearance: if value.dark {
                                Appearance::Dark
                            } else {
                                Appearance::Light
                            },
                        }))
                    }
                    wire::runtime_command::Action::FocusReport(value) => {
                        RuntimeCommand::FocusReport(*value)
                    }
                    wire::runtime_command::Action::CellPixelMetrics(value) => {
                        RuntimeCommand::CellPixelMetrics(pixels(value.width, value.height)?)
                    }
                    wire::runtime_command::Action::ClearScreenAndScrollback(_) => {
                        RuntimeCommand::ClearScreenAndScrollback
                    }
                    wire::runtime_command::Action::ApplicationPointer(mouse) => {
                        let stamp =
                            super::super::content::$p::decode_stamp(required(&mouse.stamp)?)?;
                        RuntimeCommand::Pointer(ApplicationPointerInput {
                            stamp,
                            cell: decode_cell(required(&mouse.cell)?, stamp)?,
                            phase: match mouse.phase {
                                1 => PointerPhase::Press,
                                2 => PointerPhase::Motion,
                                3 => PointerPhase::Release,
                                _ => return Err(ProtocolError::Malformed),
                            },
                            button: match mouse.button {
                                0 => None,
                                1 => Some(PointerButton::Left),
                                2 => Some(PointerButton::Middle),
                                3 => Some(PointerButton::Right),
                                _ => return Err(ProtocolError::Malformed),
                            },
                            modifiers: modifiers(mouse.modifiers)?,
                        })
                    }
                    wire::runtime_command::Action::ApplicationWheel(mouse) => {
                        let stamp =
                            super::super::content::$p::decode_stamp(required(&mouse.stamp)?)?;
                        RuntimeCommand::Wheel(ApplicationWheelInput {
                            stamp,
                            cell: decode_cell(required(&mouse.cell)?, stamp)?,
                            horizontal_steps: mouse.horizontal_steps,
                            vertical_steps: mouse.vertical_steps,
                            modifiers: modifiers(mouse.modifiers)?,
                        })
                    }
                    wire::runtime_command::Action::Pointer(_) => {
                        return Err(ProtocolError::UnexpectedMessage)
                    }
                };
                Ok(FencedRuntimeCommand { fence, command })
            }
            pub fn encode_command(
                request: u64,
                value: &FencedRuntimeCommand,
            ) -> Result<wire::RuntimeCommand, ProtocolError> {
                if request == 0
                    || (value.fence.content().is_some()
                        && !matches!(
                            value.command,
                            RuntimeCommand::Pointer(_) | RuntimeCommand::Wheel(_)
                        ))
                {
                    return Err(ProtocolError::Malformed);
                }
                let action = match &value.command {
                    RuntimeCommand::Pointer(mouse) => {
                        if mouse.stamp.fence() != value.fence {
                            return Err(ProtocolError::Malformed);
                        }
                        wire::runtime_command::Action::ApplicationPointer(
                            wire::ApplicationPointer {
                                stamp: Some(super::super::content::$p::encode_stamp(mouse.stamp)),
                                cell: Some(encode_cell(mouse.cell)),
                                phase: match mouse.phase {
                                    PointerPhase::Press => 1,
                                    PointerPhase::Motion => 2,
                                    PointerPhase::Release => 3,
                                },
                                button: match mouse.button {
                                    None => 0,
                                    Some(PointerButton::Left) => 1,
                                    Some(PointerButton::Middle) => 2,
                                    Some(PointerButton::Right) => 3,
                                },
                                modifiers: modifier_bits(mouse.modifiers),
                            },
                        )
                    }
                    RuntimeCommand::Wheel(mouse) => {
                        if mouse.stamp.fence() != value.fence {
                            return Err(ProtocolError::Malformed);
                        }
                        wire::runtime_command::Action::ApplicationWheel(wire::ApplicationWheel {
                            stamp: Some(super::super::content::$p::encode_stamp(mouse.stamp)),
                            cell: Some(encode_cell(mouse.cell)),
                            horizontal_steps: mouse.horizontal_steps,
                            vertical_steps: mouse.vertical_steps,
                            modifiers: modifier_bits(mouse.modifiers),
                        })
                    }
                    RuntimeCommand::Key(value) => wire::runtime_command::Action::Key(wire::Key {
                        code: (*value).physical_key.into(),
                        modifiers: modifier_bits(value.modifiers),
                        text: value.text.clone(),
                        action: match value.action {
                            KeyAction::Press => 1,
                            KeyAction::Repeat => 2,
                            KeyAction::Release => 3,
                        },
                        logical_key: value.logical_key.clone(),
                        consumed_modifiers: modifier_bits(value.consumed_modifiers),
                        unshifted_codepoint: value.unshifted_codepoint.map_or(0, u32::from),
                        option_as_alt: value.option_as_alt == OptionAsAltPolicy::Both,
                    }),
                    RuntimeCommand::InputMethodCommit(value) => {
                        wire::runtime_command::Action::InputMethodText(value.as_str().into())
                    }
                    RuntimeCommand::RequestPaste(value) => {
                        wire::runtime_command::Action::Paste(wire::Paste {
                            text: value.as_str().as_bytes().to_vec(),
                            approval: vec![],
                        })
                    }
                    RuntimeCommand::Resize(value) => {
                        wire::runtime_command::Action::MeasuredResize(wire::MeasuredGrid {
                            rows: u32::from(value.rows()),
                            columns: u32::from(value.columns()),
                            cell_width_px: u32::from(value.cell_pixels().width()),
                            cell_height_px: u32::from(value.cell_pixels().height()),
                        })
                    }
                    RuntimeCommand::Appearance(value) => {
                        wire::runtime_command::Action::Appearance(wire::Appearance {
                            generation: value.generation.get(),
                            foreground: rgb_bits(value.foreground),
                            background: rgb_bits(value.background),
                            palette: value.palette.iter().copied().map(rgb_bits).collect(),
                            bold_as_bright: value.bold_as_bright,
                            dark: value.appearance == Appearance::Dark,
                        })
                    }
                    RuntimeCommand::FocusReport(value) => {
                        wire::runtime_command::Action::FocusReport(*value)
                    }
                    RuntimeCommand::CellPixelMetrics(value) => {
                        wire::runtime_command::Action::CellPixelMetrics(wire::CellPixelMetrics {
                            width: u32::from(value.width()),
                            height: u32::from(value.height()),
                        })
                    }
                    RuntimeCommand::ClearScreenAndScrollback => {
                        wire::runtime_command::Action::ClearScreenAndScrollback(
                            wire::ClearScreenAndScrollback {},
                        )
                    }
                    _ => return Err(ProtocolError::UnexpectedMessage),
                };
                let encoded = wire::RuntimeCommand {
                    fence: Some(encode_fence(value.fence)),
                    request,
                    action: Some(action),
                };
                decode_command(&encoded)?;
                Ok(encoded)
            }
        }
    };
}
profile!(v1);
profile!(v2);
