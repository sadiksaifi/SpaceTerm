---
name: spaceterm-release
description: Use when the user asks to release SpaceTerm with a major, minor, or patch bump.
---

# SpaceTerm release

Input: `major|minor|patch`, for example `$spaceterm-release minor`. Ask when omitted.
An explicit request to run this skill authorizes the loop below.
Allow at most eight release attempts per invocation, counting each release tag push as one attempt.

1. Read `.tagsmith.jsonc`, `.github/workflows/release.yml`, and release tasks in `.mise.toml`. Use `$tagsmith` on the clean, synchronized release base branch. Resolve the bump once; reuse that version throughout retries.
2. Dry-run the explicit version, report the resolved release, then create and push with identical arguments. Monitor every release job.
3. Success: every job passes, published asset checksums match, and distribution updates complete. Report the release URL and stop.
4. Failure: retain diagnostic evidence, stop remaining jobs, and delete only this attempt's release, local/remote tag, and workflow artifacts. Undo its distribution changes. After the eighth failed attempt, stop and lead with the conclusion that the release failed after eight attempts.
5. Create a fix branch and PR. Fix the failure and validate through `mise run`. Obtain a fresh adversarial review using `$codex-subagent`. Address material findings, validate, and update the PR.
6. After checks pass and findings are addressed, squash-merge and delete the PR's local/remote branch. Synchronize the release base branch; retry step 2.

For Linux validation, use `ssh penguin` and `~/Projects/SpaceTerm`. For fork lookup on either host, follow `<repo-root>/AGENTS.md`.

Stop and report blockers requiring unavailable credentials, additional authority, or a product decision.
