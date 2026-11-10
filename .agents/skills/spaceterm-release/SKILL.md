---
name: spaceterm-release
description: Use when the user asks to release SpaceTerm with a major, minor, or patch bump.
---

# SpaceTerm release

Input: `major|minor|patch`, for example `$spaceterm-release minor`. Ask when omitted.
An explicit request to run this skill authorizes publishing the release and the fix loop below.
Allow at most eight attempts per invocation, counting each dispatch and each rerun as one attempt.

The release workflow, `.github/workflows/release.yml`, validates, builds, signs, verifies, pushes the tag, publishes the GitHub release and updates the Homebrew cask. `docs/adr/0009-distribute-tagged-releases-with-signed-updates.md` explains its order. You resolve the version, dispatch the workflow and drive it to a published release.

1. Synchronize a clean `main` with `origin/main`. Resolve the version once with `tagsmith tag --target spaceterm --channel stable --bump <bump> --dry-run --json`, and reuse it through every attempt. Run `mise run release:preview <version>` and `mise run release:notes:preview`, then report the version, commit and notes.
2. Dispatch with `gh workflow run release.yml --ref main -f version=<version> -f publish=true`. Watch every job of that run until the run ends.
3. Success: every job passes, `gh release view v<version>` shows the release as Latest with its assets, and the Homebrew cask step pushed or found no change. Report the release URL and stop.
4. Failure: read the failed jobs' logs and classify the cause.
   - Infrastructure, such as a lost runner, a timeout, or a GitHub or Apple notary outage: run `gh run rerun <run-id> --failed`, then continue at step 3. A rerun resumes notarization and draft uploads from earlier attempts.
   - Source or workflow defect: continue at step 5.
5. Create a fix branch and PR. Fix the defect, validate through `mise run`, and obtain a fresh adversarial review. Address material findings, validate again, and update the PR. After checks pass, squash-merge and delete the PR's local and remote branch. If the failed run pushed `v<version>` without publishing, delete its draft release and remote tag. Synchronize `main` and continue at step 2.

After the eighth failed attempt, stop and lead with the conclusion that the release failed after eight attempts.

A published release is immutable. Stop and report blockers that require unavailable credentials, additional authority, or a product decision.
