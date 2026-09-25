# Terminal Find continuation

Status: production encoder measured and validated, 2026-09-26.
Source base: `3831cd18095a`, with the current uncommitted continuation changes.
Production corpus encoding now uses the measured direct UTF-8 strategy. The coordinator completed the release benchmark, native application captures, and
full validation gate.

## Recommendation

Keep direct UTF-8 encoding in the existing SearchCorpus. It removes
the temporary `String` allocated for each populated terminal cell without
changing the corpus layout, matching algorithm, result ordering, or navigation.
The production implementation encodes each `char` into a four-byte stack buffer and
passes those bytes through the existing `push_grapheme` mapping operation.
Long grapheme clusters remain unbounded and every byte keeps its original head
cell and width. Full-grapheme reservation preserves both vectors' capacity growth.
Production-path validation passes with exact bytes, mappings, and vector capacities.

## New fixture

The coordinator ran `mise run bench:one performance_find_pipeline` successfully. No new mise task is needed. The implementation
is `src/terminal/find/performance.rs`, included through a `cfg(test)` module hook.
Fixture schema version is 3. Final fixture SHA-256:
`14aa66d4492e02747ca20b5d2488f094ca1497be3f0f8c3302acd9f9f168509e`.
Release test binary SHA-256:
`3677578382d00f81fde605cbf78b59062b5b1e0ad1c87cf7c20987c6a57dd6d5`.

The fixture uses 120 columns, 40 visible rows, and 40, 2,000, or 10,000 emitted
lines. It measures dense ASCII, sparse rows with leading unset cells, and Unicode
with wide cells and a cluster longer than the initial eight-codepoint buffer.
Version 2 also includes an explicitly labeled eight-column `narrow_history` case,
using the same native history limits and one six-column match per emitted line.
It reports actual retained rows, corpus length, mapping element size, and vector
capacity bytes. The byte-based native history limit can prune rows before the
configured 10,000-line limit; the fixture never reports requested rows as actual
history. Each retained populated row contains exactly one `needle`, which is
checked before timing.

## First measurement and capacity correction

The coordinator ran version 1 in release mode. Log:
`target/performance/continuation-find-baseline.log`. Median corpus construction
time decreased 36.7-37.8% for ASCII, 5.5-7.2% for sparse rows, and 9.0-11.4% for
Unicode. These are isolated corpus timings, not full application improvements.

Version 1 is not accepted: per-character appends altered Vec growth. Unicode
40-line input increased combined corpus capacity from 52,224 to 69,632 bytes;
2,000-line input increased it from 417,792 to 557,056 bytes. The 10,000-line Unicode
case decreased capacity instead, confirming the change depended on growth
boundaries rather than a consistent storage improvement.

Version 2 computes each full grapheme's UTF-8 byte length and reserves that length
in both vectors before appending individual characters. This preserves the
production whole-grapheme growth decision. The fixture now requires equal
individual vector capacities as well as equal bytes and mappings before timing.
The coordinator then ran version 2 successfully; all corpus bytes, mappings, and
individual vector capacities matched. Log:
`target/performance/continuation-find-capacity.log`. Median corpus construction
improved 41.6-42.8% for ASCII, 1.8-4.9% for sparse rows, and 10.3-13.6% for Unicode.
The narrow case retained 10,001 total rows after 10,000 emitted lines and improved
35.1%, with equal 2,228,224-byte combined capacity. The implementation was moved
into production after this evidence.

Version 3 preserves the original String-per-grapheme traversal as `legacy_baseline`
and compares it against the actual production `SearchCorpus::from_terminal`.
Its metadata labels original capacity as `legacy_capacity_bytes`. Timings from
versions 1 and 2 labeled the old algorithm `production`; do not combine those
labels with version 3 without accounting for the implementation change.

## Actual production measurement

Log: `target/performance/continuation-find-production.log`. All 12 cases pass
exact bytes, mappings, and individual vector capacity parity. Across three
alternating repetitions per case, median corpus construction improves 34.2-37.6%
for ASCII, 6.6-7.7% for sparse rows, 9.5-12.4% for Unicode, and 32.5-34.6%
for narrow history. At 10,001 retained rows, legacy corpus construction takes
2.763 ms and production takes 1.807 ms, with equal 2,228,224-byte capacity.
These are corpus operation timings, not application CPU or Find UI latency.
The new long-grapheme regression and all existing Find tests pass in the full
3,123-test workspace gate. Final native captures pass equal-output and terminal
consumption checks in focused, unfocused-visible, and hidden states.

## Actual history policy

The Rust `TerminalOptions.max_scrollback` field is a row limit: `new_inner` passes
it to `SCROLLBACK_MAX_LINES`. SpaceTerm supplies `MAX_SCROLLBACK_ROWS = 10_000`.
Separately, pinned Ghostty `Terminal.zig` defaults `max_scrollback_bytes` to
10,000 bytes. Its C constructor initializes only columns and rows, leaving that
byte limit in effect. The two limits apply together and pruning occurs by whole
historical pages, with at least one standard page retained. Therefore this is
not a simple 10,000-byte allocation ceiling, nor a guarantee of 10,000 rows.

The first fixture observed 41, 417, and 497 total rows after 40, 2,000, and 10,000
emitted lines respectively at 120 columns. This includes the active screen and
one blank final cursor row. The new narrow case can fit more rows per native
page without changing either history limit; version 2 retained 10,001 total rows.
No history configuration or production policy was changed for this benchmark.

Source locations checked: `third_party/libghostty-vt/src/terminal.rs:284`, pinned
`src/terminal/c/terminal.zig:898`, `src/terminal/Terminal.zig:269`, and
`include/ghostty/vt/terminal.h` options 27 and 28 in the local native source tree.

## Measurement method

Production corpus construction and the test-only legacy traversal run
inside the same optimized process, with order alternating across repetitions.
Exact corpus bytes and every byte-to-cell mapping must agree before measurement.
Separate measurements report literal matching, invalidated complete Find refresh,
and viewport-span snapshot construction. Setup, corpus equality, and terminal
trace ingestion are outside measured intervals. Each timed operation includes
destruction of its result, equally for both corpus strategies.

There is no existing Rust-global allocation counter in the application tests.
The libghostty allocator adapter would count native allocations, not these Rust
temporary Strings. The fixture reports actual capacity and timing, not invented
allocator counts. The expected removed allocation sites follow directly from
the per-populated-cell `collect::<String>()` call. Its test-only copied traversal
provided exploratory evidence. Version 3 now measures the actual production path
against the preserved legacy traversal.

## Remaining costs and order

| Order | Candidate | Current cost | Required evidence |
| --- | --- | --- | --- |
| Complete | Direct corpus UTF-8 encoding | Removes temporary Strings on dirty refresh | Production fixture parity and behavior tests pass |
| 2 | Reuse pending-space storage across rows within one corpus build | New Vec per row, including trailing unset cells ultimately discarded | Separate variant; preserve exact blank positions, wide tails and hard/soft line behavior |
| 3 | Restrict highlight iteration to visible matches | Every published Find snapshot scans all matches, even when only a few are visible | Many-match snapshot timings and exact span/current-index parity |
| 4 | Compact byte mappings | One Option<CellMapping> for each UTF-8 byte | Allocation/retained-memory evidence; preserve matches beginning or ending inside one grapheme |
| 5 | Native incremental search | Full-history traversal on every dirty refresh | Full differential trace suite and a parity adapter, not a direct replacement |

Reusing the pending-space Vec outside the row loop can retain its capacity only
for the lifetime of one rebuild. Clear it at each row boundary so trailing blanks
still disappear. A numeric blank range needs a proof that skipped wide tails
cannot create holes; Vec reuse avoids adding that assumption.

Current matches are appended in byte order, and corpus mappings follow monotonic
screen coordinates. Therefore both match starts and ends are ordered by row.
A highlight candidate can use `partition_point` on `end.y < viewport_top`, then
stop when `start.y > viewport_bottom`. Preserve the original full-list index
when marking the current result. Validate matches spanning either viewport edge,
zero matches, a viewport between matches, multirow soft wraps, repeated matches
within one row, wide end cells, and current selection outside the viewport.
The existing snapshot benchmark isolates this scan but does not yet implement
or measure this candidate.

Keeping a SearchCorpus across refreshes is a different memory tradeoff. Its
mapping vector can exceed text storage many times over and would stay resident
while Find is open. Removing transient per-cell allocations first avoids
introducing that retained high-water footprint.

## Behavior gates

Current executable coverage in `src/terminal/find.rs` and emulator tests checks
ASCII-only case folding, exact non-ASCII matching, matches inside combining
clusters, nonoverlapping matches, soft-wrap matching, hard-line separation,
primary history, wide highlighting, stale query generation, wrapped navigation,
first navigation from the visible viewport, offscreen reveal, selected-match
preservation across output/reflow, pruning, and primary/alternate result scope.
The direct encoder keeps all downstream operations unchanged. A new focused
emulator regression searches from inside a 21-codepoint grapheme through a wide
cell and requires the exact head-to-wide-tail highlight. It covers Find's buffer
growth and mapping path, which the existing long-grapheme snapshot test did not
exercise. These tests and version 3 corpus equality pass.

`snapshot_at` refreshes Find before the clean-screen return. Every nonempty feed
invalidates an open Find, including cursor-only terminal output. A new Find
result marks search damage even when text rows were unchanged. Avoid suppressing
invalidation based solely on visible row damage: history writes, reflow, pruning,
alternate-screen changes, and tracked selection can change results. A metadata-
only fast path needs authoritative terminal text-change information.

## Native source status

The pinned Ghostty search interface was already researched in
[the previous terminal audit](performance-round2-terminal.md). Its installed
local header still documents newest-to-oldest matches, periodically fed terminal
changes, bounded ticks, and separate per-screen selection. The current Rust
wrapper does not expose that interface. The prior immutable
[Ghostty source](https://github.com/ghostty-org/ghostty/blob/b0c421fcd2e290629d4285c181b52fe2f2095f06/include/ghostty/vt/search.h)
and [WezTerm cell source](https://github.com/wezterm/wezterm/blob/b09b56c29c1e367e598b60ca266e2cc9038751e0/wezterm-cell/src/lib.rs)
remain the reference points; this continuation makes no claim about newer
upstream revisions. Native partial counts, selection retention across alternate
screens, and reversed navigation cannot replace current behavior silently.

## Other output allocations rechecked

- Snapshot cell text still uses `String::with_capacity(4)` for every rebuilt
  cell, including blank cells and wide tails. Direct UTF-8 conversion is already
  present there. Inline text or row-owned text would be a broader representation
  change and needs renderer plus retained-snapshot memory measurements.
- URL detection still allocates a per-cell result vector, flattened row text,
  and byte offsets for every rebuilt row. An ordinary-row fast rejection must
  preserve URL prefixes split across cell boundaries and Unicode whitespace.
- OSC 52 filtering still copies ordinary input into a Vec and allocates an
  effects list. PTY reading allocates a Vec for each read, and worker batching
  creates a fresh capacity-limited chunk vector. Borrowing or pooling these
  buffers is separate work with cancellation, backpressure, and protocol-order
  obligations. No new evidence warrants changing those paths in this pass.

Native application CPU, footprint, interaction latency, and equal output remain
acceptance gates after an isolated Find gain. This fixture does not measure GPUI,
GPU work, Find UI typing latency, or many-Pane behavior.
