# Performance validation

## Combined checks

The original checkout was clean at `134d7027b2014f89d29d0f9c4d087e894f33cfbc`.
All changes belong to `perf/application-resources`.

| Command | Result |
| --- | --- |
| `mise run test` | Pass: 3,112 tests, zero failures, five ignored fixtures |
| `mise run lint:rust` | Pass after resolving three lint findings |
| `mise run build:release` | Pass after combined implementation |
| `mise run fmt:check` | Pass |
| `mise run lint:scripts` | Pass |
| `mise run lint:macos:scripts` | Pass |
| `git diff --check` | Pass |

The lint fixes introduced a type alias for prepared rows, used an array for
layout test variants, and retained the ignored release fixture's runtime mode
check through `black_box`. They do not change production behavior. The full
suite preceded those lint-only edits; Clippy compiled all targets and features
afterward.

## Behavioral evidence

- Cell snapshot coverage preserves combining sequences, wide-cell tails, emoji,
  and empty cells.
- Geometry coverage exercises each layout dependency, source changes, styles,
  clipping and eviction.
- Font coverage verifies the full ordered Settings catalog and classification
  before a newly selected family resolves.
- Compression coverage verifies idle deadlines, queued-input priority, bounded
  continuation, unsupported/error dormancy, full-history text, Find and selection.

Benchmark methods, scope and results are in the individual measurement records.
Valid application measurements cover focused idle and scrolling; hidden and
focused-history trials were excluded after native state verification failed.
Three independent PR reviews completed. One P2 test-coverage finding was corrected
and verified by its reviewer; see the review records. Native GPU time/residency and an exhaustive visual comparison are not
measured by these checks.

## Review correction

After the full suite, review identified that selection copying restored history
before the compression test's Find assertion. Commit `9596103` moves the marker
to older retained history, executes Find first after compression, and recompresses
before checking selection copying. The focused test and Rust lint passed again.
This test-only change leaves the measured production binary unchanged.

- [Terminal review](performance-review-1.md).
- [Appearance review](performance-review-2.md).
- [Evidence review and resolved finding](performance-review-3.md).
