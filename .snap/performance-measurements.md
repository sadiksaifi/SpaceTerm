# Performance measurements

## Baseline

- Source: `134d7027b2014f89d29d0f9c4d087e894f33cfbc`.
- Host: macOS 27.0 build 26A428, arm64.
- Hardware: Mac16,8, Apple M4 Pro, 24 GiB memory.
- Optimized source build: `mise run build:release` passed on retry. The first
  invocation reported a transient artifact-supervisor measurement failure;
  compilation finished and the second invocation verified the complete build.
- Focused idle and scrolling application captures completed with verified endpoint
  focus, equal output, and stable window bounds. Hidden and settled-history trials
  were excluded. See [application results](performance-application-results.md).

## Isolated baseline fixtures

These results measure selected work inside the application or a native GPUI
fixture. They are not total application launch, CPU, or GPU measurements.

| Fixture | Baseline median | Sampling |
| --- | --- | --- |
| Native font enumeration and all-family classification | 403.36 ms | 10 fresh processes, 260 families |
| Native font enumeration and default-family classification | 144.38 ms | 10 fresh processes, 4 classified families |
| Stable visible geometry preparation | 1.043 microseconds/call | 5 samples, 2,000 calls each, 48 x 120 grid, GPUI test text system |
| ASCII full-screen feed and snapshot | 337.003 microseconds/frame | 7 samples, 300 frames each, 120 x 40 grid |
| Unicode full-screen feed and snapshot | 538.718 microseconds/frame | Same frame count and grid |
| ASCII partial-row feed and snapshot | 14.580 microseconds/frame | Same frame count and grid |

The font fixture measures native enumeration and the same four-glyph
classification algorithm in an isolated executable. It excludes other launch
stages. Completing the font catalog on first Settings open defers work to that
interaction; it does not eliminate all catalog work.

The native compression experiment revealed that Ghostty's byte budget further
limits the configured 10,000-row maximum. The current 120-column fixture retains
497 Scrollback rows. Preserve both existing limits in this performance work.

Candidate measurements and checks:

- [Snapshot text](performance-snapshot-results.md).
- [Visible row geometry](performance-rendering-results.md).
- [Startup font work](performance-launch-results.md).

Use [the existing protocol](../docs/performance.md) and its mise tasks for paced
workloads and process sampling. Keep builds outside measurement windows.

## Acceptance

Record baseline and candidate revision, fixture, repetitions, elapsed times,
variation, throughput, and applicable correctness checks. Measure the same fixture
before and after each change. Distinguish a microbenchmark from a complete
application measurement. Never extrapolate a microbenchmark percentage to launch,
GPU use, or total application CPU.

Record missing metrics explicitly. Process footprint is not GPU residency;
wakeups are not frame counts. Shell startup and remote authentication are separate
from the application-owned launch critical path.

## Native font classification baseline

On macOS 27 arm64 at baseline source `134d702`, `mise run bench:macos:fonts` ran ten fresh optimized GPUI processes per mode. The fixture uses the same native `TextSystem` enumeration and four-glyph classification algorithm as startup, but is an isolated microbenchmark. It found 260 family names. Each time below is measured inside the process after GPUI application creation.

| Mode | Classified families | Median | Range |
| --- | ---: | ---: | ---: |
| Names only | 0 | 120.98 ms | 115.17-133.51 ms |
| Current full classification | 260 | 403.36 ms | 373.08-498.23 ms |
| Proposed default selected families | 4 | 144.38 ms | 125.34-148.84 ms |

The selected-family fixture suggests about 259 ms less startup font work than full classification on this host. It does not measure first frame, custom selected fonts, Settings first-open cost, or total launch. Different modes ran in sequence, so shared operating-system font caches can affect the ranges. Keep the exact initial font, then measure the application before claiming a launch improvement.

## Coverage to collect

1. Optimized launch: first native frame, first terminal frame, and shell-ready time.
2. One Pane: idle, partial output, scrolling, selection, Terminal Find, resize.
3. Identical output: focused, unfocused-visible, hidden, inactive Tab, restored.
4. Pane scaling: 1, 4, 16 idle Panes and equivalent active output per Pane.
5. Graphics: fixed image, replacement/deletion, animation, hide/restore, close.
6. Lifetime: repeated creation/closure with settled footprint and retained owners.
7. Native GPU profiling and appearance comparison for rendering changes.
