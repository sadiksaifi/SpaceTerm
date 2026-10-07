//! Streaming parser for `git status --porcelain=v2 --branch -z`.
//!
//! The local count streams git's stdout here without a size limit, and the remote count parses a
//! bounded, possibly truncated copy. Only the first [`MAXIMUM_CHANGE_ENTRIES`] paths are decoded,
//! so memory stays bounded by one record however large the repository is.

use std::sync::Arc;

use super::display_text::display_text;
use super::{
    ChangeEntry, ChangeKind, ChangeTotal, Divergence, MAXIMUM_CHANGE_ENTRIES, PorcelainSummary,
    RepositoryReadError,
};

/// The largest single NUL-terminated field accepted. Real records hold one path and fixed fields.
const MAXIMUM_FIELD_BYTES: usize = 64 * 1024;
const INITIAL_OID: &[u8] = b"(initial)";
const DETACHED_HEAD: &[u8] = b"(detached)";

/// Accepts `-z` porcelain v2 output in chunks split at any byte boundary.
///
/// Malformed input is remembered and reported when the parser finishes; later chunks are ignored.
#[derive(Default)]
pub(crate) struct PorcelainParser {
    summary: PorcelainSummary,
    field: Vec<u8>,
    /// A rename or copy record whose original path field has not arrived yet.
    pending_rename: Option<PendingRecord>,
    records: u32,
    saw_oid: bool,
    saw_head: bool,
    failed: bool,
}

/// A counted record that waits for its trailing original path field.
struct PendingRecord {
    status: EntryStatus,
    path: Option<Arc<str>>,
}

#[derive(Clone, Copy)]
struct EntryStatus {
    staged: bool,
    modified: bool,
    deleted: bool,
    renamed: bool,
    conflicted: bool,
    untracked: bool,
}

impl EntryStatus {
    const UNTRACKED: Self = Self {
        staged: false,
        modified: false,
        deleted: false,
        renamed: false,
        conflicted: false,
        untracked: true,
    };

    const CONFLICTED: Self = Self {
        staged: false,
        modified: false,
        deleted: false,
        renamed: false,
        conflicted: true,
        untracked: false,
    };

    fn tracked(index: u8, worktree: u8, renamed: bool) -> Self {
        Self {
            staged: index != b'.',
            modified: matches!(worktree, b'M' | b'T'),
            deleted: index == b'D' || worktree == b'D',
            renamed,
            conflicted: false,
            untracked: false,
        }
    }

    /// The one kind the popover shows. A tracked change with no counted kind, such as an
    /// intent-to-add path, presents as modified.
    fn kind(self) -> ChangeKind {
        if self.conflicted {
            ChangeKind::Conflicted
        } else if self.renamed {
            ChangeKind::Renamed
        } else if self.staged {
            ChangeKind::Staged
        } else if self.deleted {
            ChangeKind::Deleted
        } else if self.untracked {
            ChangeKind::Untracked
        } else {
            ChangeKind::Modified
        }
    }
}

impl PorcelainParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, mut chunk: &[u8]) {
        while !self.failed && !chunk.is_empty() {
            let Some(end) = chunk.iter().position(|byte| *byte == 0) else {
                self.buffer(chunk);
                return;
            };
            if self.field.is_empty() {
                self.complete(&chunk[..end]);
            } else {
                self.buffer(&chunk[..end]);
                let field = std::mem::take(&mut self.field);
                if !self.failed {
                    self.complete(&field);
                }
                self.field = field;
                self.field.clear();
            }
            chunk = &chunk[end + 1..];
        }
    }

    /// Finishes a complete stream. Every record must be NUL-terminated.
    pub(crate) fn finish(self) -> Result<PorcelainSummary, RepositoryReadError> {
        if !self.field.is_empty() || self.pending_rename.is_some() {
            return Err(RepositoryReadError::InvalidResponse);
        }
        let total = ChangeTotal::Exact(self.records);
        self.into_summary(total)
    }

    /// Finishes a stream cut at an output limit: drops the partial trailing record and reports the
    /// counted records as a lower bound.
    pub(crate) fn finish_truncated(self) -> Result<PorcelainSummary, RepositoryReadError> {
        let total = ChangeTotal::AtLeast(self.records);
        self.into_summary(total)
    }

    fn into_summary(mut self, total: ChangeTotal) -> Result<PorcelainSummary, RepositoryReadError> {
        if self.failed || !self.saw_oid || !self.saw_head {
            return Err(RepositoryReadError::InvalidResponse);
        }
        self.summary.changes.total = total;
        Ok(self.summary)
    }

    fn buffer(&mut self, bytes: &[u8]) {
        if self.field.len() + bytes.len() > MAXIMUM_FIELD_BYTES {
            self.failed = true;
        } else {
            self.field.extend_from_slice(bytes);
        }
    }

    fn complete(&mut self, field: &[u8]) {
        if self.record(field).is_err() {
            self.failed = true;
        }
    }

    fn record(&mut self, field: &[u8]) -> Result<(), RepositoryReadError> {
        if let Some(pending) = self.pending_rename.take() {
            if field.is_empty() {
                return Err(RepositoryReadError::InvalidResponse);
            }
            self.count(pending.status, pending.path);
            return Ok(());
        }
        let (&marker, _) = field
            .split_first()
            .ok_or(RepositoryReadError::InvalidResponse)?;
        match marker {
            b'#' => self.header(field),
            b'1' => {
                let [_, status, path] = entry_fields::<9>(field)?;
                let status = tracked_status(status, false)?;
                let path = self.entry_path(path);
                self.count(status, path);
                Ok(())
            }
            b'2' => {
                let [_, status, path] = entry_fields::<10>(field)?;
                let status = tracked_status(status, true)?;
                let path = self.entry_path(path);
                self.pending_rename = Some(PendingRecord { status, path });
                Ok(())
            }
            b'u' => {
                let [_, status, path] = entry_fields::<11>(field)?;
                xy(status)?;
                let path = self.entry_path(path);
                self.count(EntryStatus::CONFLICTED, path);
                Ok(())
            }
            b'?' => {
                let path = single_path(field)?;
                let path = self.entry_path(path);
                self.count(EntryStatus::UNTRACKED, path);
                Ok(())
            }
            b'!' => single_path(field).map(|_| ()),
            _ => Err(RepositoryReadError::InvalidResponse),
        }
    }

    fn header(&mut self, field: &[u8]) -> Result<(), RepositoryReadError> {
        let line = field
            .strip_prefix(b"# ")
            .ok_or(RepositoryReadError::InvalidResponse)?;
        let (name, value) = match line.iter().position(|byte| *byte == b' ') {
            Some(space) => (&line[..space], &line[space + 1..]),
            None => (line, &[][..]),
        };
        let headers = &mut self.summary.headers;
        match name {
            b"branch.oid" => {
                headers.oid = if value == INITIAL_OID {
                    None
                } else {
                    Some(object_id(value)?)
                };
                self.saw_oid = true;
            }
            b"branch.head" => {
                headers.branch = if value == DETACHED_HEAD {
                    None
                } else {
                    Some(required_text(value)?)
                };
                self.saw_head = true;
            }
            b"branch.upstream" => headers.upstream = Some(required_text(value)?),
            b"branch.ab" => headers.divergence = Some(divergence(value)?),
            _ => {}
        }
        Ok(())
    }

    /// Decodes a path only while the retained entry list has room.
    fn entry_path(&self, path: &[u8]) -> Option<Arc<str>> {
        (self.summary.changes.entries.len() < MAXIMUM_CHANGE_ENTRIES).then(|| display_text(path))
    }

    fn count(&mut self, status: EntryStatus, path: Option<Arc<str>>) {
        let changes = &mut self.summary.changes;
        for (counted, counter) in [
            (status.staged, &mut changes.staged),
            (status.modified, &mut changes.modified),
            (status.deleted, &mut changes.deleted),
            (status.renamed, &mut changes.renamed),
            (status.conflicted, &mut changes.conflicted),
            (status.untracked, &mut changes.untracked),
        ] {
            if counted {
                *counter = counter.saturating_add(1);
            }
        }
        if let Some(path) = path
            && changes.entries.len() < MAXIMUM_CHANGE_ENTRIES
        {
            changes.entries.push(ChangeEntry {
                kind: status.kind(),
                path,
            });
        }
        self.records = self.records.saturating_add(1);
    }
}

/// Splits a `1`, `2`, or `u` record into its marker, `XY` status, and trailing path. `N` is the
/// number of space-separated fields including the path, which may itself contain spaces.
fn entry_fields<const N: usize>(field: &[u8]) -> Result<[&[u8]; 3], RepositoryReadError> {
    let mut parts = field.splitn(N, |byte| *byte == b' ');
    let marker = parts.next().ok_or(RepositoryReadError::InvalidResponse)?;
    if marker.len() != 1 {
        return Err(RepositoryReadError::InvalidResponse);
    }
    let status = parts.next().ok_or(RepositoryReadError::InvalidResponse)?;
    for _ in 2..N - 1 {
        if parts.next().is_none_or(|metadata| metadata.is_empty()) {
            return Err(RepositoryReadError::InvalidResponse);
        }
    }
    match parts.next() {
        Some(path) if !path.is_empty() => Ok([marker, status, path]),
        _ => Err(RepositoryReadError::InvalidResponse),
    }
}

fn single_path(field: &[u8]) -> Result<&[u8], RepositoryReadError> {
    match field.get(1..) {
        Some([b' ', path @ ..]) if !path.is_empty() => Ok(path),
        _ => Err(RepositoryReadError::InvalidResponse),
    }
}

fn xy(status: &[u8]) -> Result<(u8, u8), RepositoryReadError> {
    const CODES: &[u8] = b".MTADRCU";
    match *status {
        [index, worktree] if CODES.contains(&index) && CODES.contains(&worktree) => {
            Ok((index, worktree))
        }
        _ => Err(RepositoryReadError::InvalidResponse),
    }
}

fn tracked_status(status: &[u8], renamed: bool) -> Result<EntryStatus, RepositoryReadError> {
    let (index, worktree) = xy(status)?;
    Ok(EntryStatus::tracked(index, worktree, renamed))
}

fn object_id(value: &[u8]) -> Result<Arc<str>, RepositoryReadError> {
    if matches!(value.len(), 40 | 64) && value.iter().all(u8::is_ascii_hexdigit) {
        Ok(String::from_utf8_lossy(value).into())
    } else {
        Err(RepositoryReadError::InvalidResponse)
    }
}

/// A branch or upstream name, kept exact for lookups. Presentation sanitizes it.
fn required_text(value: &[u8]) -> Result<Arc<str>, RepositoryReadError> {
    if value.is_empty() {
        return Err(RepositoryReadError::InvalidResponse);
    }
    Ok(String::from_utf8_lossy(value).into())
}

fn divergence(value: &[u8]) -> Result<Divergence, RepositoryReadError> {
    let mut parts = value.split(|byte| *byte == b' ');
    let ahead = parts
        .next()
        .and_then(|part| part.strip_prefix(b"+"))
        .and_then(count);
    let behind = parts
        .next()
        .and_then(|part| part.strip_prefix(b"-"))
        .and_then(count);
    match (ahead, behind, parts.next()) {
        (Some(ahead), Some(behind), None) => Ok(Divergence { ahead, behind }),
        _ => Err(RepositoryReadError::InvalidResponse),
    }
}

fn count(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Parses one complete stream held in memory.
pub(crate) fn parse_porcelain(output: &[u8]) -> Result<PorcelainSummary, RepositoryReadError> {
    let mut parser = PorcelainParser::new();
    parser.push(output);
    parser.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::{ChangeSummary, StatusHeaders};

    const OID: &str = "1bccb23a55670b76916d70699c1b04ef507bb38d";
    const MODES: &str = "N... 100644 100644 100644";
    const HASHES: &str =
        "61780798228d17af2d34fce4cfbdf35556832472 61780798228d17af2d34fce4cfbdf35556832472";

    fn headers() -> String {
        format!("# branch.oid {OID}\0# branch.head main\0")
    }

    fn ordinary(xy: &str, path: &str) -> String {
        format!("1 {xy} {MODES} {HASHES} {path}\0")
    }

    fn renamed(xy: &str, path: &str, original: &str) -> String {
        format!("2 {xy} {MODES} {HASHES} R100 {path}\0{original}\0")
    }

    fn unmerged(xy: &str, path: &str) -> String {
        format!("u {xy} N... 100644 100644 100644 100644 {HASHES} {OID} {path}\0")
    }

    fn parse(output: &str) -> Result<PorcelainSummary, RepositoryReadError> {
        parse_porcelain(output.as_bytes())
    }

    fn changes(output: &str) -> ChangeSummary {
        parse(&(headers() + output)).unwrap().changes
    }

    fn entry(kind: ChangeKind, path: &str) -> ChangeEntry {
        ChangeEntry {
            kind,
            path: path.into(),
        }
    }

    #[test]
    fn parser_should_read_branch_headers_and_ignore_unknown_header_lines() {
        let summary = parse(&format!(
            "# branch.oid {OID}\0# branch.head feature/a b\0# stash 3\0# future.header x\0\
             # branch.upstream origin/feature\0# branch.ab +2 -15\0"
        ))
        .unwrap();

        assert_eq!(
            summary,
            PorcelainSummary {
                headers: StatusHeaders {
                    oid: Some(OID.into()),
                    branch: Some("feature/a b".into()),
                    upstream: Some("origin/feature".into()),
                    divergence: Some(Divergence {
                        ahead: 2,
                        behind: 15
                    }),
                },
                changes: ChangeSummary::default(),
            }
        );
    }

    #[test]
    fn parser_should_report_initial_oid_and_detached_head_as_absent() {
        let unborn = parse("# branch.oid (initial)\0# branch.head main\0").unwrap();
        let detached = parse(&format!("# branch.oid {OID}\0# branch.head (detached)\0")).unwrap();

        assert_eq!(
            (unborn.headers.oid, unborn.headers.branch.as_deref()),
            (None, Some("main"))
        );
        assert_eq!(
            (detached.headers.oid.as_deref(), detached.headers.branch),
            (Some(OID), None)
        );
        assert_eq!(detached.headers.upstream, None);
        assert_eq!(detached.headers.divergence, None);
    }

    #[test]
    fn parser_should_accept_sha256_object_ids() {
        let oid = "a".repeat(64);
        let summary = parse(&format!("# branch.oid {oid}\0# branch.head main\0")).unwrap();

        assert_eq!(summary.headers.oid.as_deref(), Some(oid.as_str()));
    }

    #[test]
    fn parser_should_keep_an_upstream_without_divergence() {
        let summary = parse(&(headers() + "# branch.upstream origin/gone\0")).unwrap();

        assert_eq!(summary.headers.upstream.as_deref(), Some("origin/gone"));
        assert_eq!(summary.headers.divergence, None);
    }

    #[test]
    fn ordinary_records_should_count_staged_modified_deleted_and_type_changes() {
        let summary = changes(
            &[
                ordinary("M.", "staged.rs"),
                ordinary(".M", "modified.rs"),
                ordinary("MM", "both.rs"),
                ordinary(".T", "type.rs"),
                ordinary("D.", "staged-delete.rs"),
                ordinary(".D", "deleted.rs"),
                ordinary("A.", "added.rs"),
            ]
            .concat(),
        );

        assert_eq!(
            summary,
            ChangeSummary {
                total: ChangeTotal::Exact(7),
                staged: 4,
                modified: 3,
                deleted: 2,
                renamed: 0,
                conflicted: 0,
                untracked: 0,
                entries: vec![
                    entry(ChangeKind::Staged, "staged.rs"),
                    entry(ChangeKind::Modified, "modified.rs"),
                    entry(ChangeKind::Staged, "both.rs"),
                    entry(ChangeKind::Modified, "type.rs"),
                    entry(ChangeKind::Staged, "staged-delete.rs"),
                    entry(ChangeKind::Deleted, "deleted.rs"),
                    entry(ChangeKind::Staged, "added.rs"),
                ],
            }
        );
    }

    #[test]
    fn intent_to_add_records_should_count_once_and_present_as_modified() {
        let summary = changes(&ordinary(".A", "planned.rs"));

        assert_eq!(summary.total, ChangeTotal::Exact(1));
        assert_eq!(
            (summary.staged, summary.modified, summary.deleted),
            (0, 0, 0)
        );
        assert_eq!(summary.entries, [entry(ChangeKind::Modified, "planned.rs")]);
    }

    #[test]
    fn rename_records_should_consume_the_original_path_and_keep_the_new_path() {
        let summary = changes(
            &[
                renamed("R.", "new name.rs", "old name.rs"),
                renamed("RM", "edited.rs", "before.rs"),
                renamed("C.", "copy.rs", "source.rs"),
                ordinary(".M", "after.rs"),
            ]
            .concat(),
        );

        assert_eq!(summary.total, ChangeTotal::Exact(4));
        assert_eq!(
            (summary.renamed, summary.staged, summary.modified),
            (3, 3, 2)
        );
        assert_eq!(
            summary.entries,
            [
                entry(ChangeKind::Renamed, "new name.rs"),
                entry(ChangeKind::Renamed, "edited.rs"),
                entry(ChangeKind::Renamed, "copy.rs"),
                entry(ChangeKind::Modified, "after.rs"),
            ]
        );
    }

    #[test]
    fn unmerged_records_should_count_every_conflict_code_only_as_conflicted() {
        let codes = ["DD", "AU", "UD", "UA", "DU", "AA", "UU"];
        let output: String = codes
            .iter()
            .map(|code| unmerged(code, &format!("{code}.rs")))
            .collect();

        let summary = changes(&output);

        assert_eq!(summary.total, ChangeTotal::Exact(7));
        assert_eq!(summary.conflicted, 7);
        assert_eq!(
            (
                summary.staged,
                summary.modified,
                summary.deleted,
                summary.renamed
            ),
            (0, 0, 0, 0)
        );
        assert!(
            summary
                .entries
                .iter()
                .all(|entry| entry.kind == ChangeKind::Conflicted)
        );
    }

    #[test]
    fn untracked_records_should_count_and_ignored_records_should_be_skipped() {
        let summary = changes("? new file.rs\0! target/\0? other.rs\0! .DS_Store\0");

        assert_eq!(summary.total, ChangeTotal::Exact(2));
        assert_eq!(summary.untracked, 2);
        assert_eq!(
            summary.entries,
            [
                entry(ChangeKind::Untracked, "new file.rs"),
                entry(ChangeKind::Untracked, "other.rs"),
            ]
        );
    }

    #[test]
    fn paths_should_be_decoded_lossily_and_sanitized_for_display() {
        let mut output = headers().into_bytes();
        output.extend_from_slice(b"? bad\xff.rs\0? evil\x1b[31m\xe2\x80\xaetxt.exe\0");

        let summary = parse_porcelain(&output).unwrap();

        assert_eq!(
            summary.changes.entries,
            [
                entry(ChangeKind::Untracked, "bad\u{fffd}.rs"),
                entry(ChangeKind::Untracked, "evil\u{fffd}[31m\u{fffd}txt.exe"),
            ]
        );
    }

    #[test]
    fn entries_should_stop_at_the_cap_while_the_total_stays_exact() {
        let output: String = (0..45)
            .map(|index| format!("? file-{index}.rs\0"))
            .collect();

        let summary = changes(&output);

        assert_eq!(summary.total, ChangeTotal::Exact(45));
        assert_eq!(summary.untracked, 45);
        assert_eq!(summary.entries.len(), MAXIMUM_CHANGE_ENTRIES);
        assert_eq!(&*summary.entries[19].path, "file-19.rs");
    }

    #[test]
    fn chunked_input_should_match_whole_input_at_every_split_point() {
        let output = headers()
            + "# branch.upstream origin/main\0# branch.ab +1 -0\0"
            + &ordinary("MM", "a b.rs")
            + &renamed("R.", "new.rs", "old.rs")
            + &unmerged("UU", "conflict.rs")
            + "? untracked.rs\0! ignored\0";
        let expected = parse(&output).unwrap();
        let bytes = output.as_bytes();

        for split in 0..=bytes.len() {
            let mut parser = PorcelainParser::new();
            parser.push(&bytes[..split]);
            parser.push(&bytes[split..]);
            assert_eq!(parser.finish().unwrap(), expected, "split at {split}");
        }
        let mut parser = PorcelainParser::new();
        for byte in bytes {
            parser.push(std::slice::from_ref(byte));
        }
        assert_eq!(parser.finish().unwrap(), expected);
    }

    #[test]
    fn truncated_finish_should_drop_a_partial_record_and_report_a_lower_bound() {
        let output = headers() + "? one.rs\0? two.rs\0? thr";
        let mut parser = PorcelainParser::new();
        parser.push(output.as_bytes());

        let summary = parser.finish_truncated().unwrap();

        assert_eq!(summary.changes.total, ChangeTotal::AtLeast(2));
        assert_eq!(summary.changes.untracked, 2);
        assert_eq!(summary.changes.entries.len(), 2);
    }

    #[test]
    fn truncated_finish_should_drop_a_rename_whose_original_path_is_missing() {
        let output = headers() + "? one.rs\0" + &renamed("R.", "new.rs", "old.rs");
        let cut = output.len() - "old.rs\0".len();
        for end in [cut, cut + 3] {
            let mut parser = PorcelainParser::new();
            parser.push(&output.as_bytes()[..end]);

            let summary = parser.finish_truncated().unwrap();

            assert_eq!(summary.changes.total, ChangeTotal::AtLeast(1));
            assert_eq!(summary.changes.renamed, 0);
            assert_eq!(summary.changes.entries.len(), 1);
        }
    }

    #[test]
    fn truncated_finish_should_report_a_lower_bound_even_at_a_record_boundary() {
        let mut parser = PorcelainParser::new();
        parser.push((headers() + "? one.rs\0").as_bytes());

        assert_eq!(
            parser.finish_truncated().unwrap().changes.total,
            ChangeTotal::AtLeast(1)
        );
    }

    #[test]
    fn complete_finish_should_reject_an_unterminated_record_or_missing_original_path() {
        assert_eq!(
            parse(&(headers() + "? one.rs")),
            Err(RepositoryReadError::InvalidResponse)
        );
        let rename = renamed("R.", "new.rs", "old.rs");
        assert_eq!(
            parse(&(headers() + &rename[..rename.len() - "old.rs\0".len()])),
            Err(RepositoryReadError::InvalidResponse)
        );
    }

    #[test]
    fn malformed_input_should_be_an_invalid_response() {
        let oid = format!("# branch.oid {OID}\0");
        let head = "# branch.head main\0";
        for output in [
            String::new(),
            oid.clone(),
            head.to_owned(),
            headers() + "\0",
            headers() + "x what\0",
            headers() + "#branch.oid\0",
            headers() + "? \0",
            headers() + "?\0",
            headers() + "?path\0",
            headers() + "1 M. short\0",
            headers() + &ordinary("XY", "bad-code.rs"),
            headers() + &ordinary("M", "short-code.rs"),
            headers() + &format!("1 M. {MODES} {HASHES} \0"),
            headers() + &format!("1 M.  {MODES} {HASHES} path\0"),
            headers() + &format!("2 R. {MODES} {HASHES} R100 new.rs\0\0"),
            headers() + "u UU N... 100644 path\0",
            headers() + "12 M. x\0",
            "# branch.oid nothex\0".to_owned() + head,
            "# branch.oid abc\0".to_owned() + head,
            "# branch.oid\0".to_owned() + head,
            oid.clone() + "# branch.head\0",
            oid.clone() + "# branch.head \0",
            headers() + "# branch.upstream\0",
            headers() + "# branch.ab +1\0",
            headers() + "# branch.ab 1 2\0",
            headers() + "# branch.ab +1 -x\0",
            headers() + "# branch.ab +1 -2 3\0",
            headers() + "# branch.ab +99999999999 -0\0",
        ] {
            assert_eq!(
                parse(&output),
                Err(RepositoryReadError::InvalidResponse),
                "{output:?}"
            );
        }
    }

    #[test]
    fn malformed_input_should_stay_invalid_when_later_input_is_well_formed() {
        let mut parser = PorcelainParser::new();
        parser.push(b"garbage\0");
        parser.push(headers().as_bytes());

        assert_eq!(parser.finish(), Err(RepositoryReadError::InvalidResponse));
    }

    #[test]
    fn an_oversized_field_should_be_rejected_without_unbounded_buffering() {
        let mut parser = PorcelainParser::new();
        parser.push(headers().as_bytes());
        parser.push(b"? ");
        let chunk = vec![b'a'; 16 * 1024];
        for _ in 0..8 {
            parser.push(&chunk);
        }

        assert!(parser.field.len() <= MAXIMUM_FIELD_BYTES);
        assert_eq!(
            parser.finish_truncated(),
            Err(RepositoryReadError::InvalidResponse)
        );
    }

    #[test]
    fn arbitrary_bytes_should_never_panic() {
        let mut seed = 0x2545_f491_u32;
        for _ in 0..512 {
            let mut parser = PorcelainParser::new();
            let mut bytes = headers().into_bytes();
            for _ in 0..96 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                bytes.push(b"12u?!# .MRDU\0\xffab"[seed as usize % 16]);
            }
            parser.push(&bytes);
            let _ = parser.finish();
        }
    }
}
