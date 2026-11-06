# Step updates through Update Floors and migrate data by Data Generation

SpaceTerm Core with its CLI, the macOS desktop and the Linux desktop each update themselves from a remote. A person may skip many releases. Every release therefore names the oldest version allowed to update to it directly, and each updater reaches a newer release through the releases it requires. When a release changes a retained on-disk format, it migrates the person's data. Reset is the last resort. A migration step stays in the code only until the floor of a release passes it.

## Update Floor

Every release publishes one Update Floor per product: one for SpaceTerm Core with its CLI, one for the desktops. The floor is a release version at or below the release's own version. The first release with this system is its own floor.

- The Core feeds and the Linux desktop feed carry it as `floor`. The feed schema lives in [feed.rs](../../crates/spaceterm-core/src/update/feed.rs), and [the feed module](../../mise-tasks/lib/spaceterm_tasks/release_feed.py) writes it.
- A Core archive's `build.json` carries the Core floor as `floor`, so desktop pushes, local archives and `install.sh` see it without a feed. A Development or Preflight build names a commit and has no order, so its `floor` is null and no floor applies to it.
- The macOS appcast carries the desktop floor as `sparkle:minimumUpdateVersion` on the release's item. The release's item is followed by every item of the floor release's own signed appcast, so the appcast holds one item per step back to the first release. Sparkle installs the newest item whose floor the installed version meets. [The release module](../../mise-tasks/lib/spaceterm_tasks/release.py) writes and verifies that chain.

No catalog or server serves the chain. Every release already uploads its own feeds and appcast, so `download/v<floor>/` reaches the floor release's. A GitHub release is never deleted, because the chain downloads from old releases.

## Updaters follow the floor

The Core and the Linux desktop fetch the latest feed as before. While the running version is below that release's floor, they fetch the floor release's feed from `download/v<floor>/` and repeat. They install the first release whose floor the running version meets. A release installed this way is a Stepping Stone. After restarting into a Stepping Stone, the updater checks again at once instead of waiting for its next scheduled check.

Every source refuses a candidate whose floor is above the running version: the feed, a desktop push, `spaceterm update --archive`, the `build.json` check after an upload, and `install.sh`, which names the floor version. [ordering.rs](../../crates/spaceterm-core/src/update/ordering.rs) owns the refusal for the Core, beside the version order of [ADR 0040](0040-install-core-versions-from-the-feed-a-push-or-a-local-archive.md). A desktop offers no push to a Core below the embedded build's floor. It offers that Core's own feed update instead, and the CLI reports the floor version an offline Machine needs as `details.floor`. A restart into a Stepping Stone is silent only under the rule of [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).

## Data Generations

Each retained data root has one Data Generation, an integer that names the format of every file in it. The four roots are Core data, CLI state, desktop config (`settings.json`, whose `schema_version` is its generation) and desktop state. Each root records its generation in its own `generation.json`. [generations.rs](../../crates/spaceterm-core/src/migration/generations.rs) declares each root's `oldest` and `current` generation on one line, and the release tooling reads that line.

Each root has an ordered list of steps from `oldest` to `current`. A file step rewrites one JSON value. A root step splits or merges files. An empty step is allowed, so a break that changes no data can still raise a floor. Readers stay strict and carry no versions. A released step never changes.

One runner migrates a root before anything reads it: under the service lock on the Core, under the CLI's own lock, and before Settings load on the desktop.

The greenfield desktop admits only marked data. An empty root starts at the current generation. Unmarked retained files are unreadable and use the existing reset route without a Stepping Stone. A newer marked generation remains unsupported. The desktop reset registries include managed SSH configuration and background image copies, preserving their bytes without readers for their old formats.

1. At `current`, desktop roots validate their registered files before admitting them. Other roots do nothing.
2. From `oldest` up to `current`, it copies the files to `migration-backup/` with their generation, applies the steps in memory, parses every file with its strict reader, writes each file atomically and writes `generation.json` last. A crash before that last write restarts from the backup.
3. Above `current`, it restores the backup when an activation returned to this runtime and the backup holds this runtime's generation. Otherwise it refuses with the existing unsupported-state errors and overwrites nothing. A Core that refuses never becomes ready, so the return of ADR 0034 applies. The first release ships this case, because an older runtime runs it.
4. Below `oldest`, after a step returns `Unmigratable`, or when a migrated file fails its reader, it resets the data.

A step that moves credential items creates the replacement items first and deletes the old ones only after `generation.json` commits.

## Reset is the last resort

The order is: migrate, then stay on the previous version, then reset. Data below `oldest` usually means an install that bypassed the updater, so the first thing the person sees names the Stepping Stone to install. Rebuildable client state, such as view, workspace order and update history, resets on its own: the file moves to `reset/`, defaults replace it, and a one-time notice names the kind of data lost. Settings, Workspace organization, saved connections and their credential references reset only after the person confirms. The Machine ID and pairings never reset on their own.

## Computing the floor

The release computes each product's floor with no manual step: the oldest published release, not withdrawn, whose `current` generation in every root of that product is at least the root's `oldest` today. Core uses Core data and CLI state, and the desktops use desktop config and desktop state. A release is published when GitHub lists it as a published, non-draft release. A tag alone does not count, so a release that failed before publication is never a floor. A release that predates `generations.rs` is not eligible. A release whose roots no eligible release reaches is refused, because no update could reach it. [The floor module](../../mise-tasks/lib/spaceterm_tasks/release_floor.py) lists the published releases with the GitHub CLI and reads their declarations with `git show v<release>:<file>`.

A broken Stepping Stone is replaced in two steps. First, publish a patch release on its line at the same Data Generations, such as 1.1.1 for a broken 1.1.0. Then list the broken release in [the withdrawn release list](../../packaging/withdrawn-releases.toml). The floor computation skips a withdrawn release, so the next floor is the repair. An ordinary patch release changes no floor, so updates gain no step. A withdrawn release stays published, because people still run it and its feed still leads them on, and it still counts as the previous release below.

To retire old steps, delete them and their fixtures and raise `oldest`. The next release's floor rises on its own. Steps retire only when keeping them costs too much, on no calendar.

[The release workflow](../../.github/workflows/release.yml) checks, before it publishes:

- No root's `oldest` or `current` decreases against the previous published release.
- Every generation from `oldest` to `current` has a fixture under `fixtures/migrations/<root>/gen-<N>/`, and every fixture migrates and parses with the strict readers.
- The floor release's Core, with Workspace organization and a pairing, installs the release's Core archive, restarts into it and still answers `status`, `list` and the pairing.
- The macOS updater test installs from below a floor and lands on the Stepping Stone.

The feed format freezes at the first release. A field added later needs a new feed file name.

## Rejected

- A permanent update-only protocol: [ADR 0035](0035-negotiate-protocol-versions-per-connection.md) already keeps the update lane usable across protocol versions, and every Release Core checks its own feed under ADR 0040.
- A catalog feed of many releases: each release already publishes its own feeds.
- General multi-file journaling: one backup and a generation marker give the same crash safety.
- Velopack, TUF, axoupdater and Omaha: each replaces a working signed feed, and none provides client-side floors.
- obake and magic_migrate keep old typed structs. refinery migrates only SQL. Zed's migrator preserves comments in hand-edited JSONC, and `settings.json` is machine-written strict JSON.
- Fixed upgrade stops on a schedule: they force steps when no format changed.
