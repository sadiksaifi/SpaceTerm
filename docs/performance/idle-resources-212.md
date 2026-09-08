# Idle resource check for issue #212

Measured on 2026-09-08 with optimized source builds, one Pane at the shell prompt,
no terminal output or images, a large window on the built-in 3024 x 1964 Retina
screen. The baseline is `cc026c2`, including the cursor blink work from #211.
Both builds use the same window size and unchanged blink behavior.

| Observation | Baseline | Change |
| --- | --- | --- |
| Focused idle CPU, five two-second samples | 1.4% to 1.8% | 1.5% to 1.8% |
| Focused idle memory reported by `top` | 215 MB, flat | 213 MB, flat |
| Hidden idle CPU | 0.0% | 0.0% |
| Hidden idle memory reported by `top` | 112 MB, flat | 110 MB, flat |
| Hidden process context switches | 70 in 10 seconds | 29 in 30 seconds |

`footprint` reported 215 MB and 213 MB while focused. The first hidden captures
reported 112 MB and 116 MB respectively; the latter included 16 MB of reclaimable
graphics memory during transition. `top` subsequently settled at 110 MB for the
changed build. A subsequent five-minute hidden-idle run stayed at 110 MB, with
CPU samples from 0.0% to 0.1%; `footprint` at the end also reported 110 MB.
These observations are not a long-duration memory proof.

Five-second `sample` captures were collected for both visibility states on both
builds. The baseline hidden capture sampled a PTY hidden-input inspection. The
changed hidden capture showed the worker waiting, with no hidden-input inspection
or terminal drawing stack sampled. Sampling does not prove that no draw occurred.
Context switches are process-level observations, not a direct count of Pane wakeups.
The schedule regression test establishes one 30-second idle fallback, with one
200 ms follow-up after a transition, in place of persistent 200 ms polling.

Automated coverage exercises focus and PTY-output hidden-input transitions,
same-generation delivery without another submission, occlusion eviction and image
restoration without new output, zero GPU reservation for images without placements,
and quit waiting for cleanup of both live and already closing Remote Workspaces.
Existing native tests exercise process termination, reaping, and runtime artifacts.
The full validation suite passed, including portable checks, native Adapter tests, linting,
script checks, and terminal-library patch tests.

A generated Python `getpass` prompt hid fixture input and normal shell input
resumed afterward. After a 100,000-line warm-up, five writes of 100,000 identical
19-byte lines were timed in the foreground shell, including `stdout.flush()`:

- Baseline seconds: 0.0984, 0.0924, 0.0931, 0.0928, 0.0960; mean 0.0945.
- Changed seconds: 0.0940, 0.0933, 0.0939, 0.0940, 0.0947; mean 0.0940.

This small write-throughput check detected no slowdown. It does not measure
key-to-photon latency or completion of the final display frame.

Live remote-host close/quit, a longer idle soak, and quantitative typing latency
remain manual follow-up checks. Output batching, queue capacity, blink timing,
scrollback limits, and drawable size are unchanged.

The single adversarial review identified a connecting-process cleanup race and an
OpenSSH option compatibility failure. The fixes keep the quit barrier alive through
native spawn, reaping, diagnostic-reader completion, and runtime cleanup, and put
`IgnoreUnknown=ForkAfterAuthentication` before the corresponding option. A test
holds the actual portable process reaper pending after both cancellation and master
status failure and verifies quit cannot finish before runtime-owner removal.

For compatibility verification, an OpenSSH 8.2p1 client was built from the official
portable release outside the repository, without OpenSSL, with adjustments for the
current macOS compiler and its working `snprintf`. Its unchanged configuration
parser rejected the unguarded option and accepted the guarded master specification
using `-G`, without making a connection. The installed client also verified that
the command overrides a configured `ForkAfterAuthentication yes` to `no`.
The option was introduced in [OpenSSH 8.7](https://www.openssh.org/txt/release-8.7).
