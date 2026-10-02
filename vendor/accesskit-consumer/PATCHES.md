# AccessKit caret geometry

Source: crates.io `accesskit_consumer` 0.38.0, upstream revision
`c88605b96d04431f9c3c792464a0f2f253480e94`, directory `consumer`.
The original crate archive SHA-256 is
`5d10a236f96f87d70732e44520046785431ef01d5bcd6b041317bfadd2f88245`.
The license files are from that exact upstream revision.
Registry bookkeeping and the standalone lockfile are omitted.

`TextRange::bounding_boxes` uses optional text caret geometry only for a
degenerate range at the current text selection focus. The text selection owner's
transform and ancestor transforms apply. Other positions and all non-degenerate
ranges retain character-derived geometry. Unset behavior is unchanged.

Regression tests construct real consumer trees and cover a caret beyond nonempty
text, other text positions, exact character bounds, ancestor transforms, missing
character geometry, and the absence of a text selection. The standalone manifest
uses the neighboring pinned core source so it tests the same additive API.

Word navigation uses the optional `word_starts_u32` property when present.
Current-boundary checks, forward and reverse searches, and movement through
neighboring runs compare complete character indices without byte truncation.
When absent, the existing `word_starts` behavior is unchanged. An explicitly
empty wide list remains authoritative.

Four public consumer-tree regressions cover more than 255 leading spaces,
conflicting existing boundaries, oversized grapheme splits, forward/reverse
movement through soft-wrapped runs, and unset versus explicitly empty properties.

Retained README line endings and original manifest trailing blank lines are normalized for the repository whitespace gate; source behavior is unchanged.
