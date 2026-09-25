# Event-driven Control Connection exit observation

Status: implementation and local resource fixture passed, 2026-09-26. The serialized native red gate
failed only the new collected-exit test: 202 tests passed, and the expected
failure was "native child exit observation should be available". The log is
`target/performance/continuation-remote-red.log`. The coordinator's initial green
runs passed 41 Control Connection tests and 206 native macOS tests. The optimized
resource fixture passed and measured the idle-watcher savings below. A subsequent
full gate must include the final failed-wait ordering and fixture lint cleanup.

Replace the ready Control Connection's 10 ms process-status polling with a
cancellable native exit observation. Keep the existing polling path when native
observation is unavailable or fails. Preserve OpenSSH authentication, live
authority, process-group cleanup, and shutdown timing.

## Existing ownership

[`spawn_supervisor`](../src/ssh/control_connection.rs:735) owns a thread per ready
Control Connection. It sleeps for 10 ms, locks the child, calls `try_wait`, and
publishes Failed and enqueues cleanup when the child exits. Its nominal 100
checks/second is source arithmetic, not a measured wakeup count.

The connection retains the child in `Arc<Mutex<Option<B::Child>>>`. The child
owns the process and bounded stderr reader.
[`SupervisedSshChild::enqueue_cleanup`](../src/ssh/process.rs:708) transfers those
resources once to its existing reaper. Native
[`try_status`](../src/platform/macos_ssh_process.rs:85) caches collected exit
status, and `reap` waits only if status was not collected. The new observer must
not become a competing reaper.

[`shutdown`](../src/ssh/control_connection.rs:435) stops ready supervision before
its graceful command and bounded exit phases. Rejection restores ready
supervision. [`Drop`](../src/ssh/control_connection.rs:632) requests stop and moves
joining into the cleanup callback. These lifetimes must remain intact. The
short-lived readiness, utility-command, and shutdown polls are outside this
change.

## Portable seam

Add an optional operation to the existing `SshProcessAdapter` and
`SshProcessBackend` interfaces:

```rust
fn observe_exit(&self, process: &mut Self::Process) -> Option<SshProcessExitObservation>;
// The backend uses &mut Self::Child instead.
```

The default returns `None`. Existing fake adapters, fake backends, and future
platforms keep current behavior without implementing native observation.
`SshProcessSupervisor` forwards to its retained adapter only while it retains a
process. No native PID, descriptor, signal, or native error crosses the seam.

`SshProcessExitObservation` owns one blocking waiter and an interrupt handle.
The waiter returns an exit hint, an interrupted outcome, or an existing typed
mechanism failure. The interrupt handle is cloneable through `Arc`, Send + Sync,
idempotent, and wakes a current or future wait without joining or taking the
child mutex. Concrete wait and interrupt traits avoid exposing native handles
or making the child cloneable. An already-exited process uses an immediately
ready waiter.

The waiter only reports that the owner should check status. Registration may
recheck and cache status through the exclusively borrowed process owner to
close the attachment race. The waiter never acquires reaping ownership. Failure to create
or wait on an observation is a performance fallback, not a new connection
failure or user-facing error. Do not add portable failure variants just for the
new native optimization.

## macOS mechanism and race handling

Use `kqueue` with `EVFILT_PROC` and `NOTE_EXIT`. Apple's
[kevent manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kevent.2.html)
defines process-exit notifications, and its
[CFFileDescriptor example](https://developer.apple.com/documentation/corefoundation/cffiledescriptor?language=objc)
demonstrates this process watcher. A dedicated existing supervisor thread can
block in `kevent`; no CoreFoundation run loop or additional helper thread is
needed.

Registration does not retroactively report every exit. Apple's
[XNU process filter](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_event.c#L1030-L1084)
attaches the filter and explicitly describes subsequent edge-triggered events.
That source also reports ESRCH when the PID cannot be found.

While holding the retained child's exclusive borrow:

1. If native cached exit status exists, return an immediate observation.
2. Create the native wait resources and register the process-exit event.
3. Immediately call the existing native `try_status` before returning the waiter.
   An already exited process returns an immediate observation and discards the
   unused native wait resources.
4. If process registration reports ESRCH, recheck `try_status`. Return immediate
   observation only for a confirmed exit; otherwise select polling fallback.
5. A successful registration followed by a still-running status covers future
   exits. A process that exited just before registration is covered by the
   post-registration status check.

Retained, unreaped child ownership prevents reassignment of the process identity
during setup. Do not signal an observed PID or infer successful exit from ESRCH.
The adapter must not reap from its event waiter. If a delivered exit hint is
followed by `try_wait == None`, select existing polling rather than assuming
failure or waiting again on an already consumed one-shot notification.

For interruption, register one endpoint of a private Unix socket pair in the
same kqueue. The interrupt owner shuts down the other endpoint's write side.
This makes EOF observable, including when stop precedes the blocking wait. No
write can block on pipe capacity. Shutdown applies to the socket rather than
one descriptor, so an accidentally inherited duplicate cannot suppress EOF.
An atomic stop flag remains the portable
arbiter of whether an observed wake means stop or exit. Do not close the kqueue
descriptor from another thread to interrupt it, and do not rely on arbitrary
descriptor reuse to wake a blocked syscall.

The implementation must close every descriptor on partial setup failure and
apply close-on-exec to native descriptors. `UnixStream::pair` supplies the socket
pair; retain each endpoint with an explicit native owner. Retry interrupted
shutdown/registration/wait calls, and keep native error values out of portable
errors and diagnostics. Tests must prove both stop-before-wait and
stop-during-wait complete while the child remains alive.

## Supervisor integration

Keep native setup outside the wait loop and keep the child mutex unlocked while
blocking. Store the supervisor's stop flag, interrupt handle, and join handle
with one explicit owner so every stop path uses the same operation.

The normal ready path waits for a native event, then performs the existing
status check, authority transition, child take, and cleanup enqueue in the same
order. It has no periodic timer. If observation is absent, wait fails, or an
event does not correspond to collectible status, discard that observation and
use the original 10 ms loop for this supervisor lifetime. Retrying native setup
on every iteration would create a new resource problem and is excluded.

Stop stores the flag before waking the waiter. A shutdown stop joins on its
existing background path. Connection Drop wakes the waiter and retains the
existing deferred join/cleanup path. After a rejected graceful shutdown, create
a fresh observation for the same retained child. Do not reuse an interrupted
one-shot observation. A queued exit racing stop must not overwrite Closed or
restore stale authority; retain the existing live-authority transition rules.

## Test proposal

Existing fakes can retain the default polling behavior. Add an explicit
event-driven fake backend for the new behavior. Its wait uses a condition or
channel, and test code releases it directly. Avoid timing-based sleeps for
portable assertions.

| Test | Observable invariant |
| --- | --- |
| Ready native observer remains blocked | No repeating `try_wait` calls or timer scheduling after setup |
| Explicit fake exit event | Failed lifecycle published; stale Pane and utility commands revoked; one cleanup transfer |
| Stop before wait and stop during wait | Supervisor finishes; child ownership remains with caller until existing cleanup |
| Observer unavailable, setup error, or wait error | Existing polling detects later exit; no premature Failed transition |
| Spurious exit hint with running child | Polling fallback remains live and eventually detects exit |
| Rejected graceful shutdown | Fresh observer installed and restored Ready connection still detects death |
| Concurrent Drop/exit/cleanup | No double reaping, no deadlock, no late authority restoration |

Reuse `master_death_should_invalidate_stale_pane_and_utility_commands` and
existing shutdown/Drop tests as regression coverage. Extend native tests under
`macos-native-tests` for real exit-before-registration, exit-after-registration,
stop-before-wait, stop-during-wait, and descriptor cleanup. Verify the existing
native `try_status` still collects exit status after observation; this proves
the observer did not consume the child. Exercise process-group descendant
cleanup through the existing native cleanup tests.

## Credential-free measurements

Use a local child launched by `MacOsSshProcessAdapter` with cleared environment,
private temporary working directory, and explicit `/bin/sh -c` code that blocks
on its piped stdin. Keep the child input pipe open until the fixture releases
it. This needs no SSH server, network connection, user credentials, login-shell
configuration, or user SSH files.

Compare the existing supervisor poll and the candidate event wait around the
same local child. Include 1, 4, and 16 observers; sample process CPU, interrupt
wakeups, footprint, threads, and descriptor counts for a fixed idle interval.
Release the child through its input pipe and measure elapsed time to observed
failure separately. Repeat stop/restart and create/drop cycles and record
settled resource counts. Use optimized builds and serialize build/capture work
through the coordinating agent and `mise run` tasks.

For integration, reuse the real private-socket native test harness with a
controlled backend that starts this native local child and supplies readiness
results. Exercise the actual `OpenSshControlConnection` lifecycle and adapter
without pretending a local fixture is an authenticated SSH connection.
Microbenchmarks establish waiter costs; they do not establish whole-application
remote resource savings or ordinary network-disconnection latency.

Require equal exit delivery, unchanged bounded shutdown behavior, and no
resource growth. Compare median and tail exit-observation latency against the
10 ms poll; there is no hard 10 ms scheduling guarantee in the existing code.
Report fallback captures separately from successful native-observer captures.

## Prepared resource fixture

The ignored native test `macos_ssh_exit_observer_resources` starts 1, 4, and 16
local `/bin/sh` children blocked on a shell builtin reading retained piped input.
It exercises the actual native observation owner and compares it with 10 ms
`try_status` polling. Each mode runs for 3 seconds after every observer has
registered and every worker has acknowledged readiness. Two repetitions reverse
mode order, giving 36 seconds of measured intervals. Setup and teardown are
outside each CPU and interrupt-wakeup interval.

The test self-samples `proc_pid_rusage` V0 and applies `mach_timebase_info` using
the existing macOS resource sampler's CPU calculation. Output contains only
mode, child count, repetition, elapsed seconds, CPU percentage, interrupt
wakeups/second, physical footprint, and setup/interval footprint deltas. No PIDs,
paths, commands, or child contents are printed. Process-wide footprint includes
allocator and test-harness effects; alternating order limits but does not remove
those effects. This is a watcher microbenchmark, not an authenticated remote
Workspace or whole-application claim.

The test closes interrupt endpoints and joins every worker before returning.
Workers retain ownership for killing and reaping their local children, including
failed waits. Native setup or unexpected child exit fails the fixture rather
than silently measuring fallback. The coordinator must run this optimized test
alone with `macos-native-tests`, `--ignored --nocapture --test-threads=1` through
a dedicated macOS mise task.

## Measured local watcher result

`mise run bench:macos:exit-observation` passed with the release test features
`macos-native-tests,gpui/inspector`. The first task attempt omitted the inspector
feature required by existing release tests and failed compilation; it produced
no measurement. The corrected log is
`target/performance/continuation-remote-resources-corrected.log`.

Median of two 3-second intervals per mode, reversing mode order:

| Local children | Poll CPU % | Event CPU % | Poll interrupt wakes/s | Event interrupt wakes/s |
| --- | ---: | ---: | ---: | ---: |
| 1 | 0.1169 | 0.0014 | 74.85 | 0.333 |
| 4 | 0.4015 | 0.0020 | 331.74 | 0.333 |
| 16 | 1.7178 | 0.0007 | 1333.70 | 0.333 |

CPU percentage uses one core as 100%. The event mode records one process
interrupt wake per measurement interval, consistent with the sampler's own
3-second sleep. Every interval reported zero physical-footprint change. Setup
footprint and total process footprint varied with worker count and allocator
history; these short samples do not establish a memory reduction. Successful
native observation removes the regular ready-watcher timer in this fixture.
Unsupported or failed native observation deliberately retains the old poll.

Initial correctness logs are
`target/performance/continuation-remote-control-green.log` (41 passed) and
`target/performance/continuation-remote-native-green.log` (206 passed, 2 ignored).
They cover command revocation, fallback after failed/spurious observation,
rejected shutdown restoration, Drop cancellation, exit registration races,
stop-before-wait, stop-during-wait, and retained exit-status collection.

After the completed resource run, a function-scoped `expect(deprecated)` was
added for libc's Mach timebase binding in the ignored fixture. Its rationale is
to reuse the existing native ABI without a production dependency. Production
behavior and measurement calculations are unchanged. Formatting and diff checks
passed; the coordinator owns the remaining final compile and full test gate.

## Intended source ownership

- `src/ssh/process.rs`: optional observation seam, owned waiter/interrupt types,
  and retained-adapter delegation.
- `src/ssh/control_connection.rs`: supervisor ownership, wait selection, and
  portable lifecycle tests.
- `src/platform/macos_ssh_process.rs`: native capability implementation.
- `src/platform/macos_ssh_process/exit_observation.rs`: private native wait
  mechanism, setup cleanup, and native tests.

[ADR 0002](../docs/adr/0002-keep-product-policy-portable.md) keeps process facts
native and lifecycle portable. [ADR 0003](../docs/adr/0003-let-openssh-own-remote-authentication.md)
keeps authentication with OpenSSH. This proposal changes neither decision.
