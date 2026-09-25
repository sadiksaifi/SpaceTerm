# Final launch agent review

Scope: current uncommitted Find encoder and release fixture, application and
Metal startup probes, application resource harness, its parser tests, and the
changed workload acknowledgment path. Read-only source review; no builds,
native actions, or captures. The coordinator owns the final binary and gates.

## Result

No Findings after the coordinator's harness correction.

The review identified a P2 shared-file-offset race: seeking the inherited stderr
file to zero could redirect a concurrent app sampler write and lose records.
The coordinator replaced it with `os.pread(fd, os.fstat(fd).st_size, 0)` at
`scripts/measure-macos-application-resources.py:583`. Source reinspection confirms
that capture no longer changes the child's write position. The finding is
closed. Final captures use this corrected harness; no specific earlier saved
capture was shown to be corrupt.

## Other reviewed behavior

- The Find encoder produces the same UTF-8 bytes and per-byte head-cell/width
  mapping as the previous String traversal. Full-cluster reservation preserves
  vector growth. Long combining clusters still grow the input buffer. Matching,
  ordering, navigation, hard/soft lines, and wide tails are unchanged.
- The ignored release fixture retains an independent legacy traversal and checks
  exact bytes, mappings, capacities, and actual retained rows before timing.
  Setup is outside timing; result destruction is inside both corpus timings.
  The focused emulator test covers a match beginning inside a long cluster and
  ending at a wide cell. No material test or production finding.
- Startup probes are feature- and environment-gated, emit fixed numeric fields,
  and bound stages and renderer count. Normal builds omit the instrumentation.
  Constructor intervals exclude event logging; application stage intervals
  include it. No material startup hook finding.
- Memory parsing permits fixed category labels and numeric values and rejects
  native errors, warnings, foreign process IDs, and unexpected units. Arbitrary
  native category names are aggregated without emitting them.
- Workload comparison requires equal output totals and grid dimensions. A
  bounded standard cursor reply acknowledges consumption after workload output.
  Its four-byte query is accounted separately. Native memory inspection occurs
  after output/capture and while the synthetic shell remains alive.
- Window control targets retained owned processes. Focused geometry is compared
  before state changes and across trials.
  Floating changes only the owned window's layout. Parser tests cover changed
  grids, missing acknowledgments, state mismatch, and unknown native metadata.

## Final native helper correction

The coordinator's native run found that a windowless application would not
become frontmost on this macOS host, even when activation reported success.
The helper now owns one titled 120-by-60-point content window at AppKit (8, 32),
installs a main menu, and makes that window key before self-activation. Readiness
requires one visible window, finished launch, and frontmost/active state. The
parent harness must require that window not to overlap the benchmark window.

The explicitly requested isolated native smoke passed: the helper was frontmost,
not hidden, and fully launched, with one frame at Quartz X=8, Y=862, width=120,
height=88. Those bounds do not intersect the coordinator's benchmark frame at
X=306, Y=201, width=900, height=580. Closing the retained stdin pipe terminated
the helper and its parent reaped it. No other application was controlled.
`mise run lint:macos:scripts` passed. No Rust rebuild was required.

## Measurement limits

Find corpus measurements do not establish whole-application typing latency or
many-Pane behavior. Startup stages begin at application `main`, not process
creation, and window construction is not first presentation. The app sampler
adds one wake per second. A cursor reply proves terminal consumption, not a
presented frame. Focus and visibility are observed at capture boundaries, not
continuously. Frame counters use a shorter interior interval than resource
samples. Logical Metal allocation and native physical footprint are different
metrics, and neither measures GPU power or execution time. These limits must
remain attached to any performance claims.
