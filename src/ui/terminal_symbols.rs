use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TerminalSymbol {
    BoxDrawing(char),
    BlockElement(char),
    Braille(u8),
    Powerline(char),
    LegacySextant(u8),
}

pub(super) fn terminal_symbol(text: &str) -> Option<TerminalSymbol> {
    let mut characters = text.chars();
    let character = characters.next()?;
    if characters.next().is_some() {
        return None;
    }

    match character as u32 {
        0x2500..=0x257f => Some(TerminalSymbol::BoxDrawing(character)),
        0x2580..=0x259f => Some(TerminalSymbol::BlockElement(character)),
        0x2800..=0x28ff => Some(TerminalSymbol::Braille(
            u8::try_from(character as u32 - 0x2800).ok()?,
        )),
        0xe0b0..=0xe0bf => Some(TerminalSymbol::Powerline(character)),
        0x1fb00..=0x1fb3b => Some(TerminalSymbol::LegacySextant(
            u8::try_from(character as u32 - 0x1fb00).ok()?,
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DevicePoint {
    pub(super) x: f32,
    pub(super) y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum SymbolPrimitive {
    Rect {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        alpha: u8,
    },
    Polygon {
        points: Vec<DevicePoint>,
        alpha: u8,
    },
    Stroke {
        points: Vec<DevicePoint>,
        thickness: u16,
        alpha: u8,
    },
}

#[cfg(test)]
impl SymbolPrimitive {
    fn bounds(&self) -> (f32, f32, f32, f32) {
        match self {
            Self::Rect {
                x,
                y,
                width,
                height,
                ..
            } => (
                f32::from(*x),
                f32::from(*y),
                f32::from(*x) + f32::from(*width),
                f32::from(*y) + f32::from(*height),
            ),
            Self::Polygon { points, .. } | Self::Stroke { points, .. } => points.iter().fold(
                (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                |(left, top, right, bottom), point| {
                    (
                        left.min(point.x),
                        top.min(point.y),
                        right.max(point.x),
                        bottom.max(point.y),
                    )
                },
            ),
        }
    }

    fn is_cell_local(&self) -> bool {
        let (left, top, right, bottom) = self.bounds();
        left >= 0.0 && top >= 0.0 && right.is_finite() && bottom.is_finite()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SymbolPlan {
    pub(super) width_device: u16,
    pub(super) height_device: u16,
    pub(super) scale_factor: f32,
    pub(super) primitives: Vec<SymbolPrimitive>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SymbolPlanKey {
    symbol: TerminalSymbol,
    width_device: u16,
    height_device: u16,
    scale_bits: u32,
}

#[derive(Default)]
pub(super) struct SymbolPlanCache {
    plans: HashMap<SymbolPlanKey, Arc<SymbolPlan>>,
}

impl SymbolPlanCache {
    pub(super) fn get(
        &mut self,
        symbol: TerminalSymbol,
        cell_width: f32,
        line_height: f32,
        width_cells: u8,
        scale_factor: f32,
    ) -> Arc<SymbolPlan> {
        let scale_factor = valid_scale_factor(scale_factor);
        let width_device =
            scaled_dimension(cell_width * f32::from(width_cells.max(1)), scale_factor);
        let height_device = scaled_dimension(line_height, scale_factor);
        let key = SymbolPlanKey {
            symbol,
            width_device,
            height_device,
            scale_bits: scale_factor.to_bits(),
        };
        Arc::clone(self.plans.entry(key).or_insert_with(|| {
            Arc::new(build_symbol_plan_for_device(
                symbol,
                width_device,
                height_device,
                scale_factor,
            ))
        }))
    }

    pub(super) fn invalidate_scale_dependent(&mut self) {
        self.plans.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.plans.len()
    }
}

#[cfg(test)]
impl SymbolPlan {
    fn touches_left_and_right_edges(&self) -> bool {
        let right = f32::from(self.width_device);
        self.primitives
            .iter()
            .any(|primitive| primitive.bounds().0 == 0.0)
            && self
                .primitives
                .iter()
                .any(|primitive| primitive.bounds().2 == right)
    }

    fn covers_cell(&self) -> bool {
        self.primitives.iter().any(|primitive| {
            matches!(
                primitive,
                SymbolPrimitive::Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                    alpha: u8::MAX,
                } if *width == self.width_device && *height == self.height_device
            )
        })
    }
}

#[cfg(test)]
fn build_symbol_plan(
    symbol: TerminalSymbol,
    width: u16,
    height: u16,
    scale_factor: f32,
) -> SymbolPlan {
    let scale_factor = valid_scale_factor(scale_factor);
    let width_device = scaled_dimension(f32::from(width), scale_factor);
    let height_device = scaled_dimension(f32::from(height), scale_factor);
    build_symbol_plan_for_device(symbol, width_device, height_device, scale_factor)
}

fn build_symbol_plan_for_device(
    symbol: TerminalSymbol,
    width_device: u16,
    height_device: u16,
    scale_factor: f32,
) -> SymbolPlan {
    let mut plan = SymbolPlan {
        width_device,
        height_device,
        scale_factor,
        primitives: Vec::new(),
    };

    match symbol {
        TerminalSymbol::BoxDrawing(character) => add_box_drawing(&mut plan, character),
        TerminalSymbol::BlockElement(character) => add_block_element(&mut plan, character),
        TerminalSymbol::Braille(pattern) => add_braille(&mut plan, pattern),
        TerminalSymbol::Powerline(character) => add_powerline(&mut plan, character),
        TerminalSymbol::LegacySextant(index) => add_legacy_sextant(&mut plan, index),
    }
    plan
}

fn valid_scale_factor(scale_factor: f32) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn scaled_dimension(logical: f32, scale_factor: f32) -> u16 {
    if !logical.is_finite() || logical <= 0.0 {
        return 1;
    }
    (logical * scale_factor)
        .round()
        .clamp(1.0, f32::from(u16::MAX)) as u16
}

fn rect(plan: &mut SymbolPlan, x: u16, y: u16, width: u16, height: u16, alpha: u8) {
    if width == 0 || height == 0 {
        return;
    }
    plan.primitives.push(SymbolPrimitive::Rect {
        x,
        y,
        width: width.min(plan.width_device.saturating_sub(x)),
        height: height.min(plan.height_device.saturating_sub(y)),
        alpha,
    });
}

fn add_block_element(plan: &mut SymbolPlan, character: char) {
    let width = plan.width_device;
    let height = plan.height_device;
    let fraction = |total: u16, numerator: u16, denominator: u16| {
        ((u32::from(total) * u32::from(numerator) + u32::from(denominator) / 2)
            / u32::from(denominator)) as u16
    };
    match character as u32 {
        0x2580 => rect(plan, 0, 0, width, fraction(height, 1, 2), u8::MAX),
        code @ 0x2581..=0x2587 => {
            let amount = u16::try_from(code - 0x2580).unwrap_or(1);
            let filled = fraction(height, amount, 8);
            rect(plan, 0, height - filled, width, filled, u8::MAX);
        }
        0x2588 => rect(plan, 0, 0, width, height, u8::MAX),
        code @ 0x2589..=0x258f => {
            let amount = u16::try_from(0x2590 - code).unwrap_or(1);
            rect(plan, 0, 0, fraction(width, amount, 8), height, u8::MAX);
        }
        0x2590 => {
            let filled = fraction(width, 1, 2);
            rect(plan, width - filled, 0, filled, height, u8::MAX);
        }
        0x2591 => rect(plan, 0, 0, width, height, 64),
        0x2592 => rect(plan, 0, 0, width, height, 128),
        0x2593 => rect(plan, 0, 0, width, height, 192),
        0x2594 => rect(plan, 0, 0, width, fraction(height, 1, 8), u8::MAX),
        0x2595 => {
            let filled = fraction(width, 1, 8);
            rect(plan, width - filled, 0, filled, height, u8::MAX);
        }
        code @ 0x2596..=0x259f => {
            let masks = [
                0b0100, 0b1000, 0b0001, 0b1101, 0b1001, 0b0111, 0b1011, 0b0010, 0b0110, 0b1110,
            ];
            let index = usize::try_from(code - 0x2596).unwrap_or(0);
            add_quadrants(plan, masks[index]);
        }
        _ => {}
    }
}

fn add_quadrants(plan: &mut SymbolPlan, mask: u8) {
    let middle_x = plan.width_device / 2;
    let middle_y = plan.height_device / 2;
    let regions = [
        (0, 0, middle_x, middle_y),
        (middle_x, 0, plan.width_device - middle_x, middle_y),
        (0, middle_y, middle_x, plan.height_device - middle_y),
        (
            middle_x,
            middle_y,
            plan.width_device - middle_x,
            plan.height_device - middle_y,
        ),
    ];
    for (bit, (x, y, width, height)) in regions.into_iter().enumerate() {
        if mask & (1 << bit) != 0 {
            rect(plan, x, y, width, height, u8::MAX);
        }
    }
}

fn add_braille(plan: &mut SymbolPlan, pattern: u8) {
    let dot = (plan.width_device / 4).min(plan.height_device / 8).max(1);
    let x_centers = [plan.width_device / 4, plan.width_device * 3 / 4];
    let y_centers = [
        plan.height_device / 8,
        plan.height_device * 3 / 8,
        plan.height_device * 5 / 8,
        plan.height_device * 7 / 8,
    ];
    let locations = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for (bit, (column, row)) in locations.into_iter().enumerate() {
        if pattern & (1 << bit) != 0 {
            rect(
                plan,
                x_centers[column].saturating_sub(dot / 2),
                y_centers[row].saturating_sub(dot / 2),
                dot,
                dot,
                u8::MAX,
            );
        }
    }
}

fn add_legacy_sextant(plan: &mut SymbolPlan, index: u8) {
    let pattern = index + index / 0x14 + 1;
    let middle_x = plan.width_device / 2;
    let first_y = plan.height_device / 3;
    let second_y = plan.height_device * 2 / 3;
    let regions = [
        (0, 0, middle_x, first_y),
        (middle_x, 0, plan.width_device - middle_x, first_y),
        (0, first_y, middle_x, second_y - first_y),
        (
            middle_x,
            first_y,
            plan.width_device - middle_x,
            second_y - first_y,
        ),
        (0, second_y, middle_x, plan.height_device - second_y),
        (
            middle_x,
            second_y,
            plan.width_device - middle_x,
            plan.height_device - second_y,
        ),
    ];
    for (bit, (x, y, width, height)) in regions.into_iter().enumerate() {
        if pattern & (1 << bit) != 0 {
            rect(plan, x, y, width, height, u8::MAX);
        }
    }
}

fn add_powerline(plan: &mut SymbolPlan, character: char) {
    let width = f32::from(plan.width_device);
    let height = f32::from(plan.height_device);
    let point = |x, y| DevicePoint { x, y };
    let polygon = |plan: &mut SymbolPlan, points| {
        plan.primitives.push(SymbolPrimitive::Polygon {
            points,
            alpha: u8::MAX,
        });
    };
    let stroke = |plan: &mut SymbolPlan, points| {
        plan.primitives.push(SymbolPrimitive::Stroke {
            points,
            thickness: (plan.scale_factor.round() as u16).max(1),
            alpha: u8::MAX,
        });
    };
    match character as u32 {
        0xe0b0 => polygon(
            plan,
            vec![
                point(0.0, 0.0),
                point(width, height / 2.0),
                point(0.0, height),
            ],
        ),
        0xe0b1 => stroke(
            plan,
            vec![
                point(0.0, 0.0),
                point(width, height / 2.0),
                point(0.0, height),
            ],
        ),
        0xe0b2 => polygon(
            plan,
            vec![
                point(width, 0.0),
                point(0.0, height / 2.0),
                point(width, height),
            ],
        ),
        0xe0b3 => stroke(
            plan,
            vec![
                point(width, 0.0),
                point(0.0, height / 2.0),
                point(width, height),
            ],
        ),
        0xe0b4 | 0xe0b6 => polygon(
            plan,
            vec![
                point(0.0, 0.0),
                point(width, 0.0),
                point(width, height),
                point(0.0, height),
            ],
        ),
        0xe0b5 | 0xe0b7 => stroke(
            plan,
            vec![
                point(0.0, 0.0),
                point(width, height / 2.0),
                point(0.0, height),
            ],
        ),
        0xe0b8 => polygon(
            plan,
            vec![point(0.0, 0.0), point(width, height), point(0.0, height)],
        ),
        0xe0ba => polygon(
            plan,
            vec![point(width, 0.0), point(width, height), point(0.0, height)],
        ),
        0xe0bc => polygon(
            plan,
            vec![point(0.0, 0.0), point(width, 0.0), point(0.0, height)],
        ),
        0xe0be => polygon(
            plan,
            vec![point(0.0, 0.0), point(width, 0.0), point(width, height)],
        ),
        0xe0b9 | 0xe0bf => stroke(plan, vec![point(0.0, 0.0), point(width, height)]),
        0xe0bb | 0xe0bd => stroke(plan, vec![point(width, 0.0), point(0.0, height)]),
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum LineStyle {
    None,
    Light,
    Heavy,
    Double,
}

fn add_box_drawing(plan: &mut SymbolPlan, character: char) {
    let codepoint = character as u32;
    let light = (plan.scale_factor.round() as u16).max(1);
    match codepoint {
        0x2504 | 0x2505 | 0x2508 | 0x2509 | 0x254c | 0x254d => {
            let count = if matches!(codepoint, 0x2504 | 0x2505) {
                3
            } else if matches!(codepoint, 0x2508 | 0x2509) {
                4
            } else {
                2
            };
            let thickness = if matches!(codepoint, 0x2505 | 0x2509 | 0x254d) {
                light * 2
            } else {
                light
            };
            add_horizontal_dashes(plan, count, thickness);
        }
        0x2506 | 0x2507 | 0x250a | 0x250b | 0x254e | 0x254f => {
            let count = if matches!(codepoint, 0x2506 | 0x2507) {
                3
            } else if matches!(codepoint, 0x250a | 0x250b) {
                4
            } else {
                2
            };
            let thickness = if matches!(codepoint, 0x2507 | 0x250b | 0x254f) {
                light * 2
            } else {
                light
            };
            add_vertical_dashes(plan, count, thickness);
        }
        0x2571 => add_diagonal(plan, true, false, light),
        0x2572 => add_diagonal(plan, false, true, light),
        0x2573 => add_diagonal(plan, true, true, light),
        _ => {
            let encoded = box_line_encoding(codepoint).or(match codepoint {
                0x256d => Some(0x14),
                0x256e => Some(0x50),
                0x256f => Some(0x41),
                0x2570 => Some(0x05),
                _ => None,
            });
            if let Some(encoded) = encoded {
                for direction in 0..4 {
                    let style = match (encoded >> (direction * 2)) & 0x03 {
                        1 => LineStyle::Light,
                        2 => LineStyle::Heavy,
                        3 => LineStyle::Double,
                        _ => LineStyle::None,
                    };
                    add_line_direction(plan, direction, style, light);
                }
            }
        }
    }
}

fn add_line_direction(plan: &mut SymbolPlan, direction: u8, style: LineStyle, light: u16) {
    let thicknesses: &[u16] = match style {
        LineStyle::None => return,
        LineStyle::Light => &[light],
        LineStyle::Heavy => &[light * 2],
        LineStyle::Double => &[light, light],
    };
    for (index, thickness) in thicknesses.iter().copied().enumerate() {
        let double_offset = if matches!(style, LineStyle::Double) {
            if index == 0 {
                -(i32::from(light) * 2)
            } else {
                i32::from(light)
            }
        } else {
            0
        };
        let center_x = i32::from(plan.width_device / 2);
        let center_y = i32::from(plan.height_device / 2);
        let half = i32::from(thickness / 2);
        match direction {
            0 => {
                let x = (center_x + double_offset - half).max(0) as u16;
                rect(
                    plan,
                    x,
                    0,
                    thickness,
                    plan.height_device / 2 + light,
                    u8::MAX,
                );
            }
            1 => {
                let y = (center_y + double_offset - half).max(0) as u16;
                let x = (center_x - i32::from(light)).max(0) as u16;
                rect(plan, x, y, plan.width_device - x, thickness, u8::MAX);
            }
            2 => {
                let x = (center_x + double_offset - half).max(0) as u16;
                let y = (center_y - i32::from(light)).max(0) as u16;
                rect(plan, x, y, thickness, plan.height_device - y, u8::MAX);
            }
            3 => {
                let y = (center_y + double_offset - half).max(0) as u16;
                rect(
                    plan,
                    0,
                    y,
                    plan.width_device / 2 + light,
                    thickness,
                    u8::MAX,
                );
            }
            _ => {}
        }
    }
}

fn add_horizontal_dashes(plan: &mut SymbolPlan, count: u16, thickness: u16) {
    let gap = thickness.max(1);
    let available = plan
        .width_device
        .saturating_sub(gap * count.saturating_sub(1));
    let segment = (available / count).max(1);
    let y = plan.height_device.saturating_sub(thickness) / 2;
    for index in 0..count {
        let x = index * (segment + gap);
        let width = if index + 1 == count {
            plan.width_device - x
        } else {
            segment
        };
        rect(plan, x, y, width, thickness, u8::MAX);
    }
}

fn add_vertical_dashes(plan: &mut SymbolPlan, count: u16, thickness: u16) {
    let gap = thickness.max(1);
    let available = plan
        .height_device
        .saturating_sub(gap * count.saturating_sub(1));
    let segment = (available / count).max(1);
    let x = plan.width_device.saturating_sub(thickness) / 2;
    for index in 0..count {
        let y = index * (segment + gap);
        let height = if index + 1 == count {
            plan.height_device - y
        } else {
            segment
        };
        rect(plan, x, y, thickness, height, u8::MAX);
    }
}

fn add_diagonal(plan: &mut SymbolPlan, rising: bool, falling: bool, thickness: u16) {
    let width = f32::from(plan.width_device);
    let height = f32::from(plan.height_device);
    if rising {
        plan.primitives.push(SymbolPrimitive::Stroke {
            points: vec![
                DevicePoint { x: width, y: 0.0 },
                DevicePoint { x: 0.0, y: height },
            ],
            thickness,
            alpha: u8::MAX,
        });
    }
    if falling {
        plan.primitives.push(SymbolPrimitive::Stroke {
            points: vec![
                DevicePoint { x: 0.0, y: 0.0 },
                DevicePoint {
                    x: width,
                    y: height,
                },
            ],
            thickness,
            alpha: u8::MAX,
        });
    }
}

fn box_line_encoding(codepoint: u32) -> Option<u8> {
    const LINES: &[(u32, u8)] = &[
        (0x2500, 0x44),
        (0x2501, 0x88),
        (0x2502, 0x11),
        (0x2503, 0x22),
        (0x250c, 0x14),
        (0x250d, 0x18),
        (0x250e, 0x24),
        (0x250f, 0x28),
        (0x2510, 0x50),
        (0x2511, 0x90),
        (0x2512, 0x60),
        (0x2513, 0xa0),
        (0x2514, 0x05),
        (0x2515, 0x09),
        (0x2516, 0x06),
        (0x2517, 0x0a),
        (0x2518, 0x41),
        (0x2519, 0x81),
        (0x251a, 0x42),
        (0x251b, 0x82),
        (0x251c, 0x15),
        (0x251d, 0x19),
        (0x251e, 0x16),
        (0x251f, 0x25),
        (0x2520, 0x26),
        (0x2521, 0x1a),
        (0x2522, 0x29),
        (0x2523, 0x2a),
        (0x2524, 0x51),
        (0x2525, 0x91),
        (0x2526, 0x52),
        (0x2527, 0x61),
        (0x2528, 0x62),
        (0x2529, 0x92),
        (0x252a, 0xa1),
        (0x252b, 0xa2),
        (0x252c, 0x54),
        (0x252d, 0x94),
        (0x252e, 0x58),
        (0x252f, 0x98),
        (0x2530, 0x64),
        (0x2531, 0xa4),
        (0x2532, 0x68),
        (0x2533, 0xa8),
        (0x2534, 0x45),
        (0x2535, 0x85),
        (0x2536, 0x49),
        (0x2537, 0x89),
        (0x2538, 0x46),
        (0x2539, 0x86),
        (0x253a, 0x4a),
        (0x253b, 0x8a),
        (0x253c, 0x55),
        (0x253d, 0x95),
        (0x253e, 0x59),
        (0x253f, 0x99),
        (0x2540, 0x56),
        (0x2541, 0x65),
        (0x2542, 0x66),
        (0x2543, 0x96),
        (0x2544, 0x5a),
        (0x2545, 0xa5),
        (0x2546, 0x69),
        (0x2547, 0x9a),
        (0x2548, 0xa9),
        (0x2549, 0xa6),
        (0x254a, 0x6a),
        (0x254b, 0xaa),
        (0x2550, 0xcc),
        (0x2551, 0x33),
        (0x2552, 0x1c),
        (0x2553, 0x34),
        (0x2554, 0x3c),
        (0x2555, 0xd0),
        (0x2556, 0x70),
        (0x2557, 0xf0),
        (0x2558, 0x0d),
        (0x2559, 0x07),
        (0x255a, 0x0f),
        (0x255b, 0xc1),
        (0x255c, 0x43),
        (0x255d, 0xc3),
        (0x255e, 0x1d),
        (0x255f, 0x37),
        (0x2560, 0x3f),
        (0x2561, 0xd1),
        (0x2562, 0x73),
        (0x2563, 0xf3),
        (0x2564, 0xdc),
        (0x2565, 0x74),
        (0x2566, 0xfc),
        (0x2567, 0xcd),
        (0x2568, 0x47),
        (0x2569, 0xcf),
        (0x256a, 0xdd),
        (0x256b, 0x77),
        (0x256c, 0xff),
        (0x2574, 0x40),
        (0x2575, 0x01),
        (0x2576, 0x04),
        (0x2577, 0x10),
        (0x2578, 0x80),
        (0x2579, 0x02),
        (0x257a, 0x08),
        (0x257b, 0x20),
        (0x257c, 0x48),
        (0x257d, 0x21),
        (0x257e, 0x84),
        (0x257f, 0x12),
    ];
    LINES
        .iter()
        .find_map(|(candidate, encoded)| (*candidate == codepoint).then_some(*encoded))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn substitution_requires_exactly_one_supported_terminal_symbol() {
        for text in ["─", "█", "⣿", "", "\u{1fb00}"] {
            assert!(terminal_symbol(text).is_some(), "missing {text:?}");
        }

        for text in ["", "a", "─\u{fe0f}", "█\u{301}", "\u{200d}x", "──"] {
            assert!(terminal_symbol(text).is_none(), "substituted {text:?}");
        }
    }

    #[test]
    fn generated_geometry_reaches_cell_edges_at_one_and_two_x_scale() {
        for scale in [1.0, 2.0] {
            let horizontal = build_symbol_plan(terminal_symbol("─").unwrap(), 9, 20, scale);
            let full_block = build_symbol_plan(terminal_symbol("█").unwrap(), 9, 20, scale);
            let powerline = build_symbol_plan(terminal_symbol("").unwrap(), 9, 20, scale);

            assert!(horizontal.touches_left_and_right_edges());
            assert!(full_block.covers_cell());
            assert!(powerline.touches_left_and_right_edges());
        }
    }

    #[test]
    fn braille_and_legacy_plans_map_bits_to_cell_local_regions() {
        let braille = build_symbol_plan(terminal_symbol("⣿").unwrap(), 10, 20, 2.0);
        let sextant = build_symbol_plan(terminal_symbol("\u{1fb00}").unwrap(), 10, 20, 2.0);

        assert_eq!(braille.primitives.len(), 8);
        assert_eq!(sextant.primitives.len(), 1);
        assert!(
            braille
                .primitives
                .iter()
                .all(SymbolPrimitive::is_cell_local)
        );
        assert!(
            sextant
                .primitives
                .iter()
                .all(SymbolPrimitive::is_cell_local)
        );
    }

    #[test]
    fn owner_cache_reuses_exact_geometry_and_releases_it_on_scale_invalidation() {
        let symbol = terminal_symbol("").unwrap();
        let mut cache = SymbolPlanCache::default();

        let first = cache.get(symbol, 9.0, 20.0, 1, 2.0);
        let same = cache.get(symbol, 9.0, 20.0, 1, 2.0);
        let wide = cache.get(symbol, 9.0, 20.0, 2, 2.0);

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &wide));
        assert_eq!(wide.width_device, first.width_device * 2);
        assert_eq!(cache.len(), 2);

        cache.invalidate_scale_dependent();
        assert_eq!(cache.len(), 0);
        assert_eq!(Arc::strong_count(&first), 2);
        drop(same);
        assert_eq!(Arc::strong_count(&first), 1);
    }
}
