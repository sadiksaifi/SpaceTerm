---
name: full-ship
description: Orchestrate ready SpaceTerm issues to merged PRs with parallel subagents, adversarial reviews and unattended unblocking.
disable-model-invocation: true
---

You are the **orchestrator**. You plan slices, start subagents, route findings, hold locks, unblock, merge and clean up. Subagents do all building, reviewing, testing and computer use. Planning issues and auditing happen before this skill.

## Inputs

Mandatory (ask once if missing):
- `issues`: the ready issue numbers, in order, with their blockers.
- `stop`: when to stop, for example "after the last issue merges".

Optional (defaults shown):

```
models:
  builder:   opus-5.5 high          # implementer, integrator, fixes, PRs, acceptance runs
  reviewer:  gpt-6.1-sol high       # child and PR reviews; different model from the builder
  visual:    gpt-6.1-sol medium     # computer use here, Remora to penguin, unblock helper on m4pro
  research:  gpt-6.1-sol medium     # facts with file:line evidence only
review_rounds: { child: 2, pr: 4 }
build_slots: 3
watchdog_minutes: 10
hosts:
  self:  m3pro                      # your Mac, runs the orchestrator
  helper: m4pro                     # unblocks your Mac through Remora
  linux: penguin                    # ssh builds and tests, through its fleet alias
modules:                            # each off unless the brief turns it on
  unblock_prompts: off              # helper clicks Allow and permission dialogs on your Mac
  unblock_secrets: off              # helper types a password; give its file path on m4pro
  linux_gui: off                    # Remora to penguin's desktop; give the GDM password file path
  hand_test: on                     # visual agent tests the real app after acceptance
```

## Who drives which screen

The orchestrator runs this skill on m3pro (your Mac) only. Remora always runs on the machine that is not its target:

| Target screen | Who drives it | Runs on | How it starts |
|---|---|---|---|
| m3pro | visual agent, computer use (never Remora) | m3pro | local delegate |
| penguin desktop | visual agent, Remora | m3pro | local delegate |
| m3pro dialogs (unblock) | helper, Remora | m4pro | T3 delegate to m4pro; fallback: SSH there and start `codex exec` |

- penguin commands use SSH from m3pro, through the aliases in the `fleet` skill.
- The m4pro helper gets a plain task: dialog text, the answer, the password file path. It does not run this skill.
- Nothing targets m4pro's own screen.

## Before the user leaves

```
gather inputs; read AGENTS.md, CONTEXT.md, the issues, and .mise.toml tasks
smoke-test each enabled row with the user present:
    SSH to penguin through its fleet alias, and the m4pro delegate: a trivial command
    each screen in the table: one screenshot by its driver, from where it runs
    password files: confirm each exists without reading it
list every blocker found; resolve them with the user now
confirm "go"; from here the user is away until done
```

## Run

```
.snap/<run>/: README (inputs, rules, runbook), status, log, locks.md, notes/
start the watchdog before any agent: each tick reads status, checks each active task once,
    dead build slots, free disk, stalls over 30 min; acts on anything unattended
for each issue (start the next early on the current head; rebase --onto main after merge):
    assign, move to In Progress
    research agents report facts; you split slices with disjoint file ownership
    integration branch + worktree → one PR
    per slice, all in parallel, own worktree:
        builder:  /snap-forge
        reviewer: fresh, /snap-review, mutation proof (each repaired or merged test fails on an injected fault)
        findings → builder applies them with /snap-forge (reviewer fixes trivial ones)
        repeat up to review_rounds.child; still findings → accept on mutation evidence
        integrator (builder) rebases the child, squash-merges, runs the merge gate; siblings rebase
    integrator opens the PR with /snap-pr; move to In Review
    PR rounds, each a fresh reviewer, alternating models: /snap-review, then fixes via /snap-resolve
        up to review_rounds.pr; a large late fix earns one more round
    acceptance (builder): full suites once on every OS; classify each failure against a main baseline;
        rerun only what a fix touched
    hand test (visual): real app, a control case before each scenario; report, never answer prompts
    disclose pre-existing failures; squash-merge; issue Done
    clean branches, worktrees, targets, remote-host files
at stop: delete schedules, write the final log line, report
```

## Rules

- **Parallel by default.** Never leave a host idle. Overlap reviews, builds and suites with building.
- **Cost.** Reuse a delegate whose context fits; reviewers stay fresh.
- **Research gives facts only.** You or a builder plan from them.
- **Shared resources.** Read `locks.md` before using the GUI, the helper or penguin; add your line, remove only yours. Wrap heavy builds in the build-slot semaphore. Run one helper at a time.
- **Notes** go only to `.snap/<run>/` by absolute path, never into a worktree or the repo.
- **Testing.** Focused tests while building; at acceptance `mise run validate` on macOS and `mise run test`, `test:tools`, `test:native` on penguin. Use `mise run fmt:check`, never `cargo fmt --all --check`, which skips files pulled in through `include!`.
- **Worktrees** live at `~/.worktrees/spaceterm-desktop/`, with `target/` cloned by APFS (`cp -cR`). On penguin, put `CARGO_TARGET_DIR` on the SSD under `/var/tmp`, never `cargo clean` a shared target, and never switch its main checkout.
- **Secrets.** Pipe passwords from their file into the input; type them as keystrokes, never paste, read, print or log them.
- **Blocked?** Try to unblock on your own, for yourself or for others, using others' techniques or your own intelligence and cleverness. Ask the user only for a decision that is theirs, in one line.
- **Report** results faithfully: merged SHAs, failures and their classification, what was not verified.
