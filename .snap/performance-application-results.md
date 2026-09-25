# Source application resource comparison

## Conditions

- Baseline source revision `134d702`, binary SHA-256 `7ff71424938188adc4625c06699c29ac803c34d69c87145e16347cccee171eaa`.
- Candidate source revision `3e82d49`, binary SHA-256 `0bc96b06d7994ac00b64971501cb7b6578ea05ea0d2acf6acf16f815682cdbf3`.
- macOS 27.0 (26A428), Mac16,8, Apple M4 Pro, 24 GiB memory. Display scale and refresh rate were not recorded.
- Each binary ran from a temporary signed source bundle. Each fresh trial had empty, private XDG configuration, data, state, cache, and runtime directories. `HOME` and existing SpaceTerm settings were untouched.
- Each scenario used two baseline and two candidate processes in baseline/candidate/candidate/baseline order. After five seconds of warm-up, the sampler captured ten seconds at 0.5-second intervals. The producer finished after capture, so its completion summary did not enter the sample.
- Native checks observed the owned application frontmost with one on-screen 1472 x 937-point window before and after each accepted capture. The bounds were stable within and across trials. These checks do not establish the terminal grid, intermediate focus, display scale, or first-frame time.

## Valid foreground captures

Figures are the range of two process-level captures. CPU uses one core as 100%; wakeups are interrupt wakeups, not rendered frames. Footprint and resident memory are separate `proc_pid_rusage` counters, neither a GPU memory measurement.

| Workload | Process | App CPU | App footprint | App resident | App interrupt wakeups/s |
| --- | --- | ---: | ---: | ---: | ---: |
| Focused idle | Baseline | 1.00-1.03% | 171.31-171.40 MiB | 272.60-272.71 MiB | 120.57-120.57 |
| Focused idle | Candidate | 0.87-0.90% | 123.45-124.46 MiB | 115.78-118.05 MiB | 120.55-120.65 |
| Focused scroll | Baseline | 34.34-35.14% | 277.14-277.35 MiB | 275.77-276.84 MiB | 173.71-179.11 |
| Focused scroll | Candidate | 34.33-34.74% | 227.27-230.90 MiB | 121.51-123.10 MiB | 175.56-178.81 |

The candidate's application footprint was 46.85-47.95 MiB lower during idle and 46.45-49.86 MiB lower during scrolling in the paired repetitions. Idle CPU was lower by about 0.13 percentage points. Scrolling CPU and interrupt wakeup ranges overlap; these captures do not establish an improvement in either metric. Package wakeups varied across repetitions and do not establish power use.

All idle producers emitted exactly 40 lines and 4,087 bytes in 20.00-20.01 seconds; their CPU and wakeups were zero during capture. All scroll producers completed 1,200 updates, 9,640 lines, and 1,050,487 bytes in 20.00-20.01 seconds. Producer CPU was 0.23-0.34% for baseline and 0.34% for candidate, with about 60 interrupt wakeups/s for both. The measured application process excludes the producer; the table keeps their costs separate.

The producer's PID marker appeared 0.809-1.035 seconds after baseline launch and 0.537-0.764 seconds after candidate launch for idle; scrolling ranges were 0.759-1.033 and 0.492-0.857 seconds. The candidate was 0.176-0.272 seconds earlier within the four paired repetitions. This interval includes application launch and Python fixture startup and is detected with a 50 ms polling loop. It is neither a first-frame measurement nor shell readiness for a normal login shell. Operating-system and font caches can affect it.

The application footprint difference applies to this default-font, fresh-settings state. The candidate defers full native font catalog classification until Settings first opens, so this measurement does not establish settled memory after that interaction. These captures compare all selected changes together and do not attribute the footprint difference to one change. The idle fixture has only one screen of output; the scrolling fixture never settles. Neither provides an application-level measure of idle Scrollback compression.

## Invalid or unavailable states

- Hidden scrolling did not enter sampling. The owned baseline was alive, fully launched, frontmost, and had one visible window, but macOS `NSRunningApplication.hide()` returned false, including when called from a fresh helper process. No hidden-state resource claim follows.
- Settled history emitted the fixed 10,000 lines and reached its readiness marker. In two attempts, the owned baseline remained alive and visible but lost frontmost status during the ten-second capture. Those incomplete runs were excluded, so no application-level settled-history comparison follows.
- Native GPU residency, GPU execution, watts, first native frame, first terminal frame, exact grid dimensions, unfocused-but-visible state, Pane scaling, graphics lifecycle, and lifetime leak behavior were not measured in this matrix.

The first automation attempt reused an `NSWorkspace` object in one Python process and read stale frontmost state after switching between application processes. The accepted runs query state in a fresh helper process each time. Apple's [NSRunningApplication documentation](https://developer.apple.com/documentation/appkit/nsrunningapplication?language=objc) says time-varying properties can remain cached until the next main-runloop turn. Raw accepted capture records are in `target/performance/application-idle.jsonl` and `target/performance/application-scroll.jsonl`; that target directory is local build output.
