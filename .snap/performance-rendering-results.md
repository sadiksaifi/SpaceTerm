# Rendering measurements

## Stable visible geometry

Baseline source: `134d7027b2014f89d29d0f9c4d087e894f33cfbc` plus the ignored
`performance_geometry` fixture. Command: `mise run bench:one performance_geometry`.
Optimized release test binary, macOS 27.0 build 26A428, arm64. One GPUI test
window with NoopTextSystem, 48 visible rows by 120 cells, one warmed cache,
2,000 unchanged calls per sample, five samples in one process and one test thread.
The fixture measures CPU cache preparation only. It does not measure native font
shaping, complete frame cost, GPU work, or application launch.

| Revision | Microseconds per call, sorted | Median | Result |
| --- | --- | --- | --- |
| Baseline | 1.022, 1.029, 1.043, 1.080, 1.091 | 1.043 | Pass |
| Exact source/layout cache, working tree | 0.00802, 0.00804, 0.00808, 0.00810, 0.00819 | 0.00808 | Pass |

The candidate median is about 129 times faster in this fixture and saves about
1.035 microseconds per unchanged call. This is not an application CPU or GPU
speedup claim. `mise run test:one terminal_element::tests` passed 67 tests and
ignored the benchmark as intended. The new focused test covers exact reuse,
origin, clipping bounds, font size, cell width, line height, scale, decoration
metrics, visible row count, color, selection, and eviction.
