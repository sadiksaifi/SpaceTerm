# Issue 43 Acceptance Evidence Schema

This document defines the evidence bundle and final GitHub comment for the campaign described in
[`ISSUE_43_ACCEPTANCE_RUNBOOK.md`](ISSUE_43_ACCEPTANCE_RUNBOOK.md). The schema is intentionally
independent of provisional harness and workload script names. Record the exact driver path and
invocation used after lower stack layers are rebased.

## Bundle layout

Create one directory named with the campaign run ID:

```text
<run-id>/
  campaign.yaml
  artifacts.tsv
  control.sha256
  identity/
  native/
  focus/
  capability/
  failure/
  performance/
  package/
  supplementary/
```

`campaign.yaml` is the structured campaign record. `artifacts.tsv` is the payload integrity and
privacy manifest. `control.sha256` is a detached digest file for those two control files. The
remaining directories contain immutable payload artifacts. A GitHub upload or external artifact
store may change the public filename; preserve the original relative path and SHA-256 in the final
issue comment.

## Artifact naming

Name every payload artifact (the three control filenames are fixed by the bundle layout):

```text
<case-id>--<subject>--<attempt-2digit>--<artifact-kind>.<extension>
```

Requirements:

- use only lowercase ASCII letters, digits, and hyphens before the extension;
- use case IDs from the runbook;
- use `spaceterm` or `ghostty` as the subject;
- start attempts at `01` and increment rather than overwriting;
- use descriptive kinds such as `interaction`, `screen-recording`, `pty-bytes`, `rss`,
  `time-profiler`, `allocations`, `config`, `verification`, or `validation`;
- keep raw and redacted artifacts separate, publish only privacy-reviewed artifacts, and record
  the relationship in `redaction_notes`;
- compute the hash after the final safe artifact is produced.

## Payload manifest and control digests

`artifacts.tsv` has one header and one row per immutable payload artifact:

```text
artifact_id	record_id	subject	case_id	relative_path	sha256	bytes	media_type	created_utc	run_id	producer	producer_version	privacy_review	redaction_notes	public_url
```

Field rules:

| Field | Rule |
| --- | --- |
| `artifact_id` | `<record-id>-<kind>`; unique within the campaign. |
| `record_id` | Unique owning observation record defined below. |
| `subject` | `spaceterm` or `ghostty`; must match the owning record. |
| `case_id` | Stable runbook inventory identity; must match the owning record. |
| `relative_path` | Path below the run-ID directory; no absolute or parent-relative paths. |
| `sha256` | Lowercase 64-character SHA-256 of the published file. |
| `bytes` | Exact non-negative byte count. |
| `media_type` | Concrete type such as `image/png`, `video/quicktime`, `text/plain`, `text/tab-separated-values`, or the documented Instruments trace type. |
| `created_utc` | RFC 3339 UTC timestamp. |
| `run_id` | Exact frozen campaign run ID. |
| `producer` | Capturing command or application name without credentials. |
| `producer_version` | Exact version output or app version/build. |
| `privacy_review` | `PASS` only after manual inspection; otherwise `PENDING` or `REJECTED`. |
| `redaction_notes` | `none`, or a precise description that does not reveal the removed value. |
| `public_url` | Direct GitHub attachment/artifact URL after upload; blank before upload. |

The payload manifest MUST NOT contain rows for `campaign.yaml`, `artifacts.tsv`, or
`control.sha256`. Those are control files, not payload artifacts. This exclusion prevents a file
from directly or transitively committing to its own digest.

Finalize and verify the bundle in this order:

1. Privacy-review and upload every payload artifact, then freeze its final published bytes and
   public URL.
2. Generate `artifacts.tsv` over only those frozen payload files.
3. Generate `campaign.yaml`, including the SHA-256 and row count of `artifacts.tsv` under
   `payload_manifest`.
4. Generate `control.sha256` with exactly these two lines, in this order, using two ASCII spaces
   between the lowercase digest and filename. Encode it as UTF-8 without a byte-order mark, use LF
   line endings, and include one final LF:

   ```text
   <campaign.yaml sha256>  campaign.yaml
   <artifacts.tsv sha256>  artifacts.tsv
   ```

5. From the run-ID directory, `shasum -a 256 -c control.sha256` is the expected verifier. Upload all
   three control files and record their direct URLs plus the result of
   `shasum -a 256 control.sha256` in the final issue comment.

`control.sha256` does not list itself, and no bundle file contains the digest of
`control.sha256`. The final GitHub comment is the external anchor for that digest. An artifact
with a missing hash, `PENDING`/`REJECTED` privacy review, or inaccessible public URL cannot support
a final PASS.

## Campaign record

Use this YAML-compatible template. Values in angle brackets are required placeholders unless the
field is explicitly marked optional.

```yaml
schema_version: 2
issue: 43
run_id: <i43-YYYYMMDDTHHMMSSZ-commit12-dmgsha12>
campaign_status: <PASS|FAIL>
started_utc: <RFC3339 UTC>
finished_utc: <RFC3339 UTC>

frozen_artifact:
  repository: https://github.com/sadiksaifi/SpaceTerm
  commit_sha: <40 lowercase hex>
  cargo_lock_sha256: <64 lowercase hex>
  working_tree_clean: true
  package_command: just package
  marketing_version: <value>
  build_version: <value>
  executable_architectures: [<value>]
  code_signing_command: <sanitized exact command>
  code_signing_result: <PASS|FAIL>
  app_bundle_sha256: <64 lowercase hex>
  dmg_sha256: <64 lowercase hex>
  package_verification_artifact: <artifact_id>
  launch_source: mounted verified DMG

host:
  macos_version: <value>
  macos_build: <value>
  machine_model: <value>
  model_identifier: <value>
  cpu: <value>
  memory_bytes: <integer>
  displays:
    - display_id: <non-sensitive local label>
      logical_resolution: <width>x<height>
      backing_resolution: <width>x<height>
      refresh_hz: <number>
      backing_scale: <number>
  terminal_font_selected: <family and face>
  jetbrains_mono_nerd_font_available: <true|false>
  initial_grid:
    rows: <integer>
    columns: <integer>
    logical_width: <number>
    logical_height: <number>
    backing_pixel_width: <integer>
    backing_pixel_height: <integer>
  input_sources: [<non-US source and IME used>]
  second_display_available: <true|false>
  numpad_available: <true|false>

clean_environment:
  workspace_root: <$TMPDIR-normalized path>
  temporary_configurations:
    - program: <name>
      path: <$HOME/$TMPDIR-normalized path>
      sha256: <64 lowercase hex>
  permanent_user_configuration_used: false
  privacy_review: PASS

programs:
  - name: <Bash|Zsh|Vim|Neovim|tmux|less|fzf|btop|Yazi|Claude Code|pi-coding-agent>
    executable: <$HOME-normalized exact path>
    executable_sha256: <64 lowercase hex>
    version_command: <sanitized exact command>
    version_output: <value>

ghostty_reference:
  original_prd_revision: 46767b521358200bfe3f268f365ccd2f218db558
  embedded_conformance_revision: a887df42c56f6de86c0fe6da9c4eeca37931e083
  runnable_build_source: <release channel/source>
  public_version: <value>
  commit_sha: <value or unavailable with reason>
  marketing_version: <value>
  build_version: <value>
  executable: <$HOME-normalized exact path>
  executable_architectures: [<value>]
  code_signing_result: <PASS|FAIL>
  app_bundle_sha256: <64 lowercase hex>
  config_path: <$HOME/$TMPDIR-normalized path>
  config_sha256: <64 lowercase hex>
  relationship_to_recorded_revisions: <explicit explanation>
  ambiguity_notes: <none or explicit substitution/availability note>

drivers:
  - purpose: <identity|native|focus|failure|workload|rss|package|payload-manifest|control-digest>
    path: <repository-relative path or manual>
    commit_sha: <40 lowercase hex>
    version_or_help: <value>
    invocation: <sanitized exact command>

case_results:
  - <case record; repeat for every actual subject-scoped execution>

validation:
  command: just validate
  status: <PASS|FAIL|NOT-RUN>
  artifact_id: <artifact_id>

known_deviations:
  - case_id: <case_id>
    record_id: <effective failing record ID>
    smallest_reproduction: <sanitized steps>
    follow_up_issue: <URL>
    status: <open|fixed-and-rerun>

payload_manifest:
  path: artifacts.tsv
  sha256: <64 lowercase hex>
  payload_rows: <positive integer>
  excluded_control_files: [campaign.yaml, artifacts.tsv, control.sha256]
  privacy_review: PASS

control_digest:
  path: control.sha256
  algorithm: sha256
  entries_in_order: [campaign.yaml, artifacts.tsv]
  digest_anchored_in: final GitHub issue comment
```

Do not add serial numbers, hardware UUIDs, account/host names, Apple IDs, network identifiers,
terminal content, clipboard content, environment dumps, private paths, logical/typed key text, or
credentials to `campaign.yaml`.

## Case record

`case_id` is the stable runbook inventory identity. It never identifies a subject or a particular
execution and never changes for a rerun. Every actual execution is a separate subject-scoped
record with this unique identity:

```text
<run-id>-<case-id>-<subject>-a<attempt-2digit>
```

`subject` is `spaceterm` or `ghostty`. `attempt` starts at 1 and increases independently for each
`(case_id, subject)` pair. A rerun creates a new record and sets `supersedes_record_id` to the exact
prior record for the same case and subject. It does not rename or overwrite the prior record.
During finalization, each required `(case_id, subject)` has exactly one effective leaf record: the
one that no other record supersedes. A supersession chain must be linear; forks and cycles are
invalid.

When SpaceTerm and Ghostty are compared, each record sets `comparison_record_id` to the exact
opposite-subject record. The references must be reciprocal, both records must share the same
`case_id`, and their workload inputs and comparison-affecting settings must match. A comparison
record is not a rerun and must never appear in `supersedes_record_id`. Use `null` for an unpaired
record. Rerunning either side of a paired case requires new records for both subjects: each new
record supersedes its own prior subject record, and the two new records link to each other. This
keeps comparison settings and reciprocal references coherent.

```yaml
record_id: <run-id>-<case-id>-<subject>-a<attempt-2digit>
case_id: <stable runbook inventory id>
subject: <spaceterm|ghostty>
matrix: <native|focus|capability|failure|performance|package|supplementary>
attempt: <positive integer>
comparison_record_id: <exact opposite-subject record ID or null>
supersedes_record_id: <exact prior same-subject record ID or null>
status: <PASS|FAIL|NOT-RUN|SKIPPED-UNAVAILABLE|NOT-APPLICABLE>
started_utc: <RFC3339 UTC>
finished_utc: <RFC3339 UTC>
frozen_identity_verified: <true|false>
command: <sanitized exact command>
environment_and_config: <paths/checksums and non-secret settings>
interactions:
  - order: <integer>
    action: <exact reproducible interaction>
    timing: <value or not timing-sensitive>
expected: <complete expected result>
authority: <published protocol, Apple behavior, product contract, or issue #43>
observed: <complete observation>
artifacts: [<artifact_id>, <artifact_id>]
comparison_observation: <difference or similarity to comparison record, or not applicable>
smallest_reproduction: <required for FAIL; otherwise none>
skip_reason: <required only for SKIPPED-UNAVAILABLE>
conditional_subcases:
  - name: <issue #43 conditional subcase>
    status: <PASS|SKIPPED-UNAVAILABLE|NOT-APPLICABLE>
    availability_or_precondition_evidence: <recorded fact>
notes: <non-secret notes>
```

For `PASS`, `frozen_identity_verified` must be true, required artifacts must exist, and the
observation must directly cover every unconditional clause of the runbook row. For
`SKIPPED-UNAVAILABLE`, quote the issue's conditional phrase and record the availability fact. Use
`NOT-APPLICABLE` only for `just doctor` when tool availability is already known or a coding-agent
link that was not presented. Do not use either status for a named program or required tool. The
campaign result uses only effective leaf records; superseded failures remain in the evidence
history, while every required effective leaf must pass under the frozen-artifact rules.

## Matrix case inventory

The exact requirements for these IDs are in the runbook. Every non-supplementary ID must have an
effective SpaceTerm record. Every required performance ID must also have an effective paired
Ghostty record. A failed named-program SpaceTerm record must have the corresponding Ghostty record
required to determine whether the frozen reference differs under the same reproduction.

### Native

```text
native-bash
native-zsh
native-vim
native-neovim
native-tmux
native-less
native-fzf
native-btop
native-yazi-no-previews
native-claude-code
native-pi-coding-agent
```

### Terminal Input Focus

```text
focus-pane-switch
focus-sidebar
focus-workspace-rename
focus-workspace-context-menu
focus-pane-menu
focus-window-menu
focus-top-chrome
focus-window-selector
focus-terminal-find
focus-native-panels
focus-non-key-os-window
focus-app-activation
focus-hierarchy-switch
```

### Capability and native service

```text
capability-keyboard
capability-mouse
capability-paste
capability-focus-bytes
capability-styles
capability-links
capability-resize-scrollback
capability-accessibility
capability-attention
capability-macos-services
capability-context-actions
capability-quick-look
capability-local-diagnostics
```

### Failure recovery

```text
failure-presentation-recoverable
failure-renderer-resource-recoverable
failure-platform-action-recoverable
failure-pty-fatal
failure-emulator-session-fatal
failure-normal-exit
failure-diagnostics-bounded
```

### Performance

Use separate `spaceterm` and `ghostty` records for each paired run. Both retain the same inventory
`case_id` and link to each other's unique `record_id` through reciprocal
`comparison_record_id` fields.

```text
perf-sustained-ascii
perf-sustained-unicode-styles
perf-sustained-scrolled
perf-sustained-hidden
perf-resize
perf-render-idle-cursor-blink
perf-render-text-blink
perf-render-sustained-output
perf-render-selection
perf-render-marked-text
perf-render-live-resize
```

`perf-render-kitty-static` is supplementary and belongs to issue #89.

### Package

```text
package-doctor
package-build
package-launch-dmg
package-window-shell
package-command-output
package-resize
package-process-reap
package-identity
package-final-validate
```

## Focus evidence extension

Add these fields to every focus case:

```yaml
focused_pane_identity_before: <opaque non-secret identity>
focused_pane_identity_blocked: <same or expected changed identity>
terminal_input_focus_before: true
terminal_input_focus_blocked: false
terminal_input_focus_restored: true
cursor_negotiated_before: <shape/blink/visibility>
cursor_blocked: <steady hollow or hidden>
cursor_restored: <shape/blink/visibility>
hollow_visible_on_next_presented_frame: <true|false>
dec_1004:
  enabled: <true|false>
  enable_current_state_bytes_hex: <exact bytes>
  loss_bytes_hex: <exact bytes>
  gain_bytes_hex: <exact bytes>
  duplicate_reports: <integer>
  held_key_release_bytes_hex: <ordered exact bytes>
  pty_artifact_id: <artifact_id>
```

The recording or frame sequence must make the authoritative focus transition and the next
presented frame identifiable. A screenshot taken at an unknown time cannot prove next-frame
behavior.

## Failure evidence extension

Add these fields to every failure-recovery case:

```yaml
injection_or_trigger: <exact production-Seam trigger>
presentation_generation_before: <integer>
presentation_generation_visible_during_failure: <integer or unavailable with reason>
visible_state: <observation>
terminal_input_usable_during_failure: <true|false|not-applicable>
recovery_action: <exact action>
post_recovery_result: <observation>
owned_processes_remaining: <integer or not-applicable>
diagnostics_bytes: <integer or not-applicable>
diagnostics_content_audit: <PASS|FAIL|not-applicable>
```

## Performance evidence extension

In addition to the base record's required reciprocal `comparison_record_id`, add these fields to
sustained-output and resize records:

```yaml
optimization_profile: <value>
workload_command: <sanitized exact command>
workload_input_sha256: <64 lowercase hex>
duration_seconds: <integer>
warmup_seconds: <integer>
bytes_processed: <integer>
initial_grid: <rows>x<columns>/<backing pixels>
rss_samples_artifact_id: <artifact_id>
rss_sample_interval_seconds: 10
first_post_warmup_five_minutes:
  minimum_bytes: <integer>
  maximum_bytes: <integer>
  range_bytes: <integer>
final_five_minutes:
  minimum_bytes: <integer>
  maximum_bytes: <integer>
  range_bytes: <integer>
allowed_range_delta_bytes: <max of 10 percent of first range or 64 MiB>
memory_plateau_result: <PASS|FAIL>
maximum_main_thread_stall_ms: <number>
input_responsiveness_observation: <value>
ui_backlog_observation: <value>
final_presentation_observation: <value>
shell_process_exit_observation: <value>
time_profiler_artifact_id: <artifact_id>
allocations_artifact_id: <artifact_id>
screen_artifact_ids: [<artifact_id>]
```

For resize, also record resize count, reflow timings, PTY cell/pixel geometry samples, final grid,
Selection anchoring, Terminal Viewport anchoring, backing-scale transition, and whether a second
display was available.

For render-path proof, add a conclusion for each forbidden stack or behavior and link the exact
trace/call-tree evidence:

```yaml
paint_text_shaping_stack_present: <true|false>
paint_path_or_plan_construction_present: <true|false>
paint_normal_frame_allocation_stack_present: <true|false>
cursor_or_blink_reshaped_unchanged_rows: <true|false>
changed_row_proportionality_result: <PASS|FAIL>
exceptional_error_allocations_excluded: <description>
```

## Required issue comment template

Issue #43 requires one final comment with this minimum shape:

```text
Acceptance run: <date/time and run identity>
Commit: <sha>
App/DMG SHA-256: <hashes>
macOS/hardware/display: <facts>
Shell/TUI versions: <table>
Native matrix: <pass/fail table with artifact links>
Focus matrix: <pass/fail table with screenshots and DEC 1004 byte log>
Failure recovery: <pass/fail table>
Performance: <workloads, RSS samples, trace links, SpaceTerm/Ghostty observations>
Packaged smoke: <result and artifacts>
just validate: <result and log>
Known deviations: <smallest reproductions and linked follow-up issues>
Final result: PASS or FAIL
```

Use the expanded Markdown template below so the minimum fields and all matrices remain auditable.

````markdown
## Acceptance run

- **Run ID:** `<run-id>`
- **Started/finished (UTC):** `<timestamps>`
- **Commit:** `<40-character SHA>`
- **Cargo.lock SHA-256:** `<hash>`
- **App/DMG SHA-256:** `<app hash>` / `<DMG hash>`
- **Package version/build/architecture/signing:** `<facts>`
- **Launch source:** mounted verified DMG
- **Campaign record:** [campaign.yaml](<URL>)
- **Payload manifest:** [artifacts.tsv](<URL>) (`<SHA-256>`; `<row count>` payload rows)
- **Detached control digests:** [control.sha256](<URL>) (`<control.sha256 SHA-256>`)
- **Privacy review:** PASS

### macOS, hardware, display, and terminal identity

| Field | Recorded value |
| --- | --- |
| macOS version/build | `<value>` |
| Machine model / model identifier | `<value>` |
| CPU / memory | `<value>` |
| Display logical/backing/refresh/scale | `<value>` |
| Selected terminal font | `<value>` |
| JetBrainsMono Nerd Font available | `<true/false>` |
| Initial rows/columns | `<value>` |
| Initial logical/backing dimensions | `<value>` |
| Clean Workspace root / temporary config | `<privacy-normalized facts and hashes>` |

### Shell and TUI versions

| Program | Executable | Version | Executable SHA-256 |
| --- | --- | --- | --- |
| Bash | `<value>` | `<value>` | `<hash>` |
| Zsh | `<value>` | `<value>` | `<hash>` |
| Vim | `<value>` | `<value>` | `<hash>` |
| Neovim | `<value>` | `<value>` | `<hash>` |
| tmux | `<value>` | `<value>` | `<hash>` |
| less | `<value>` | `<value>` | `<hash>` |
| fzf | `<value>` | `<value>` | `<hash>` |
| btop | `<value>` | `<value>` | `<hash>` |
| Yazi | `<value>` | `<value>` | `<hash>` |
| Claude Code | `<value>` | `<value>` | `<hash>` |
| pi-coding-agent | `<value>` | `<value>` | `<hash>` |

### Native matrix

| Case / record IDs | Status | Command/interactions | Expected/observed | Evidence | Ghostty difference |
| --- | --- | --- | --- | --- | --- |
| `<case ID; SpaceTerm record; paired Ghostty record when required>` | `<status>` | `<summary>` | `<summary>` | `<links>` | `<value>` |

### Terminal Input Focus matrix

| Case / SpaceTerm record | Status | Cursor before/blocked/restored | Next-frame proof | DEC 1004 and held-release bytes | Evidence |
| --- | --- | --- | --- | --- | --- |
| `<case ID; SpaceTerm record ID>` | `<status>` | `<values>` | `<value>` | `<exact hex and byte-log link>` | `<links>` |

### Capability and native-service matrix

| Case / SpaceTerm record | Status | Expected/observed | Evidence |
| --- | --- | --- | --- |
| `<case ID; SpaceTerm record ID>` | `<status>` | `<summary>` | `<links>` |

### Failure recovery

| Case / SpaceTerm record | Status | Trigger / visible generation | Recovery / post-recovery | Evidence |
| --- | --- | --- | --- | --- |
| `<case ID; SpaceTerm record ID>` | `<status>` | `<summary>` | `<summary>` | `<links>` |

### Performance

- **Frozen Ghostty reference:** `<complete identity, hashes, revision relationship, ambiguity>`
- **Workloads:** `<exact commands, inputs, byte counts, durations>`
- **RSS samples and plateau calculation:** `<links and result>`
- **Time Profiler / Allocations:** `<links>`
- **SpaceTerm/Ghostty observations:** `<paired summary without treating Ghostty as authority>`

| Scenario | SpaceTerm record | Ghostty record | Required threshold | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| Sustained ASCII | `<record ID and facts>` | `<record ID and facts>` | `<facts>` | `<status>` | `<links>` |
| Sustained Unicode/styles | `<record ID and facts>` | `<record ID and facts>` | `<facts>` | `<status>` | `<links>` |
| Scrolled/hidden restoration | `<record ID and facts>` | `<record ID and facts>` | `<facts>` | `<status>` | `<links>` |
| Resize | `<record ID and facts>` | `<record ID and facts>` | `<facts>` | `<status>` | `<links>` |
| Render-path proof | `<record ID and facts>` | `<record ID and facts>` | `<facts>` | `<status>` | `<links>` |

### Packaged smoke

| Case / SpaceTerm record | Status | Observation | Evidence |
| --- | --- | --- | --- |
| `<case ID; SpaceTerm record ID>` | `<status>` | `<summary>` | `<links>` |

### just validate

- **Result:** `<PASS/FAIL/NOT-RUN>`
- **Complete log:** [validation log](<URL>) (`<SHA-256>`)

### Known deviations

| Case / record IDs | Smallest reproduction | Ghostty observation | Follow-up issue | Current status |
| --- | --- | --- | --- | --- |
| `<case and record IDs, or none>` | `<steps>` | `<observation>` | `<URL>` | `<status>` |

### Supplementary Kitty static graphics

`<Not run, or issue #89 smoke result and artifact links. This row does not affect conventional acceptance.>`

## Final result: `<PASS or FAIL>`

`<One-sentence conclusion. Any required FAIL, NOT-RUN, missing payload/control evidence, digest failure, or frozen-identity mismatch requires FAIL.>`
````

Before posting, replace every placeholder, include every case row rather than writing “all passed,”
confirm every link resolves for another viewer, regenerate `artifacts.tsv`, regenerate
`campaign.yaml`, regenerate and verify `control.sha256`, and compute the detached control file's
SHA-256 for the issue-comment anchor.
