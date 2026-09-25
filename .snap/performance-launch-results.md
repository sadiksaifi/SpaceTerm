# Launch work results

## Native font classification

The startup appearance path enumerated all installed font families and resolved and measured four glyphs in every family before the first Workspace window opened. The selected Font families alone determine the first appearance. The change retains the name enumeration, classifies the requested Chrome and Terminal fonts and default Terminal fallback families before appearance resolution, and classifies remaining families before the first Settings Window renders. The resolver and startup selector now share the ordered default Terminal family list. Later appearance changes classify a newly requested named family before resolution; explicit font reload still refreshes the complete catalog. [`src/ui/appearance_runtime.rs`](../src/ui/appearance_runtime.rs), [`src/appearance/resolution.rs`](../src/appearance/resolution.rs), [`src/ui/settings_window.rs`](../src/ui/settings_window.rs).

The isolated native fixture ran on macOS 27 arm64 with an optimized GPUI executable and a fresh process for each sample. It mirrors the startup enumeration and four-glyph classification algorithm. It found 260 family names. Baseline samples preceded the implementation; candidate samples followed it. Times start after GPUI application creation and stop after classification. They exclude SSH probing, Settings file I/O, window construction, Terminal Session startup, rendering, and shell readiness.

| Fixture and revision | Samples | Median | Range |
| --- | ---: | ---: | ---: |
| Baseline names only | 10 | 120.98 ms | 115.17-133.51 ms |
| Baseline all 260 families | 10 | 403.36 ms | 373.08-498.23 ms |
| Baseline four default selected families | 10 | 144.38 ms | 125.34-148.84 ms |
| Candidate all 260 families | 10 | 391.37 ms | 382.90-454.50 ms |
| Candidate four default selected families | 10 | 129.77 ms | 124.39-141.81 ms |

The candidate's same-run full versus selected comparison reduces isolated font work by about 262 ms at the median. The fixture copied the classification algorithm; it did not call the application's `capture_initial_fonts` function. The before and after runs used the same fixture, and their variation should not be interpreted as an additional 15 ms gain from changes to the fixture. Application first-frame, CPU, GPU, memory, and wakeup effects remain unmeasured. A selected named font can add classification work, and a different font collection can change both costs.

Completion of the full list is synchronous when Settings first opens. That cost moves from launch to Settings opening and has not been timed in the actual application. The first Settings frame still receives the full, ordered list. The complete list remains frozen until explicit font reload, matching the previous lifecycle except for the interval between launch and first Settings open. If installed fonts change in that interval without an explicit reload, late classification could observe a changed face; that system-font replacement scenario is untested. Startup family names and already selected font entries remain retained.

macOS GPUI assigns `FontId` values as families load. Selected families may receive different numeric IDs because they load before other families. `resolution_identity` formats that ID and is used only in runtime resolved typography and terminal row reuse; it is absent from the Settings Document and storage. The deferred catalog reuses the already classified selected entries, and catalog completion does not refresh or republish appearance. A different numeric ID across launches does not change the selected family or face and has no persistence contract.

`mise run test:one appearance_runtime` passed all 19 focused tests, including initial/full catalog equality and named-font refresh behavior. The combined `mise run test` passed 3,112 tests, including Settings checks. Rust lint and the optimized application build passed. The baseline and candidate commands are `mise run bench:macos:fonts full 10` and `mise run bench:macos:fonts selected 10`; the fixture is [`examples/performance_fonts.rs`](../examples/performance_fonts.rs).
