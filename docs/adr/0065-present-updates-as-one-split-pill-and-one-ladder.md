# Present updates as one split pill and one ladder

SpaceTerm Core restarts for an update only under the No Work Going On checks and accepted limits below. A waiting update gets harder to ignore as it ages. One Update Ladder covers SpaceTerm and every SpaceTerm Core: Offer from the download, Ask after two days, Insist after seven. The title bar holds one split pill or nothing. Its main area shows the Update Step, and its chevron always opens the same list of SpaceTerm and every Machine. From the Ask stage SpaceTerm on macOS relaunches into its update after an hour without use. SpaceTerm Core restarts once every Terminal Session has had no work for an hour, under the native freeze and final checks below. A breaking SpaceTerm Core update always asks.

[ADR 0040](0040-install-core-versions-from-the-feed-a-push-or-a-local-archive.md) owns how a Pending Version arrives, and [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md) owns the restart into it. This record owns how every update is presented, when each presentation escalates, and when an update applies itself without a click.

## Rules

1. **The title bar holds one split pill or nothing.** The main area is the next step the person can take, a failure SpaceTerm will not retry by itself, or progress on a download, relaunch or restart the person started on this desktop. The chevron always opens the same list. The pill never counts, never changes shape while visible, and never changes because of work nobody on this desktop started.
2. **SpaceTerm never ends a Terminal Session that has work going on in order to update.** No stage restarts SpaceTerm Core over a running program, a paused or background job, or an unsent command line.
3. **The longer an update waits, the harder it is to ignore.** Presence, emphasis and reminder cadence rise on one ladder for SpaceTerm and every SpaceTerm Core. Texts never get longer as a stage rises.
4. **One term per state and per action, written by one module.** An action names the software it changes, never the computer. A label runs the same command on every surface.
5. **Each fact has one unsolicited surface.** The title bar is that surface. The title-bar list, Settings > Updates, the Machine card and the menu bar extra render the same rows when the person opens them. The Pane and the sidebar do not repeat restart progress, a failure appears once, and SpaceTerm keeps no list of recent updates.
6. **Say only what the facts prove.** No text calls a Machine up to date, promises a restart the facts cannot prove, promises anything from an offline Machine's last facts, says Terminal Sessions ended after a timeout, or claims an age for a version whose clock it inherited.
7. **Rules live in this record, words in one copy module, and every update change ships with the replayed journey and a live rehearsal.** No ADR quotes a person-facing update string, and no string snapshot is the only test of a behavior.

## The split pill

The pill is a capsule at the trailing edge of the title bar, on macOS and on the Linux client-drawn title bar. It has two segments: the main area, a glyph and a label, and the chevron. It is hidden when there is no Update Step. Whenever it is visible it has both segments, in every state.

The main area shows the first Update Step in this priority order:

1. A failure SpaceTerm will not retry by itself and that this desktop has not dismissed.
2. A restart started on this desktop that got no answer within the 180-second restart deadline.
3. A restart the person approved on this desktop, while it runs.
4. A download the person started on this desktop, while it runs, determinate when the total is known.
5. A relaunch the person chose, until SpaceTerm quits.
6. An action step, ordered first by stage (a hard stop, then Insist, Ask and Offer) and then by kind: SpaceTerm's relaunch into a downloaded update, then Restart Core for a SpaceTerm Core update that is owed, then SpaceTerm's download when automatic downloads are off or a download failure was dismissed on this desktop. A SpaceTerm Core update is owed when it is breaking (from Offer), when it blocks a Machine's Workspaces (at once), or when it would apply itself (from Ask). The step names the first such Machine: the local Machine first, then store order.
7. The release notes, on the first run of a newer SpaceTerm, until the notes open from anywhere or SpaceTerm quits. They never return on a later launch.

Progress and the release notes are plain. An action step is accent at Offer and Ask, and warning at Insist and for a hard stop. The relaunch runs at once with no confirmation under [ADR 0049](0049-own-status-controls-in-the-desktop-client.md). Restart Core opens its confirmation. A failure step opens that failure's alert. The main area's label never carries a count or a Machine name.

The chevron opens the list in every visible state. The list holds the SpaceTerm row, a SpaceTerm Core heading with one row per Machine (the local Machine first, then store order), and an entry that opens Settings > Updates. Choosing a row performs its action, and a row without an action is informational. The list holds no count, no action that updates several Machines, and no recent updates.

The pill keeps its shape:

- Visible always means the main area and the chevron. No background event turns it into anything else.
- While the list is open, its rows keep their number and order and only their text changes. The pill stays visible until the list closes, even when its step resolved meanwhile, and then hides.
- The main area changes only for the person's act, for progress or the end of work the person started on this desktop, or for a new need: a breaking or blocking SpaceTerm Core version arriving, a stage reached, or a failure that needs the person. An automatic download, push or check, a silent restart, a restart another device started, and a failure that retries by itself never change it.
- Offline or stale facts never produce a SpaceTerm Core step.
- Several needs show one at a time in the main area. The list shows all of them.

A confirmation covers one Machine. An alert opens without a click on the pill only when the Machine refuses the click's preview at once, because the person is waiting on that click. Every later outcome appears once, as a failure step, and its alert opens on click. Dismissing a failure's alert dismisses that failure's title-bar presentation on this desktop. The title bar then shows the next update action still needed, including downloading an available release after a failed download. It hides only when no Update Step remains. The row keeps the failure until newer facts supersede it. Dismissal never changes whether an update applies itself.

[state.rs](../../src/updates/state.rs) is the only place that decides the Update Step, and it derives the step and the rows from the same stages.

## The Update Ladder

### The clock

Every stage counts how long the app or a Machine has had an update waiting, from the first version it has not applied. A newer version that replaces a waiting one inherits the clock, and a restart or relaunch into the update resets it.

- SpaceTerm Core: `PendingVersion.installed_at`, which every released Core sends and which a replacing version keeps. The desktop reads it against its own clock, and the Core reads it against its own clock for the idle rule. The desktop ignores `overdue_since`, so every Core follows one desktop clock.
- SpaceTerm: `UpdateHistory.published_at`, which keeps the earliest publication time across replacements and clears once the installed version passes the first one observed.

A newer version can therefore appear with a step only on a Machine that has already waited, and texts say that an update has waited, never that a version is old. A per-version clock was rejected: releases a day apart would never reach the first threshold. Neither clock needs a wire or retained-format change. The desktop stores the stages in the three existing `UpdateStage` values, so `update-history.json` keeps its format.

### The stages

| Stage | From | Title bar | Emphasis | Update Reminders | Cancel in the Restart Core confirmation | Applies by itself |
|---|---|---|---|---|---|---|
| Offer | The download | SpaceTerm's relaunch or download; Restart Core only for a breaking or blocking SpaceTerm Core update. A SpaceTerm Core update that applies itself shows only in the list. | Accent | None | Closes the alert; the step stays | SpaceTerm Core with no Terminal Session at all, under ADR 0034 |
| Ask | 2 days | Every waiting update, including a SpaceTerm Core update that applies itself | Accent | One a day | Named for what it does, and hides that Machine's step and its reminders on this desktop for 24 hours | Also when no work is going on for an hour: SpaceTerm on macOS relaunches. SpaceTerm Core restarts under the native freeze and final checks below. Never a breaking SpaceTerm Core update |
| Insist | 7 days | The same steps | Warning; the menu bar extra's icon shows a dot | One every 4 hours | Closes the alert; the step stays | As in Ask |

Each stage adds presence, emphasis or cadence, never words: a step's label is the same text at every stage. The last stage is Insist. No stage ends a Terminal Session that has work going on, and no stage holds a launch.

The 24-hour deferral is desktop memory for one run. It hides only an Ask step and never an Insist or hard-stop step.

Two hard stops sit above the ladder, with no clock:

- **A protocol mismatch** under [ADR 0035](0035-negotiate-protocol-versions-per-connection.md). A SpaceTerm Core too old for this SpaceTerm, or a SpaceTerm too old for a Core, blocks that Machine's Workspaces. Its step appears at once in warning emphasis, Cancel does not hide it, and the blocked Workspace surface explains the block and offers the same step.
- **The Update Floor** of [ADR 0064](0064-step-updates-through-update-floors-and-migrate-data-generations.md). It is a path, not a stage: a Machine below the floor first takes a Stepping Stone, which its Core fetches by itself, and each Stepping Stone climbs the ladder with a fresh clock after its restart.

### Update Reminders

[ADR 0063](0063-own-notification-authorization-in-the-desktop.md) owns who posts. An Update Reminder posts at its stage's cadence, for SpaceTerm and every SpaceTerm Core alike: daily from Ask, every four hours from Insist. It posts only while no SpaceTerm window is active, because an active window's title bar already shows the step. A reminder that comes due while a window is active posts once SpaceTerm has had no active window for a moment. A SpaceTerm Core reminder also waits while that Machine's 24-hour deferral holds. Each new reminder replaces the previous one for the same update. A click performs the step: SpaceTerm relaunches, and a SpaceTerm Core reminder opens the Restart Core confirmation.

### Parameters

[ladder.rs](../../src/updates/ladder.rs) owns the desktop's parameters as constants. [The update engine](../../crates/spaceterm-core/src/service/runtime/updates.rs) holds its own Ask threshold and idle period for the SpaceTerm Core idle restart, with the same values. The owner tunes them there without changing this design:

| Parameter | Value |
|---|---|
| Ask after | 2 days |
| Insist after | 7 days |
| Ask cadence | 1 day |
| Insist cadence | 4 hours |
| Ask deferral | 24 hours |
| Idle period | 1 hour |

## No Work Going On

A Terminal Session has no work going on when all four conditions hold:

1. **Its shell owns the foreground.** The shell's process group holds the PTY foreground, so no editor, SSH, nested multiplexer, REPL or running command is in front.
2. **The shell waits at an empty prompt.** Its shell-integration markers show the shell's own prompt with an empty command line. A shell without markers always counts as work.
3. **Nothing else runs in its terminal session.** No process besides the shell shares the PTY's session: no background job, stopped job or other child.
4. **Nothing happened for the idle period.** No input reached it and it printed no output.

Attached clients are not a condition: a window showing a quiet prompt is not work, and the input clock covers typing. A Terminal Session whose foreground, processes or markers SpaceTerm Core cannot read counts as work, so a platform without reliable detection keeps the rule of ADR 0034 alone.

SpaceTerm Core restarts into a Pending Version by itself under this rule only when all of these hold:

- The update has reached the Ask stage.
- The Pending Version is not breaking.
- Every live Terminal Session on the Machine has had no work going on for the idle period at the same time.
- The engine has not tried this version silently in this boot. A start that fails does not loop.

[The update engine](../../crates/spaceterm-core/src/service/runtime/updates.rs) checks the rule again under the membership fence of ADR 0034. A Machine with no Terminal Session keeps the 10-second rule of ADR 0034 from the download.

### What this release does

This release enables the SpaceTerm Core idle restart on macOS and Linux from Ask by `PendingVersion.installed_at`, only for a nonbreaking Pending Version and at most once per version and boot. Every live Terminal Session must meet the four No Work Going On conditions. A helper that aborts before Stop leaves the update pending and can retry after another eligible hour. The 1.2 detailed facts keep their shape and report `WhenNothingRuns` and `RestartCause::NothingRunning`. The watcher computes work counts only for a nonbreaking Ask update with no busy update operation.

Input admission and native PTY reads stamp receipt generations. Under the panes lock, admission records every generation before scanning every live session once. A changed generation refuses the scan. The resulting revocable lease binds those generations, exact membership, the candidate and the Core boot. Input or output before grant revokes it and continues normally.

The idle scan records each process's CPU time and binds it to the lease. A late work-count refresh cannot replace that observation. The frozen check refuses any measured CPU advance, using a zero tolerance; kernel-accounting changes also count as work. macOS converts Mach CPU ticks through the kernel timebase and reads every thread's run state. Linux requires the shell to be blocked waiting to read the terminal, using its syscall and the actual terminal descriptor or read-readiness descriptor set. Both hosts also verify that the original shell executable image is still running. An unreadable counter, wait state or image refuses restart.

At grant, under the same lock as input admission, the Core records the slave device node's exact mode and owner, removes all permissions on it, sends SIGSTOP to each process in each live session's SID and waits for acknowledgement from that exact process instance. Linux uses pidfds and macOS uses PID-version audit tokens for signals, including thaw. Against that frozen population it reads fresh foreground, full SID membership, every process holding the slave open and pending master output with FIONREAD. An outside holder, another SID member, pending output, unreadability or an incomplete enumeration refuses the grant. No outside process is stopped. The permission fence stays closed through Stop and is checked again before Stop. Refusal, error, timeout and Core shutdown without Stop restore the exact device mode and owner and thaw exactly the processes this attempt stopped; a process already stopped before the attempt is never continued by it. Successful freezes remain owned until terminal teardown ends those processes before closing the PTY masters.

Frozen-process inspections run off the input-admission thread. An independent 250 ms deadline owner restores the device permissions, thaws the exact stopped processes and invalidates the attempt even if an inspector is stalled. A late inspection answer cannot grant Stop. Linux holder enumeration reads descriptor links without following their targets into filesystems.

Work/service 1.3 keep the final-grant exchange and typed restarting refusal. After grant, input is refused and kept by the client. Immediately before native Stop, the helper repeats the same grant request. The Core rechecks the lease, stopped process instances, SID membership, foreground, slave holders and pending output. Resumption or a new SID member refuses Stop, records work and thaws the attempt. The helper writes the existing StopDispatched marker before this recheck, so recovery cannot dispatch a Stop that skipped it. A refused recheck removes that marker without calling Stop. An interrupted helper leaves the existing uncertain-dispatch recovery case. No wire or retained-file field changes.

Once the helper and service ownership locks and the journal prove that no further Stop can be dispatched, the Core thaws its exact native lease even if ledger settlement cannot be written. This proof uses read-only descriptors. Existing input resumes, while the unresolved operation and its membership fence remain until settlement can persist.

The owner accepts three limits at the restart boundary. First, a process outside the session can receive the terminal's descriptor over a Unix socket after the descriptor scan. The Core does not stop or end that process. It keeps running, but its writes to the old terminal fail after Stop closes that terminal. Descriptor references still queued in a socket are absent from process FD tables. Second, another program of the same user deliberately interfering within milliseconds, or a privileged outside process, can undo the freeze or permission fence after the final check and start work or write before Stop. Before Stop, the Core refuses any observed resumption, new SID member, changed slave mode or owner, or pending output; it restores permissions, thaws the exact population and leaves the update pending. Third, a shell trap can be waiting silently inside a builtin while the recorded prompt still looks empty. When the CPU counter and terminal I/O do not reveal the trap, the Core cannot distinguish that wait from the idle prompt. The update can end that wait. The CPU and native wait checks reject observable computation; they cannot prove which shell function owns an otherwise identical blocked read.

Client channel removal transfers all remaining original input to retained ownership before release or detach. Explicitly refused input remains distinct from delivery-unknown input. Neither is replayed automatically. The existing CoreLink accessors transfer both categories to the client for person-controlled recovery.

An idle restart ends the shell process with its environment, unsaved history, screen and Scrollback. Workspaces, Tabs, Panes and layouts stay, and the new shell starts in the Pane's last confirmed directory under [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md) when the person starts it under [ADR 0043](0043-start-restored-shells-on-first-use.md). The Pane names the idle restart as the cause.

SpaceTerm itself has no work going on when it has had no keyboard or pointer input for the idle period, no alert, sheet or menu is open, and Settings are saved. Its relaunch from the Ask stage follows ADR 0049 and ends no Terminal Session. SpaceTerm relaunches by itself only on macOS, where it observes every keyboard, input-method and pointer event before a window handles it. SpaceTerm on Linux never relaunches by itself, because it cannot observe all input there, and neither does SpaceTerm on any other platform without such an observer. Its Update Reminders and Ask step still apply.

## How a Pending Version finishes

[continuation.rs](../../crates/spaceterm-core/src/update/continuation.rs) derives how a Pending Version finishes from the update facts every released SpaceTerm Core sends. The desktop and the CLI call the same function, so every row and every CLI notice promises the same thing. In order:

1. No Pending Version: nothing to finish.
2. A breaking Pending Version restarts only on confirmation.
3. The latest failure is a start failure of the Pending Version: the Core does not retry it by itself in this boot.
4. The latest failure is an install failure: unknown, because an install failure replaces the record of an earlier start failure.
5. No live Terminal Session: it restarts within the grace period of ADR 0034.
6. Otherwise it restarts once no Terminal Session is live.

The derivation never promises the idle restart, because the update facts cannot show it. SpaceTerm Core at work and service minor 2 of ADR 0035 reports how its Pending Version finishes in the detailed update facts, and that report takes precedence over the derivation. The engine computes it from its private attempt and retry state, and from its idle state. Any later change to when SpaceTerm Core restarts by itself that the update facts cannot show must be reported the same way, and its conformance test must show that the engine's silent admission agrees with the report.

## Who shows what

- **Title bar.** The split pill above. It is the one unsolicited surface for every update fact.
- **Title-bar list, Settings > Updates, Settings > Machines card, menu bar extra and Linux status item.** One row model serves all four. Each row carries the same status and the same action on every surface, each action runs one command on every surface, and a download never relaunches. Settings, the card and the menu bar extra also show each row's running version. A settled Machine shows only its version, because the desktop cannot know what its feed offers.
- **Settings > Updates.** Lists SpaceTerm and every Machine's row under [ADR 0039](0039-scope-settings-to-the-client-or-selected-server.md).
- **Settings > Machines card.** Its primary button is the row's action. The same-version restart appears in the card's more menu only while no Pending Version waits, because a restart moves into a waiting Pending Version under ADR 0034.
- **Menu bar extra and Linux status item.** SpaceTerm's step, then a SpaceTerm Core section with one entry per Machine whose submenu starts with the row's action. From Insist the icon shows a dot. Where a flat menu cannot show the Machine as a heading, the action names SpaceTerm Core and the Machine.
- **Sidebar.** No update text, progress or action.
- **Pane.** During an announced restart, until the 180-second restart deadline, the Pane keeps its frozen screen with input disabled and shows no notice under [ADR 0045](0045-freeze-terminal-screens-while-reconnecting.md). When SpaceTerm Core returns, a Pane whose Terminal Session ended says so and names one cause: a restart the person approved, an idle restart, a same-version restart, or a start that failed. A Pane that had no Terminal Session keeps ADR 0043's first-use behavior.
- **Update Reminders.** The ladder's cadence above.
- **CLI.** Command names and JSON output stay as [ADR 0055](0055-define-the-cli-output-contract-and-scope-model.md) defines them. Its notice calls the same derivation and reads the same ladder.
- **Blocked Workspace surface.** The launch screen for the local Machine and the Pane failure notice for a paired Machine explain a protocol mismatch and offer the same step as the pill.
- **Quit warning and About.** Unchanged.

## Words

ADRs state what a text must carry: the software it changes, the Machine, the versions, what ends and what stays. [The copy module](../../src/updates/copy/) holds every person-facing update string of the desktop, and its [copy deck](../../src/updates/copy/deck.txt) lists every string for every state, stage and surface. A wording change is one reviewed diff of the deck. The copy lint refuses the terms CONTEXT.md marks to avoid, and the CLI's update text runs through the same lint. Only this record and ADR 0034 name actions, and only as concepts: Restart Core for SpaceTerm Core, Relaunch for SpaceTerm.

## Release rehearsal

Before any release that changes [state.rs](../../src/updates/state.rs), the copy module, the continuation derivation, the ladder, the Machine update store, the Pane restart path or the engine's restart rules, a person runs the recorded journey live:

1. Start from the previous published release on This Mac with 2 open Terminal Sessions, and on Penguin with none. Penguin runs over `ssh penguin` and its desktop through Remora.
2. Start `mise run release:rehearsal:macos <folder>` while the previous release's window is open. The task must see that window first, because it starts capturing only when a window of a new SpaceTerm process appears.
3. Update to the candidate and relaunch into it. The task captures the title bar's trailing corner once per second for 60 seconds from the relaunched window's first appearance.
4. Open the chevron during the capture.
5. Approve one local restart.

The strip and the expected timeline of the recorded-journey replay go on the release PR. A mismatch blocks publication. When a candidate cannot reach a Release-identity desktop before publication, the SpaceTerm Core half runs with two Preflight builds, because Preflight Cores take pushes under ADR 0040, and the real published update runs on both hosts before the release is announced.

## The owner's decisions of 2026-11-11

The owner tried each option in an interactive mock and decided:

1. A waiting SpaceTerm Core update reaches the title bar after two days. It is the Ask stage, for SpaceTerm and every SpaceTerm Core, on the inherited clock.
2. After a relaunch, the pill offers the release notes for that one run.
3. The title-bar control is the split pill: the step plus a chevron that opens the list, one shape whenever visible, hidden when nothing is owed.
4. The action that updates every Machine at once and the list of recent updates are removed. A confirmation covers one Machine.
5. The SpaceTerm Core action is Restart Core on every surface.
6. The update philosophy, in the owner's words: "I wanted to have forced updates, but that doesn't mean that will kill their work. It means it will keep annoying the user. Gradually, the more they ignore the updates, the more annoying we become for them, so that eventually they do update. We never kill their session or something automatically unless it's idle. We don't want someone's work to be lost, but we also don't want someone to keep using the old or stale software. … This applies to everything: client, server, core." Rules 2 and 3, the ladder and the idle relaunch and restart carry it out.
7. Idle, in the owner's words: "Idle means there's no work going on." The four conditions of No Work Going On carry it out, with anything unreadable counting as work. SpaceTerm follows it on macOS in this release, and SpaceTerm Core enforces the native boundary and accepted outside-interference cases in What this release does.

## Retired

This record replaces these earlier decisions by name:

- From [#799](https://github.com/sadiksaifi/SpaceTerm/issues/799): "labels of at most two words"; "with one pending item it is a plain button that performs that item's action, and with two or more it opens a compact menu"; "Restart All…"; and Recent.
- From [#873](https://github.com/sadiksaifi/SpaceTerm/issues/873): "During a Core update, the title bar, sidebar, toast and Pane all say that the Machine is updating".
- From [#886](https://github.com/sadiksaifi/SpaceTerm/pull/886): the rule that the menu's label and sentence count only the items that need the person.
- From [#871](https://github.com/sadiksaifi/SpaceTerm/issues/871): "Daily notifications stop for non-breaking Core updates that wait on open Terminal Sessions; the menu row stays."
- The desktop stages of one and two days, which become Offer, Ask and Insist.
- The reminder rule that only a breaking SpaceTerm Core update notifies.
- CONTEXT.md's Overdue Update, which the Update Ladder replaces.
- The rule that an overdue or downloaded SpaceTerm update never installs by itself while SpaceTerm runs, for the idle case on macOS only.

## Rejected

- A permanent update control in the title bar: the owner chose a pill that hides when nothing is owed.
- A single button that turns into a menu with a second item: its shape followed the item count, so a background event changed it.
- A per-version clock: with daily releases no update would reach the Ask stage.
- Restarting idle Terminal Sessions before the Ask stage: with daily releases it would end idle shells almost every day.
- Requiring that no client shows the Pane before an idle restart: it contradicts the owner's definition of idle. A longer idle period is the one setting to change if the loss proves unacceptable.
