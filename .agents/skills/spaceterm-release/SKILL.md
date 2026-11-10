---
name: spaceterm-release
description: Publish a SpaceTerm release with a major, minor, or patch bump.
disable-model-invocation: true
---

# SpaceTerm release

Input: `major`, `minor`, or `patch`, and an optional maximum number of attempts, 3 by default. Ask for the bump when it is missing. Running this skill authorizes publishing the release and every fix below. Each dispatch and each rerun is one attempt.

`.github/workflows/release.yml` validates, builds, signs, verifies, tags, publishes, and updates the Homebrew cask. You dispatch it and drive it to a published release. When a job fails, read `docs/adr/0009-distribute-tagged-releases-with-signed-updates.md` for how the jobs depend on each other.

1. Sync a clean `main` with `origin/main`. Resolve the version with `tagsmith tag --target spaceterm --channel stable --bump <bump> --dry-run --json`, and keep it for every attempt. Report the version, the commit, and the output of `mise run release:notes:preview`.
2. Run `gh workflow run release.yml --ref main -f version=<version> -f publish=true`. Follow that run job by job, because a failed job leaves every job that does not need it running for up to 90 minutes. Run this in the background with a two-hour timeout, start it again if it times out, and act when it prints `ended` or `failed`:

   ```sh
   until state=$(gh run view <run-id> --json status,jobs --jq 'if .status == "completed" then "ended" elif any(.jobs[]; .conclusion == "failure") then "failed" else empty end') && [ -n "$state" ]; do sleep 60; done; echo "$state"
   ```

3. The release is done when every job passed and `gh release view v<version>` shows a Latest release with its assets. Report the release URL.
4. When a job fails, read its log with `gh api --allow-escape-sequences repos/{owner}/{repo}/actions/jobs/<job-id>/logs`, which works while the run is in progress. Check whether that job passed in an earlier release run, and `git log` the files that define it. A deterministic error in a job that never passed before, such as a newly listed compatibility pair, is a defect. Classify the failure:
   - Transient, such as a lost runner, a timeout, or a GitHub or Apple outage: wait with `gh run watch <run-id>` until the run ends, then run `gh run rerun <run-id> --failed`, which resumes notarization and uploads from the earlier attempt. Once `gh run view <run-id> --json status --jq .status` no longer prints `completed`, follow the run again.
   - Defect in the source or the workflow: run `gh run cancel <run-id>` at once, since the run can no longer publish. Fix the defect in a PR, validated through `mise run` and a fresh adversarial review with its material findings addressed. Squash-merge, delete the branch, and sync `main`. If the failed run pushed `v<version>` and published nothing, delete that tag and its draft release. Return to step 2.

When the last allowed attempt fails, stop and lead with the conclusion that the release failed after that many attempts. Stop and report any blocker that needs missing credentials, more authority, or a product decision.
