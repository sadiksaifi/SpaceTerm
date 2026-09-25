# Artifact supervisor validation repair

Final validation exposed a supervision failure: `du -sk` can return status 1
while Cargo removes temporary files during traversal. The supervisor raised
`SystemExit(2)` inside its monitor and left its owned Cargo process group alive.
Available disk space was not the cause. The parent observed 71 GiB free.

The repair permits three complete measurement attempts, separated by 100 ms.
A failed traversal's partial total never authorizes cleanup. Persistent failure
still reports the content-free measurement error and exits with status 2.
The supervisor retains its active group through a `finally` cleanup boundary,
which sends TERM, escalates to KILL when needed, and reaps the leader. It reports
unverified termination rather than claiming success. A signal received during
cleanup retains the existing `128 + signal` exit convention.

The regression fixture starts the actual supervisor with a private `du` adapter
and temporary owned target. Two transient failures emit a deliberately oversized
partial total; recovery must preserve the guarded command's status 37 and its
artifact. Persistent failure must stop both the TERM-ignoring leader and its
descendant, preserve the artifact, and exit with status 2. The fixture has its own
retained-group cleanup so a failing test cannot leave its children running.

Validation:

- Red: `mise run artifacts:test` failed with `transient: status 2; persistent:
  owned process survived` before the repair.
- Green: `mise run artifacts:test` passed with the repair and after the final
  signal-status adjustment. Existing budget, signal, ownership, child-status,
  descendant, cleanup-failure, and executable-selection cases also passed.
- `mise run lint:scripts` passed.

This is validation infrastructure, not an application performance optimization.
No Rust builds or native performance captures were run for this repair.
