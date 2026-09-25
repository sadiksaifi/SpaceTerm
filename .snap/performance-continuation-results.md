# Continuing performance results

Status: final native captures and full validation passed; final publication pending, 2026-09-26.

## Build and capture conditions

Host: Mac16,8, M4 Pro, 24 GiB, macOS 27.0. CoreGraphics reports a
1512 x 982 display mode, 3024 x 1964 pixel mode, and 120 Hz. Final application
captures use identical 900 x 580-point windows at (306, 201), a 60 x 24 terminal
grid, fresh isolated Settings, and the same deterministic workload. No build
runs during a performance interval.

The comparison baseline already includes the earlier PR changes, shared native
display-link ownership, and lazy path targets. It keeps continuous visible frame
callbacks. Baseline binary: `spaceterm-continuation-lazy-paths`, SHA-256
`140afe7cfd325bbd2e7c2efb6aa8e7684a33e212e713e464f6c1f74d0c300b6a`.
Final probe binary: `spaceterm-continuation-final-probes`, SHA-256
`15ecb87140ea18ddd132f4af01fca07c8d03ab7e2f41b420c608eebb998b6a0f`.
It includes complete demand scheduling, immediate asynchronous first wake,
Find encoding, native exit observation, and the full startup stage probes.

Each application capture includes a 1 Hz numeric sampler in both binaries.
Normal application builds omit these probes. DSR after output confirms that the
terminal processed the stream and replied; it does not prove presentation.
Frame counts are measured over their own shorter interior interval and must be
normalized by that interval before comparison with one another.

## Final focused scrolling

Log: `continuation-final-scroll-fixed.jsonl`. Four ABBA trials pass geometry,
output, grid, and consumption checks. Every producer completes 1,200 updates,
9,640 lines and 1,050,487 bytes; the DSR query adds four separately counted bytes.

| Role | Trial | CPU, one core = 100% | Interrupt wakes/second |
| --- | --- | --- | --- |
| Baseline | 1 | 20.632% | 178.43 |
| Candidate | 1 | 21.679% | 107.48 |
| Candidate | 2 | 21.810% | 108.35 |
| Baseline | 2 | 23.459% | 186.41 |

CPU ranges overlap; no focused-scrolling CPU improvement is claimed. Wakes are
lower while output and consumption remain equal. Native source starts occur
approximately once per output-driven scene during this workload. This is
included in measured CPU, not omitted from accounting.

## Final focused idle

Log: `continuation-final-idle.jsonl`. Baseline CPU is 0.815% with 121.58
interrupt wakes/second. Candidate CPU is 0.074% with 1.60 wakes/second. The
baseline executes 1,080 clean native/logical frame callbacks in its nine-second
counter interval; the candidate executes zero. Both complete the same fixture
and acknowledge terminal consumption. Footprint is 74.11 versus 74.20 MiB, so
this result is a CPU/wakeup improvement, not a memory reduction.

Earlier two-pair demand captures independently showed zero idle frame callbacks
and CPU of 0.113-0.135% versus 0.765-0.775%. Those precede the immediate-wake
adjustment and are recorded separately in performance-continuation.md.

## Final unfocused-visible scrolling

Log: `continuation-final-unfocused-window.jsonl`. The small owned helper is
frontmost and its bounds do not overlap SpaceTerm. SpaceTerm remains on-screen
and nonhidden at both capture boundaries. Both trials complete and acknowledge
all 1,200 updates. Both continue drawing approximately 49 scenes/second after
normalizing the counter intervals.

Baseline: 23.015% CPU and 182.21 interrupt wakes/second. Candidate: 20.815% CPU
and 112.17 wakes/second. This is one paired state/behavior capture, not a broad
unfocused CPU-performance guarantee.

## Final hidden scrolling

Log: `continuation-final-hidden.jsonl`. Both processes are verified hidden and
complete and acknowledge all 1,200 updates. Neither executes a logical frame,
draw, or presentation during the counter interval. Baseline CPU is 0.509% and
candidate CPU is 0.638%; both report approximately 2 interrupt wakes/second,
including the probe sampler. No hidden CPU or RAM improvement is claimed.
This capture verifies continued terminal processing and replies with rendering
asleep, using the final binary.

## Native frame latency

The initial demand candidate saved idle wakes but added restart latency. The
retained first-wake signal removes that penalty. In eight alternating samples
per mode, continuously paced versus fully idle source medians are:

| Trigger | Continuous source | Idle source with initial signal |
| --- | --- | --- |
| Entity notification to presentation observation | 4.664 ms | 1.293 ms |
| Native key input to presentation observation | 5.554 ms | 1.377 ms |

The worst idle samples are 1.411 ms and 1.817 ms respectively. This fixture
observes scene submission with 1 ms polling. It is not a photon-latency test or
a historical-binary comparison. Log: continuation-frame-demand-first-wake-latency.log.
The real native behavior fixture passes notification, queued callback, animation,
direct draw, input grace, hide/restore, unfocused-visible rendering, and close
inside a callback.

## Other measured changes

The native process-exit observation fixture reduces 16 idle observer threads
from about 1.72% CPU and 1,334 interrupt wakes/second to 0.0007% CPU and 0.33
wakes/second. These are owned local children exercising the mechanism, not
complete connected Remote Workspaces. See performance-remote-exit.md.

The actual production Find encoder has exact UTF-8, byte-to-cell mapping, and
vector-capacity parity with the retained legacy implementation. Median corpus
construction is 34.2-37.6% faster for ASCII, 6.6-7.7% for sparse rows,
9.5-12.4% for Unicode, and 34.6% faster with 10,001 retained narrow rows.
See performance-find-continuation.md.

Lazy path targets eliminate three ordinary-startup allocation sets while native
path pixels and resizing remain unchanged. They do not establish a large
physical-RAM reduction. The native allocation fixture measures a 24.144 to
0.207 microsecond median drawable-resize component cost without paths, and
112,852,992 to zero bytes of peak path allocation accounting. Those values
are neither complete resize latency nor physical residency.
The roughly 96 MiB graphics-ledger transition occurs
with zero path targets too. See performance-path-textures.md and
performance-metal-ledgers.md.

## Excluded attempts

- A pre-hide geometry mismatch invalidated two earlier hidden comparison pairs.
- Explicit AeroSpace floating requests were rejected after AeroSpace became
  disabled. The harness did not enable it or change its configuration. Final
  captures use the application's natural geometry without that optional flag.
- The windowless focus helper never became frontmost on this host. It was
  replaced with an owned small window and strict non-overlap verification.
- Review found shared stderr-offset interference in the probe reader. Final
  captures use `os.pread`, leaving the application's write offset untouched.

## Final validation

`mise run validate:macos` exits 0. The gate includes 3,123 workspace tests,
206 native macOS tests, native Metal backdrop and path pixel tests, display-link
registry tests, the alternate Blade compile check, vendor tests, formatting,
lint, and 20 benchmark parser/ownership checks. The native frame lifecycle,
complete frame behavior, and first-wake latency fixtures also pass.
Three independent final reviews report no unresolved material findings.
The artifact supervisor race discovered during the first gate was reproduced,
fixed, tested, and included in the successful rerun. See
performance-artifact-supervision.md and the three performance-final-review files.

The normal, probe-free `mise run build:release` also exits 0 (32.09 seconds).
Final release binary SHA-256:
`e892e8523c31330b55d80bf2d74087d4bbb2876da4d28bf7d50f064d75fde05a`.
Log: `target/performance/continuation-build-final-release.log`.
