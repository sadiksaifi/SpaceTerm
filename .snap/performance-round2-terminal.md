# Terminal performance follow-up

Research date: 2026-09-26. Local baseline: `1e25ad960d27fd7f526d083901c4e6ae10a21a60`.
Status: source audit complete; candidates below have not been implemented or measured in this follow-up.
The user explicitly requested `.snap` research records. No production code, dependency pins, or build settings were changed by this audit.

## Recommendation

Measure and remove repeated color snapshot allocations first. Then profile the output filter and active Terminal Find before selecting a larger change. The strongest newly identified engine opportunity is Ghostty's existing incremental search C API, which SpaceTerm's Rust wrapper does not expose. Its navigation semantics differ, so it needs a parity adapter and differential tests.

The earlier direct UTF-8 cell construction and idle compression are already implemented. Their results are in [engine research](performance-research-engines.md) and [snapshot measurements](performance-snapshot-results.md). They are not new recommendations.

## Sources and applicability

Source links were checked on 2026-09-26. Ghostty is pinned locally at `b0c421fcd2e290629d4285c181b52fe2f2095f06`; local patches still participate in the actual application build. WezTerm comparisons use the previously researched immutable revision `b09b56c29c1e367e598b60ca266e2cc9038751e0`, not a claim about its latest release.

- Ghostty's [batched stream parser](https://github.com/ghostty-org/ghostty/blob/b0c421fcd2e290629d4285c181b52fe2f2095f06/src/terminal/stream.zig#L657-L782) already decodes runs in bulk, using SIMD where supported. Release builds take that path; the debug parser uses byte dispatch. SpaceTerm already benefits through libghostty-vt. Any second byte scan in SpaceTerm deserves independent profiling before modifying Ghostty.
- Ghostty's [search C interface](https://github.com/ghostty-org/ghostty/blob/b0c421fcd2e290629d4285c181b52fe2f2095f06/include/ghostty/vt/search.h) exposes bounded `tick`, terminal-reading `feed`, and blocking `run`. Its state survives primary/alternate screen switches and recovers after reflow, pruning, and reset. Matching is ASCII-insensitive and otherwise byte-exact. The wrapper must respect its terminal borrowing and serialization rules. SpaceTerm currently has no Rust search module.
- WezTerm's [cell representation](https://github.com/wezterm/wezterm/blob/b09b56c29c1e367e598b60ca266e2cc9038751e0/wezterm-cell/src/lib.rs#L547-L727) stores short text inline in `TeenyString`, with a heap fallback for longer clusters. Attributes use compact common fields and optional heap storage. This supports investigating compact immutable SpaceTerm cell text; it does not establish that copying WezTerm's unsafe representation is appropriate.
- WezTerm's [PTY pipeline](https://github.com/wezterm/wezterm/blob/b09b56c29c1e367e598b60ca266e2cc9038751e0/mux/src/lib.rs#L118-L259) separates reading from parsing and coalesces work with bounded parser buffers. SpaceTerm already separates PTY reading and Terminal Emulator work. Its buffer sizes should be tuned against input latency and equal-byte throughput, not copied from another terminal.
- Ghostty's [1.3 release report](https://ghostty.org/docs/install/release-notes/1-3-0#performance-improvements), released 2026-03-09, describes optimizing against public terminal recordings as well as synthetic tests. Add representative deterministic application traces to SpaceTerm's evaluation. Do not infer SpaceTerm gains from Ghostty's reported gains.

## Ranked candidates

| Order | Candidate | Source-established cost | Risk and next evidence |
| --- | --- | --- | --- |
| 1 | Reuse unchanged color state | Three fresh `Arc` allocations in every unsuppressed snapshot attempt, including clean attempts | Narrow change; benchmark clean, cursor-only, one-row and full snapshots |
| 2 | Borrow ordinary output through OSC 52 filtering | Per-read byte copy plus effect allocation before Ghostty parses the same bytes | Moderate protocol risk; profile ASCII and escape-heavy streams first |
| 3 | Reuse Find text scratch storage, then evaluate native incremental search | Full history traversal and one temporary `String` per populated cell after each output invalidation with nonempty Find | Large potential while Find is open; preserve match ordering and publication behavior |
| 4 | Store common cell text inline | One heap allocation per rebuilt cell, including blanks and wide tails | Broad type change; requires retained-memory and renderer measurements |
| 5 | Skip unnecessary URL detection scratch work | Three scratch allocations per rebuilt nonempty row even when it contains no URL | Narrower than cell representation; prove conservative fast rejection against current schemes |
| 6 | Reuse PTY read/batch storage | Fresh `Vec` per read and fresh eight-slot batch vector per worker batch | Keep bounded queues, fairness, shutdown and backpressure; measure allocation hot spots first |

These are source costs, not measured resource improvements. Candidate order balances semantic risk and experiment cost, rather than claiming measured impact order.

### Color state

In [snapshot construction](../src/terminal/emulator.rs), `snapshot_at` obtains native colors and allocates configured colors, a 256-entry palette, and 256 override booleans before checking `Dirty::Clean` and `damage.is_clean()`. The palette payload is 1,024 bytes (`Color` has four `u8` fields); override payload is 256 bytes. These figures exclude Arc headers, configured colors, allocator overhead, and the native color query's separate stack copies.

Compare raw/current values against retained state, then clone existing Arcs on equality and allocate on changes. Keep reading the metadata that detects changes; an early return based only on native row dirtiness could lose appearance, palette override, cursor, selection, graphics, metadata, or Find changes. Reuse configured colors while the applied appearance is unchanged. Existing immutable snapshots must retain their old values.

Parity cases: OSC 4 and reset; default foreground/background/cursor set and reset; reverse colors; bold-as-bright; host appearance updates while application overrides exist; cursor text contrast; primary/alternate screen; clean repeated attempts. Compare complete snapshots against the baseline and ensure unchanged attempts still return `None`. Add clean/cursor-only cases to the existing optimized snapshot fixture. At 60 snapshot attempts/s these three calls represent 180 allocation requests/s per presentable Terminal Session, but actual attempt frequency is workload-dependent; idle Sessions are event-driven.

### Output filtering

[PTY reader](../src/platform/native_pty.rs) copies every successful read into a `Vec`; [OSC 52 filtering](../src/terminal/native_services/osc52.rs) then allocates `terminal` with the input length and scans/copies bytes one by one. `feed` also returns an allocated effects list. Completed OSC 52 sequences clone raw bytes and parse an operation even though the worker denies the operation. Retaining protocol ordering is essential: raw bytes reach Ghostty and operation boundaries flush earlier replies.

A candidate borrowed-slice/callback interface should pass ordinary contiguous runs without copying and retain owned storage only for split prefixes or sequences. A conservative ground-state path for chunks with no ESC is a smaller first experiment. Avoid changing denial behavior, malformed sequence handling, oversize bounds, reply order, or synchronized-output boundaries. Tests must split sequences at every byte position and interleave terminal queries, OSC 52, focus reporting, and ordinary output.

Measure fixed-byte plain text, many tiny writes, full-screen TUI escapes, malformed OSC, and long valid OSC. Record allocation count and bytes, parser time, completed bytes/s, p95/p99 input delay, and process CPU. A larger read buffer is a separate experiment because it can increase latency and per-Pane retained memory.

### Terminal Find

[Find refresh](../src/terminal/find.rs) rebuilds `SearchCorpus` over every retained row when dirty and the query is nonempty. [Emulator feed](../src/terminal/emulator.rs) invalidates Find after nonempty output. Corpus construction allocates a temporary UTF-8 `String` for each populated cell, a pending-spaces vector per row, and one `Option<CellMapping>` for every corpus byte. Multibyte graphemes therefore duplicate mapping data. Snapshot generation scans every match to collect viewport spans. Compression may have to restore pages touched by Find.

The small first experiment reuses one text scratch buffer across cells and reuses bounded row scratch space. It preserves the existing corpus and matching algorithm. The larger experiment adds a narrow safe wrapper over native search and compares it against current Find using the same terminal trace.

Native search is not a drop-in replacement. Native matches are newest-to-oldest; SpaceTerm currently stores oldest-to-newest. Native `SELECT_NEXT` moves toward older content; current Next increments toward newer content. Native search retains each screen's selected match; current Find clears it after a screen switch. Native incremental results can be partial; current refresh publishes a complete result set. Preserve these behaviors through an adapter or reject the experiment. Do not introduce partial counts or delayed highlights without separate product authorization.

Differential cases: ASCII case folding, non-ASCII byte matching, long combining clusters, wide cells, blank and trailing cells, wrapped matches, hard newlines, nonoverlapping repeated matches, first navigation from a scrolled viewport, query replacement, prune, reflow, reset, alternate screen transitions, compression, selected-match anchoring, and Pane closure during pending work. Measure Find closed/open with identical output and actual retained rows. The prior engine fixture retained 497 Scrollback rows despite a requested 10,000 because a byte limit also applies; report observed history rather than assuming 10,000.

### Cell text and hyperlink scratch

The current snapshot uses `String::with_capacity(4)` for every dirty cell. A 120 x 40 full rebuild therefore requests at least 4,800 cell text allocations, including spaces, before row scratch storage. This is a deterministic call-site count, not an allocation-profiler result. Consider a safe compact text type with inline bytes and heap fallback, or immutable row-owned text plus cell ranges. Preserve unbounded valid grapheme clusters and stable owned snapshots; do not cap text to fit inline storage. Measure type size, allocations, retained footprint for many Panes, full redraw time, and downstream shaping before selecting a representation.

[URL detection](../src/terminal/native_services/hyperlink.rs) allocates its result vector, flattened row text, and cell offsets for each rebuilt row. A cheap conservative scheme-prefix check could avoid work on ordinary text, while reusing scratch buffers could retain exact detection behavior. Any guard must account for a prefix split across cells, Unicode whitespace, supported schemes, punctuation trimming, and precedence of explicit hyperlinks. Never cache local capability-bearing targets without retaining their existing authority and lifetime checks.

### PTY ownership and batching

The reader's 16 KiB stack buffer and bounded eight-event channel are existing safeguards. The worker creates `Vec::with_capacity(8)` for each batch and stops at intervening commands. A pool or reusable buffer protocol must include buffers held by the producer, queue and active batch, and release them when a Terminal Session closes. It must not make a slow or hidden Pane consume unbounded memory or wait for a UI frame to return buffers. Keep this separate from OSC filtering so gains can be attributed.

## Measurement handoff

Use `.mise.toml` tasks as command authority. Existing entry point: `mise run bench:one performance_snapshot`; add purpose-specific optimized fixtures through the same task if selected. Use `mise run test:one <filter>` for focused parity checks and the project's required validation tasks after production edits. This audit ran no builds or benchmarks, avoiding concurrent workload contamination.

Collect before/after from optimized source builds with identical geometry, output bytes, retained history, active query, display scale and visibility. Include one Pane and many Panes; focused, unfocused-visible, hidden and inactive Tab conditions. Record CPU, physical footprint, allocations, wakeups, completed output, and input latency. No engine-only timing proves lower GPU use or faster first frame. Preserve the terminal content and appearance comparison as an acceptance gate, and record missing native measurements explicitly.
