# Terminal snapshot measurements

## Change

Build each cell's owned UTF-8 text directly through Ghostty's existing
`graphemes_utf8` interface. A four-byte initial buffer accommodates one Unicode
scalar and grows for longer clusters. Blank cells and wide-character tails
remain a single space. This removes the intermediate `Vec<char>` and one native
call for ordinary populated cells. The snapshot still owns its text.

## Experiment

Command: `mise run bench:one performance_snapshot`. The ignored fixture uses a
120 x 40 Terminal Emulator, seven samples of 300 feed-plus-snapshot operations
per workload, alternating two payloads prepared before timing. Each sample warms
20 frames. The optimized Rust test binary enables `gpui/inspector` because existing
application tests require that interface; the fixture does not invoke GPUI.

Baseline uses the original cell construction at commit
`134d7027b2014f89d29d0f9c4d087e894f33cfbc`. Other independent branch changes do not
participate in this direct Terminal Emulator fixture. Host details are in
[measurements](performance-measurements.md).

| Workload | Baseline median (range), microseconds/frame | Candidate median (range), microseconds/frame | Median reduction |
| --- | --- | --- | --- |
| ASCII, all rows | 337.003 (330.130-386.452) | 224.835 (223.186-226.856) | 33.3% |
| Unicode, all rows | 538.718 (536.019-547.644) | 470.118 (463.986-471.228) | 12.7% |
| ASCII, one row | 14.580 (14.278-14.648) | 11.767 (10.904-12.183) | 19.3% |

Both optimized fixture commands passed. The Unicode payload includes combining
marks, CJK, and emoji. `mise run test:one
snapshot_preserves_full_graphemes_and_empty_cells` passed, checking a cluster with
20 combining marks, wide-cell tails, and empty rows against explicit text.

## Limits

These timings include parsing and immutable Screen construction. They exclude
PTY I/O, GPUI rendering, GPU work, accessibility clients, and complete application
CPU. No allocation profiler was used. Removal of the intermediate allocation is
source evidence; the table is the measured hot-path effect.
