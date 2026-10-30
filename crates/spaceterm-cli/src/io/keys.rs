//! `pane send` keys: `--key` names, typed text, and the batches they travel in.
//!
//! A batch holds at most [`MAX_KEY_BATCH`] events and [`MAX_KEY_TEXT_BYTES`] bytes of text, the
//! bounds a server accepts in one Input message.
use crate::contract::{Code, Details, Failure, UsageDetails, UsageKind};
use spaceterm_server::pane::keys::{named, text};
use spaceterm_server::pane::{Key, MAX_KEY_BATCH, MAX_KEY_TEXT_BYTES};

/// The keys `--key` names, in order. An unknown name is `invalid_arguments` for `--key`.
pub fn parse(names: &[String]) -> Result<Vec<Key>, Failure> {
    let mut keys = Vec::new();
    for name in names {
        keys.extend(named(name).map_err(|_| {
            Failure::with(
                Code::InvalidArguments,
                Details::Usage(UsageDetails {
                    argument: Some("--key".into()),
                    kind: UsageKind::InvalidValue,
                }),
            )
        })?);
    }
    Ok(keys)
}

/// Text as typed keys of at most [`MAX_KEY_TEXT_BYTES`] each, split at character boundaries.
pub fn typed(value: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let mut end = rest.len().min(MAX_KEY_TEXT_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        keys.push(text(piece));
        rest = tail;
    }
    keys
}

/// Splits keys into batches within the server's bounds, in order.
pub fn batches(keys: Vec<Key>) -> Vec<Vec<Key>> {
    let mut batches: Vec<Vec<Key>> = Vec::new();
    let mut bytes = 0;
    for key in keys {
        let size = key.text.as_ref().map_or(0, String::len);
        let full = batches
            .last()
            .is_none_or(|batch| batch.len() >= MAX_KEY_BATCH || bytes + size > MAX_KEY_TEXT_BYTES);
        if full {
            batches.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        if let Some(batch) = batches.last_mut() {
            batch.push(key);
        }
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_names_name_the_argument_and_never_the_value() {
        let failure = parse(&["Enter".into(), "Hyper-x".into()]).unwrap_err();
        assert_eq!(failure.code, Code::InvalidArguments);
        assert_eq!(
            failure.details,
            Details::Usage(UsageDetails {
                argument: Some("--key".into()),
                kind: UsageKind::InvalidValue,
            })
        );
        assert_eq!(parse(&["C-c".into(), "M-x".into()]).unwrap().len(), 3);
    }

    #[test]
    fn batches_stay_within_the_servers_bounds_and_keep_order() {
        let mut keys = typed(&"é".repeat(5000));
        keys.extend(parse(&vec!["Enter".to_owned(); 40]).unwrap());
        let total = keys.len();
        let batches = batches(keys);
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), total);
        for batch in &batches {
            assert!(batch.len() <= MAX_KEY_BATCH);
            let bytes: usize = batch
                .iter()
                .map(|key| key.text.as_ref().map_or(0, String::len))
                .sum();
            assert!(bytes <= MAX_KEY_TEXT_BYTES);
        }
        let text: String = batches
            .iter()
            .flatten()
            .filter_map(|key| key.text.clone())
            .collect();
        assert_eq!(text, "é".repeat(5000));
    }
}
