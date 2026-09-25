# Final independent review

Date: 2026-09-26.

Verdict: No Findings.

This read-only review covers the production Find corpus encoder, its long-cluster regression, native SSH exit observation and Control Connection supervision, and the macOS application resource harness. It excludes this reviewer's GPUI frame-demand implementation. No code was changed and no builds or tests were run by this reviewer. The coordinator owns the final validation gate.

## Reviewed behavior

- `src/terminal/find.rs:341` preserves the previous UTF-8 bytes, byte-to-cell mappings, cluster head, wide-cell endpoint, and whole-cluster reservation while removing the temporary String. `src/terminal/emulator/tests.rs:162` exercises a cluster exceeding the initial scratch buffer and a match ending in a wide cell. The independent legacy fixture in `src/terminal/find/performance.rs` compares corpus bytes, mappings, capacities, and retained-row match counts.
- `src/platform/macos_ssh_process/exit_observation.rs:13` registers before checking collected status under the process owner's exclusive borrow. The observer owns its queue and cancellation endpoints but does not reap the child. Its interrupt is idempotent, wakes an existing or future wait, and releases descriptors when the waiter is dropped.
- `src/ssh/control_connection.rs:178` stops and interrupts the supervisor before joining it. `src/ssh/control_connection.rs:762` waits outside the child mutex, checks the stop flag before collecting status, and retains polling after unavailable, failed, interrupted, or premature observation. Shutdown rejection restores supervision. Drop interrupts before transferring cleanup ownership. Native and controlled tests cover these lifecycle branches.
- `scripts/measure-macos-application-resources.py:197` distinguishes focused, unfocused-visible, and hidden states and checks geometry. The helper owns one small native window and retains its lifetime pipe. It confirms its own activation and removes its startup timer before readiness. `scripts/measure-macos-application-resources.py:254` requires the helper to remain frontmost with exactly one on-screen window that does not overlap SpaceTerm. The parent checks this before and after capture and rejects changed SpaceTerm geometry. Floating targets only the benchmark process's single window.
- `scripts/measure-macos-application-resources.py:294` rejects unequal output or grid dimensions. Its DSR acceptance requires a complete bounded cursor reply after producer output; this proves terminal consumption, not presentation. `scripts/terminal-resource-workload.py:49` bounds acknowledgment waiting and restores terminal attributes and descriptor blocking on completion and failure. The harness tests reject malformed counters, wrong process memory data, unequal workloads, and invalid window states.

## Limits

The review establishes no additional performance result. Native scheduling, display behavior, and resource measurements remain the coordinator's recorded validation evidence. No external review was posted.

The final helper replacement and overlap checks were re-reviewed on 2026-09-26. Verdict remains No Findings. The helper now creates a small window because windowless activation failed on the tested macOS version. The reviewer ran no native applications, tests, or builds for this recheck.
