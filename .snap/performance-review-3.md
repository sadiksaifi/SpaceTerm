# Adversarial review 3

- PR: [#352](https://github.com/sadiksaifi/SpaceTerm/pull/352).
- Initial reviewed head: `acb6b193aec419ebbc9124e706517fc65fcd43a2`.
- Base: `134d7027b2014f89d29d0f9c4d087e894f33cfbc`.
- Independent agent: `pr_review_evidence`, GPT-6 Sol, high reasoning.
- Emphasis: measurement validity, workload parity and resource cleanup.
- Coverage: all 31 changed paths, affected production callers, tests, benchmark
  methods, research/results, PR metadata and existing feedback.
- Method: read-only snap-review behavior, change economy, tests and architecture
  lenses. The reviewer did not edit files, post comments or run build/native jobs.

## Initial finding

P2: `restored_history_preserves_find_and_selection` copied the full selection
before its post-compression Find. Copying restores cold pages. Its marker at
output row 950 was also near the viewport. The test could pass while Find over
compressed history regressed. No other material finding was reported.

## Resolution

Commit `959610391bf84e49262a8f8d86dde3729e9e4536` moves the marker to retained
output row 600. Find now runs immediately after the first compression pass.
The test then compresses again before checking full selection copying, so both
consumers exercise restoration independently.

`mise run test:one restored_history_preserves_find_and_selection` passed and
`mise run lint:rust` passed after the correction. Production code and the measured
application binary did not change.

The same independent reviewer verified the published fix at `9596103`. The marker
is explicitly proven retained, and the new Find generation forces corpus refresh
before selection copying. Native tracked selection pins do not prevent eligible
cold-page compression. The finding is resolved, with no new finding in the delta.

A native `Complete` result does not guarantee that a page compressed. Actual
compression benefit is supported separately by the native footprint fixture;
this behavioral test does not claim to measure compression savings.
