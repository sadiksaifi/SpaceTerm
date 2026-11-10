---
name: spaceterm-release
description: Publish a SpaceTerm release with a major, minor, or patch bump.
disable-model-invocation: true
---

# SpaceTerm release

Input: `major`, `minor`, or `patch`. Ask when it is missing. Running this skill authorizes publishing the release and every fix below. Allow eight attempts. Each dispatch and each rerun is one attempt.

`.github/workflows/release.yml` validates, builds, signs, verifies, tags, publishes, and updates the Homebrew cask. You dispatch it and drive it to a published release. When a job fails, read `docs/adr/0009-distribute-tagged-releases-with-signed-updates.md` for how the jobs depend on each other.

1. Sync a clean `main` with `origin/main`. Resolve the version with `tagsmith tag --target spaceterm --channel stable --bump <bump> --dry-run --json`, and keep it for every attempt. Report the version, the commit, and the output of `mise run release:notes:preview`.
2. Run `gh workflow run release.yml --ref main -f version=<version> -f publish=true`. Follow that run until it ends.
3. The release is done when every job passed and `gh release view v<version>` shows a Latest release with its assets. Report the release URL.
4. When a job fails, read its log and classify the failure:
   - Transient, such as a lost runner, a timeout, or a GitHub or Apple outage: run `gh run rerun <run-id> --failed`, which resumes notarization and uploads from the earlier attempt. Follow the run again.
   - Defect in the source or the workflow: fix it in a PR, validated through `mise run` and a fresh adversarial review with its material findings addressed. Squash-merge, delete the branch, and sync `main`. If the failed run pushed `v<version>` and published nothing, delete that tag and its draft release. Return to step 2.

After eight failed attempts, stop and lead with the conclusion that the release failed after eight attempts. Stop and report any blocker that needs missing credentials, more authority, or a product decision.
