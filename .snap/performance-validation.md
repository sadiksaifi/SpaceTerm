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
Three PR review reports will supplement this record. Native GPU time/residency and an exhaustive visual comparison are not
measured by these checks.
