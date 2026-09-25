# Performance problem inventory

Status: initial research complete. This inventory precedes implementation. Entries
describe observed code costs or profiling questions, not measured application
bottlenecks. Research records provide upstream evidence and tradeoffs.

| Area | Evidence or question | Potential solution | Required evidence |
| --- | --- | --- | --- |
| Launch | `StartupDependencies::capture` probes SSH synchronously before GPUI creation | Defer remote-only work behind its owner, preserving availability behavior | Probe latency, first frame, unavailable-SSH behavior |
| Launch | Settings, appearance, controls, native services initialize before first window | Profile stages; defer only independent work | Stage timings; identical first frame and menus |
| Snapshot text | `TerminalEmulator::snapshot_at` calls `graphemes()` for each cell, allocating `Vec<char>` before `String` | Use existing UTF-8 destination-buffer interface | Optimized snapshot benchmark; Unicode, empty and wide-tail fidelity |
| Snapshot colors | New palette/configuration arrays are allocated before clean-snapshot early return | Reuse immutable appearance state where identity is known | Allocation profile; OSC colors and appearance invalidation |
| Row preparation | Geometry preparation runs again on unchanged rows | Cache complete prepared geometry using all layout dependencies | Optimized benchmark; origin, clipping, scale, colors and content invalidation |
| Frame scheduling | GPUI display link appears active for clean visible windows | Investigate on-demand scheduling with correct restart | Native wakeups; input/resize/animation reliability |
| Spinner | Ten discrete frames use per-display-frame animation | Schedule discrete frame transitions | Identical phase/cadence, lifecycle and redraw counts |
| GPU allocations | Metal path targets are allocated for drawable size | Lazy allocation and reuse if paths are absent | GPU capture; paths, blur, resize and scale pixel checks |
| Scrollback | Application does not schedule available Ghostty compression | Bounded idle compression driven by activity token | Footprint, compression CPU, restored text, selection/search and latency |
| PTY throughput | Each read allocates a chunk; queues are bounded | Profile copying and consider buffer reuse | Equal throughput; input fairness and bounded memory |
| Pane scaling | Each Pane owns worker, reader, termination threads | Measure stack/residency and idle scheduling before changing ownership | 1/4/16-Pane scaling, deterministic teardown |
| Terminal Find | Dirty queries reconstruct all available Scrollback with per-byte cell mapping | Incremental corpus or streaming matches if measured | Search semantics, wraps, reflow, pruning, active output latency |
| Hyperlinks | Each rebuilt row constructs text, offsets, result vectors | Avoid unnecessary intermediate storage or reuse scratch buffers | Unicode cell mapping, OSC precedence, local authority unchanged |
| Accessibility | Demand-driven snapshots coexist with native accessibility clients | Profile native demand separately from headless fixtures | Assistive-client parity and CPU/resource captures |
| Hidden state | Existing snapshot gates and UI cache eviction already suppress work | Verify scaling and restoration; optimize only remaining work | Output continues, replies/metadata preserved, exact restored screen |
| Graphics lifetime | Multiple decoded/uploaded/native image representations | Profile reservation and GPU owners across replacement and close | Image restoration, quota semantics and settled footprint |
| Build configuration | Default release profile; native VT build choices | Compare optimization/LTO variants after runtime baselines | Launch, throughput, binary size; never trade correctness for size |

## Selection gate

For each implementation, identify its owner, unchanged behavior, repeatable
before/after fixture, invalidation or lifecycle risks, and tests that cover them.
Prefer changes that remove repeated work at an existing interface. Record why
larger changes are deferred when measurement or correctness evidence is missing.

## Selected experiments

1. Direct cell UTF-8 construction: remove the intermediate character vector while
   retaining owned text and all cell semantics.
2. Stable visible geometry reuse: avoid preparing unchanged rows with unchanged
   layout. Validate every geometry invalidation key.
3. Idle Scrollback compression: measure native benefit before adding bounded
   worker-owned scheduling. Preserve logical history and interactive latency.
4. Startup font work: measure full catalog classification; assess resolving only
   requested fonts before first window and obtaining the full catalog on demand.

The SSH availability redesign, framework frame-source changes, GPU path texture
allocation, and incremental Find index remain profiling candidates. Their risk
and missing measurements do not justify speculative production changes in the
first implementation batch.
