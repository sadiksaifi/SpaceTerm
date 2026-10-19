//! Atomic row replacement against one exact presentation baseline.
use super::{Cell, MAX_COLUMNS, MAX_ROWS, Reply, Screen};
use crate::ServerError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RowChanges {
    pub base: u64,
    pub row_count: u16,
    pub indices: Vec<u16>,
    /// Complete current metadata, with only the replacement rows in `rows`.
    pub screen: Screen,
}

/// Retains only the last emitted screen. Unknown bases receive a complete reset.
#[derive(Default)]
pub struct ScreenEncoder {
    previous: Option<Baseline>,
}
struct Baseline {
    generation: u64,
    columns: u16,
    cell_pixels: Option<(u16, u16)>,
    alternate: bool,
    rows: Vec<Vec<Cell>>,
}
impl ScreenEncoder {
    pub fn baseline(&self) -> Option<u64> {
        self.previous.as_ref().map(|screen| screen.generation)
    }
    pub fn encode(&mut self, mut screen: Screen, base: Option<u64>) -> Reply {
        let compatible = self.previous.as_ref().filter(|previous| {
            Some(previous.generation) == base
                && previous.generation < screen.generation
                && previous.columns == screen.columns
                && previous.rows.len() == screen.rows.len()
                && previous.cell_pixels == screen.cell_pixels
                && previous.alternate == screen.alternate
        });
        let replacement = compatible.and_then(|previous| {
            let indices: Vec<u16> = screen
                .rows
                .iter()
                .zip(&previous.rows)
                .enumerate()
                .filter_map(|(index, (new, old))| (new != old).then_some(index as u16))
                .collect();
            if indices.len() == screen.rows.len() {
                return None;
            }
            let rows = indices
                .iter()
                .map(|index| screen.rows[*index as usize].clone())
                .collect();
            Some((previous.generation, indices, rows))
        });
        let row_count = screen.rows.len() as u16;
        self.previous = Some(Baseline {
            generation: screen.generation,
            columns: screen.columns,
            cell_pixels: screen.cell_pixels,
            alternate: screen.alternate,
            rows: match &replacement {
                Some(_) => std::mem::take(&mut screen.rows),
                None => screen.rows.clone(),
            },
        });
        match replacement {
            Some((base, indices, rows)) => {
                screen.rows = rows;
                Reply::ScreenChanges(Box::new(RowChanges {
                    base,
                    row_count,
                    indices,
                    screen,
                }))
            }
            None => Reply::Screen(Box::new(screen)),
        }
    }
}

/// One decoder belongs to one exact Pane lease and is discarded on reattachment.
#[derive(Default)]
pub struct ScreenDecoder {
    previous: Option<Screen>,
}
impl ScreenDecoder {
    pub fn baseline(&self) -> Option<u64> {
        self.previous.as_ref().map(|screen| screen.generation)
    }
    pub fn apply(&mut self, reply: Reply) -> Result<Reply, ServerError> {
        let screen = match reply {
            Reply::Screen(screen) => screen,
            Reply::ScreenChanges(changes) => {
                let Some(previous) = self
                    .previous
                    .take()
                    .filter(|screen| screen.generation == changes.base)
                else {
                    // A lost local baseline requests a complete reset on the next PollChanges.
                    return Ok(Reply::Unchanged);
                };
                if changes.screen.generation <= changes.base
                    || changes.screen.columns != previous.columns
                    || changes.screen.cell_pixels != previous.cell_pixels
                    || changes.screen.alternate != previous.alternate
                    || changes.row_count as usize != previous.rows.len()
                    || changes.indices.len() != changes.screen.rows.len()
                    || changes.indices.windows(2).any(|pair| pair[0] >= pair[1])
                    || changes
                        .indices
                        .iter()
                        .any(|index| *index >= changes.row_count)
                {
                    return Err(ServerError::Protocol);
                }
                let mut screen = changes.screen;
                let replacements = std::mem::replace(&mut screen.rows, previous.rows);
                for (index, row) in changes.indices.into_iter().zip(replacements) {
                    screen.rows[index as usize] = row;
                }
                Box::new(screen)
            }
            other => return Ok(other),
        };
        if !(2..=MAX_COLUMNS).contains(&screen.columns)
            || !(2..=MAX_ROWS as usize).contains(&screen.rows.len())
            || screen
                .rows
                .iter()
                .any(|row| row.len() != screen.columns as usize)
            || screen.wrapped.len() != screen.rows.len()
        {
            self.previous = None;
            return Err(ServerError::Protocol);
        }
        self.previous = Some((*screen).clone());
        Ok(Reply::Screen(screen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen(generation: u64) -> Screen {
        serde_json::from_value(serde_json::json!({
            "generation": generation, "columns": 2,
            "rows": [[cell("a"),cell("b")],[cell("c"),cell("d")]],
            "wrapped": [false,false], "foreground": [255,255,255],
            "background": [0,0,0], "palette": [], "reversed": false,
            "foreground_overridden": false, "background_overridden": false,
            "cursor_overridden": false, "palette_overrides": [],
            "cursor": [0,0,1], "cursor_visible": true, "cursor_blinking": false,
            "cursor_shape": 0, "cursor_color": [255,255,255],
            "cursor_text": [0,0,0], "alternate": false
        }))
        .unwrap()
    }
    fn cell(text: &str) -> serde_json::Value {
        serde_json::json!({"text":text,"foreground":"Default","background":"Default",
            "underline_color":"Default","flags":0,"underline":0})
    }
    #[test]
    fn one_changed_row_reconstructs_text_styles_and_complete_metadata() {
        let mut encoder = ScreenEncoder::default();
        let mut decoder = ScreenDecoder::default();
        decoder.apply(encoder.encode(screen(1), None)).unwrap();
        let mut next = screen(2);
        next.rows[1][0].text = "界".into();
        next.rows[1][0].flags = 2;
        next.cursor = Some((1, 1, 1));
        next.wrapped[1] = true;
        let update = encoder.encode(next.clone(), decoder.baseline());
        assert!(
            matches!(&update, Reply::ScreenChanges(changes) if changes.indices == [1] && changes.screen.rows.len() == 1)
        );
        let Reply::Screen(actual) = decoder.apply(update).unwrap() else {
            panic!("screen missing")
        };
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(next).unwrap()
        );
    }

    #[test]
    fn cursor_only_update_carries_no_rows() {
        let mut encoder = ScreenEncoder::default();
        encoder.encode(screen(1), None);
        let mut next = screen(2);
        next.cursor = Some((1, 1, 1));
        assert!(
            matches!(encoder.encode(next, Some(1)), Reply::ScreenChanges(changes) if changes.indices.is_empty() && changes.screen.rows.is_empty())
        );
    }

    #[test]
    fn missing_base_recovers_with_a_complete_screen_and_never_publishes_a_partial_update() {
        let mut encoder = ScreenEncoder::default();
        let mut decoder = ScreenDecoder::default();
        encoder.encode(screen(1), None);
        let mut next = screen(2);
        next.rows[0][0].text = "new".into();
        assert!(matches!(
            decoder
                .apply(encoder.encode(next.clone(), Some(1)))
                .unwrap(),
            Reply::Unchanged
        ));
        assert_eq!(decoder.baseline(), None);
        let reset = encoder.encode(next.clone(), decoder.baseline());
        assert!(matches!(&reset, Reply::Screen(_)));
        let Reply::Screen(actual) = decoder.apply(reset).unwrap() else {
            panic!("reset missing")
        };
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(next).unwrap()
        );
    }

    #[test]
    fn resize_mode_and_unknown_base_each_require_a_full_reset() {
        for kind in 0..3 {
            let mut encoder = ScreenEncoder::default();
            encoder.encode(screen(1), None);
            let mut next = screen(2);
            let base = match kind {
                0 => {
                    next.rows.push(next.rows[0].clone());
                    next.wrapped.push(false);
                    Some(1)
                }
                1 => {
                    next.alternate = true;
                    Some(1)
                }
                _ => Some(50),
            };
            assert!(matches!(encoder.encode(next, base), Reply::Screen(_)));
        }
    }

    #[test]
    fn malformed_row_replacement_invalidates_the_baseline_atomically() {
        let mut encoder = ScreenEncoder::default();
        let mut decoder = ScreenDecoder::default();
        decoder.apply(encoder.encode(screen(1), None)).unwrap();
        let mut next = screen(2);
        next.rows[0][0].text = "new".into();
        let Reply::ScreenChanges(mut changes) = encoder.encode(next, Some(1)) else {
            panic!("changes missing")
        };
        changes.indices[0] = 2;
        assert!(matches!(
            decoder.apply(Reply::ScreenChanges(changes)),
            Err(ServerError::Protocol)
        ));
        assert_eq!(decoder.baseline(), None);
    }
}
