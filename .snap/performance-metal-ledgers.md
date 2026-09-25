# Residual Metal memory attribution

Date: 2026-09-26. Scope: read-only source and primary-source audit. No allocation,
drawable-count, texture-format, storage-mode, or instrumentation change is made
by this investigation.

## Conclusion

The remaining roughly 96 MiB graphics-footprint transition is not attributed.
The coordinator reports that lazy path targets reduce their allocation count to
zero for path-free scenes, while active process footprints remain about 229-233
MiB and the later transition persists. The earlier similarity between graphics
ledger bytes and theoretical path-target bytes therefore did not establish
causation. Do not credit lazy allocation with a measured physical-RAM reduction.

The next discriminator is actual Metal allocation inventory alongside physical
footprint over the transition. Keep logical resource allocation, drawable memory,
and process physical accounting separate. Apple explains that allocating a
resource need not immediately consume physical pages, and that dirty/resident
accounting differs from resource allocation. It associates Metal allocations
with IOAccelerator and drawables with IOSurface in its memory tooling. See
[Apple's Metal memory analysis guide](https://developer.apple.com/documentation/xcode/analyzing-the-memory-usage-of-your-metal-app),
checked 2026-09-26.

## Local owners and discriminators

| Owner | Confirmed source behavior | Next numeric measurement |
| --- | --- | --- |
| `metal_renderer.rs:MetalRenderer::new` | CAMetalLayer uses BGRA8Unorm, nonopaque output, and three drawables | Actual acquired texture width, height, allocated size, configured drawable count, current device allocation |
| `metal_renderer.rs:InstanceBufferPool` | Starts at 2 MiB per managed buffer; retains completed buffers; doubles size on rendering overflow up to the existing 256 MiB limit | Buffer size, live allocated bytes, free/outstanding counts, peak outstanding, create/release counts |
| `metal_atlas.rs:MetalAtlasState::push_texture` | Allocates only on demand, at least 1024 by 1024; A8 for monochrome and BGRA8 for polychrome | Live texture count and sum of actual allocated size separately for each kind |
| `metal_renderer.rs:update_path_intermediate_textures` | Candidate allocates resolve/MSAA targets only when a path renders | Existing allocation count plus actual allocated bytes, sample count, dimensions, create/release counts |
| `metal_renderer/backdrop.rs:BackdropRenderer` | Constructor builds a pipeline without textures; first filter allocates a full capture plus two quarter-width/quarter-height buffers; path-free is unrelated to backdrop-free | Current backdrop texture bytes and dimensions; number of frames with backdrop filters |
| `metal_renderer.rs:MetalRenderer::new/draw` | Owns shaders, pipelines, a command queue and a CVMetalTextureCache; first GPU use can occur after construction | Device allocation after construction stages, first acquired drawable, submission/completion, and later settled samples |

Use exact actual resource sizes. The installed `metal` crate already exposes
`DeviceRef::current_allocated_size()` and `ResourceRef::allocated_size()`;
no Objective-C API shim is needed for those queries. Apple's
[currentAllocatedSize](https://developer.apple.com/documentation/metal/mtldevice/currentallocatedsize)
is the device's total allocated resource memory, while
[allocatedSize](https://developer.apple.com/documentation/metal/mtlresource/allocatedsize)
reports an individual resource's bytes. Neither property's documentation defines
it as process physical footprint. Do not sum the same device's total once per
renderer or count the shared instance pool once per window.

The renderer returns each instance buffer only from its command buffer's
completion handler. The free list has no explicit count limit. Triple drawables
suggest modest concurrency, but do not prove a hard bound on delayed completion
callbacks or pooled buffers. A count/byte measurement is required before changing
that policy. The current terminal workload does not establish that a buffer ever
grows beyond 2 MiB.

## Drawable sizing and the 64.4 MiB IOSurface observation

The 1472 by 937 value in prior captures is a native window size in points. It is
not necessarily the CAMetalLayer's actual device-pixel size. Titlebar/content
bounds, backing scale, resize history and allocation alignment matter.

For illustration only, doubling both full-window dimensions yields 2944 by 1874
pixels. One four-byte BGRA image is 22,068,224 bytes, or 21.046 MiB; three are
63.138 MiB. That is the same scale as the observed 64.4 MiB IOSurface dirty total,
but it does not identify the total's resources. Actual drawable texture dimensions
and sizes must precede this calculation. Do not force measurements to match the
full-window estimate.

Apple describes a pool owned by CAMetalLayer. Its
[maximumDrawableCount](https://developer.apple.com/documentation/quartzcore/cametallayer/maximumdrawablecount)
accepts two or three, with three as default. Its
[nextDrawable](https://developer.apple.com/documentation/quartzcore/cametallayer/nextdrawable())
waits for an available pool entry. An instrumentation pass must not retain extra
drawables or textures to enumerate the pool: doing so changes availability and
can stall the renderer. Sampling the current acquired drawable is safe; that
single sample does not prove all configured pool entries are resident.

Changing three drawables to two is a separate candidate with a possible frame-
pacing tradeoff. It is not an explanation for the current 96 MiB transition and
should not precede inventory. Apple's
[dynamic-buffer guidance](https://developer.apple.com/library/archive/documentation/3DDrawing/Conceptual/MTLBestPracticesGuide/TripleBuffering.html)
describes the CPU/GPU overlap and memory/latency tradeoff. That guidance concerns
dynamic buffers; it is not a direct assertion about this app's drawable footprint.

## A content-free measurement seam

Use an explicit probe build, with no hooks in the production default. Prefer a
separate typed memory snapshot over silently placing gauges in the existing
cumulative frame-counter API. The harness currently derives frame rates from
counter differences; a current-byte gauge must not be interpreted that way.

Record at 1 Hz and at named finite lifecycle boundaries:

- Device current allocation, once per actual device ownership group.
- Live/count and created/released bytes by path, atlas kind, backdrop, instance
  pool, and fixed vertex-buffer category.
- Last acquired drawable width, height, allocated size, configured pool count,
  and whether the layer is framebuffer-only.
- Outstanding/completed command-buffer counts and pool size/free/outstanding
  counts, so transient GPU ownership is distinguishable from retained free buffers.
- Existing scene/present/path counters and process graphics/physical ledgers at
  the same monotonic sample times.

Retain only numbers and fixed category names. Do not emit object pointers,
resource labels, glyph keys, texture contents, terminal data, native error text,
or environment values. Do not keep strong drawable references for measurement.
Changes to resource ownership for instrumentation need the same lifecycle care
as production code; queued command buffers may retain resources after Rust drops
its own handle. A logical owner release and a driver physical release are not
necessarily simultaneous.

A single latest acquired-drawable size is a gauge, not a pool-memory sum. If the
existing safe numeric summaries and explicit owner counts cannot identify the
residual, Apple's Metal Resource Events / VM Tracker are the next diagnostic
capabilities. Their capture changes workload timing and must run outside the CPU
comparison interval. The numeric output needs the repository's existing content-
free sanitization contract.

## How the measurements discriminate

1. If category live allocations change with the physical drop, inspect that
   owner's retain/release events and completed-command timing. The timing is a
   lead, not proof that bytes map one-for-one to footprint.
2. If device allocation drops but all explicit owned categories stay constant,
   inspect drawables and uncounted framework resources. The difference must not
   automatically be called a driver cache.
3. If both device and owner allocations stay constant while graphics footprint
   drops, investigate physical residency, reuse and accounting changes. Do not
   claim that a retained logical allocation is a physical leak.
4. Repeat with fixed actual drawable dimensions at two sizes. Drawable/render-
   target costs should scale with pixel area; fixed atlas pages and the starting
   instance buffer should not. Preserve the same content and presentation mode.
5. Repeat active output, stopped output while still visible, and hidden states
   with explicit timestamps. A visibility transition is not the same condition
   as ending the producer.

## Pixel and behavior gate for later candidates

Preserve backing resolution, BGRA format, transparency/blending, path sample
count, backdrop semantics and all content. Memoryless MSAA is a separate possible
Apple-GPU optimization only if the attachment is used within one render pass and
its resolved output is the retained value. Apple documents these restrictions in
[MTLStorageMode.memoryless](https://developer.apple.com/documentation/metal/mtlstoragemode/memoryless).
It cannot explain a transition when the path allocation count is already zero.

Any later drawable/pool/storage change must pass the existing actual pixel and
fractional-edge MSAA tests, backdrop parity, resize, first presentation, repeated
hide/restore, multiple windows, and display-scale changes. Compare sustained
output pacing and input/restoration latency as well as bytes. A lower allocation
count with missing pixels, altered antialiasing or extra frame stalls fails the
user's constraint.


## Review of the bounded path-allocation fixture

`path_target_allocation_resources` measures the direct MetalLayer size setter
and target-creation calls, with resource-size queries outside the timed region
and alternating strategy order. It can report logical path-resource bytes and
component operation time. It does not acquire a drawable, submit work, write
path pixels, or measure first-path latency or process physical RAM.

Its eager control calls the candidate's helper, including equal-size reuse.
The original helper recreated targets even for equal sizes. The repeated
small/large/small sequence has 19 equal-size boundaries among 60 operations,
so this is an eager-with-reuse mechanism control rather than an exact old-source
benchmark. That makes it a conservative allocation-work control. Device peak
bytes also include fixed renderer resources; the separately summed path targets
are the precise owned category. Keep these labels when reporting results.
