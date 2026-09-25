# Independent follow-up review

Reviewed on 2026-09-26 against the working changes based on `1e25ad9`.
Reviewer: terminal research agent. No material findings in the retained snapshot
and glyph changes. The production font experiment was rejected and reverted.
This was a source review; the reviewer ran no builds or tests. Execution results belong in
[experiment results](performance-round2-results.md).

## Snapshot color reuse

Full value equality governs reuse of configured colors, palette values, and
override flags independently. Equal palette values cannot suppress a changed OSC
override flag. Native queries, appearance generation, reverse mode, cursor state,
active-screen detection, damage calculation, and publication checks remain intact.
The change shares immutable Arcs and cannot alter retained snapshots.

Existing tests cover equal OSC overrides and reset, appearance updates, reverse
colors, cursor state, screen transitions, clean suppression, and cursor-only row
reuse. The optimized snapshot-check fixture warms before seven timed samples and
measures clean attempts or cursor feed plus snapshot work. It does not establish
application CPU, memory, GPU, or launch improvements.

## Glyph color lookup

`FragmentBuilder` appends nonempty UTF-8 text and constructs strictly increasing
paint-run ends. `partition_point(end <= glyph_index)` therefore finds the same
first end greater than the index as the prior linear scan, regardless of shaped
glyph order. Lists of at most eight runs retain the prior scan. The final
`Hsla::transparent_black` fallback has the same four zero components as the prior
`rgba(0).into()` fallback.

The final parity fixture uses three and ten runs, checks explicit UTF-8 byte
colors in forward and reverse order, and covers empty and out-of-range lookup.
It exercises both lookup branches without asserting a performance result. The
benchmark isolates color lookup and excludes glyph rendering and GPU execution.

The final optimized log alternates candidate and legacy measurement order within
seven samples. At eight runs the candidate median is 0.182 versus 0.165
microseconds per 120-column row, a measured 0.017-microsecond overhead. At 120 runs
the medians are 0.549 versus 2.526 microseconds, about 78% lower lookup time.
The coordinator accepted this measured tradeoff; it is not a universal speedup.

## Rejected native font experiment

The reviewed prototype used the same `create_for_all_families` collection and
`kCTFontFamilyNameAttribute`. Apple's
[attribute API](https://developer.apple.com/documentation/coretext/ctfontcollectioncopyfontattribute%28_%3A_%3A_%3A%29)
and the installed `CTFontCollection.h` document one attribute value per descriptor,
missing values as `kCFNull`, and duplicate removal for option bit zero. The private
declaration matches the SDK's `uint32_t` option type. This does not substitute the
visible-family query that can omit hidden families.

The prototype checked null before owning the returned array, used the Copy
ownership rule for that array, and downcast elements before string conversion.
CoreFoundation's downcast retains each temporary string; normal drops balance
those references. A null bulk result took the original descriptor path,
including its descriptor-null early return. In-memory families were still appended
on the original success path. Public fallback additions, sorting, and final
deduplication were unchanged. The native parity test compared both algorithms on
the same collection without printing family names.

The initial roughly 22 ms bulk fixture omitted descriptor duplicate filtering.
The corrected fixture now uses the same duplicate-filter option as GPUI for both
algorithms. Actual production listing with those options took roughly 127-128 ms
versus roughly 116 ms for the original path, so the prototype was reverted.
The earlier result does not prove that removing descriptor filtering preserves
family attributes for every supported font installation. The visible-family API
was also rejected because it can omit hidden families. Production font behavior
remains unchanged; the comparison fixture remains available for research.

## Native measurement geometry guard

The final harness change has no material finding. `main` retains the first
successful trial's `window_before` observation and passes that same reference to
every later trial. The pre-capture check compares each focused window against it;
the existing post-capture check still compares that trial's before and after
observations. No code mutates the retained observation. A failed first trial
cannot become the reference because it never returns a result.

A geometry mismatch raises before resource samplers start and remains inside
the existing `try/finally`, so owned producer/application cleanup still executes.
The failure text and bounded failure-classification set were renamed together.
This closes cross-trial geometry acceptance for focused comparisons. Hidden
observations contain no visible bounds, so this change does not establish hidden
geometry parity. The coordinator replayed recorded observations and reported
zero rejections for the first batch and two for the mixed-geometry batch; this
review did not rerun that replay or a native capture.
