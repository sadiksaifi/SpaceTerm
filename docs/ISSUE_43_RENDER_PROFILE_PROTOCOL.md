# Issue 43 Render-Path Profile Protocol

This protocol supplies the six render-path performance cases required by issue #43. It extends
the release-performance campaign; it does not replace sustained-output, resize, native, package,
or manual acceptance. Every case runs once in the exact frozen SpaceTerm package and once in the
frozen Ghostty reference with the paired font, grid, environment, command, workload, and process
identity recorded by `freeze-performance-pair.sh`.

Source inspection, unit tests, the immutable plan, or a trace recorder reporting success cannot
prove a render-path PASS. `analyze-release-render-profile-case.sh` returns PASS only when actual
full-duration trace artifacts and a human call-tree review are present and hash-bound.

## Authenticated case and trace closure

Before any campaign secret is created or read, select the reviewed repository HEAD and freeze the
repository-owned recorder, analyzer, and helpers from its committed Git blobs:

```sh
scripts/acceptance/freeze-render-profile-tool-bundle.sh \
  --source-commit <reviewed-40-hex-HEAD> \
  --output-directory <absent-path-below-a-0700-parent>
```

The freezer accepts no secret argument. It rejects a commit other than HEAD and any listed source
that is untracked, staged, dirty, symbolic, or different from its selected Git blob. The published
private bundle preserves the `scripts/` layout, makes every helper `0555`, and emits one read-only,
exact 56-row `spaceterm.render-profile-tool-bundle/v1` manifest containing the selected commit and
the source and bundle paths and SHA-256 hashes for all 13 tools, including the executing bootstrap
freezer after it proves its own clean path and bytes against the selected Git blob. All subsequent secret-consuming
commands must be invoked from this bundle. Their authenticated case artifacts bind the bundle
manifest hash, while the recorder and analyzer independently require the externally selected
source commit and verify the complete manifest rather than trusting its self-asserted commit.

Before capture, run the bundled `render-trace-receipt.py manifest` once per scenario and subject.
Its exact ordered schema is `MANIFEST_KEYS` in that script, uses
`SPACETERM_RENDER_CAMPAIGN_CASE_MANIFEST_V1\0`, and binds campaign/session, a
unique 64-hex case nonce, scenario/subject, subject identity, the expected render intent/evidence,
common driver intent/receipt, recorder-owned trace-anchor receipt, and trace-receipt paths,
their parent identity, and the frozen
campaign-secret device/inode and key identifier. Every expected case path must still be absent.
The campaign index must reject any nonce repeated among its 12 rows.

The trace recorder alone publishes the exact 27-row authenticated anchor handoff with domain
`SPACETERM_RENDER_TRACE_ANCHOR_V1\0`, using its internally measured epoch/continuous anchors.
Caller-supplied clock anchors are not accepted. A metadata file may remain after an anchor-helper
failure, but the recorder exits nonzero and no final trace receipt or analyzer PASS is possible.
Archive the recorder's `.trace` directory with `archive-render-trace.py --trace <case.trace>
--output <case.trace.zip>`. The helper accepts one canonical physical `.trace` root, rejects
links/special entries, bounds entries and expanded size, writes deterministic sorted members under
that single root, and atomically publishes a read-only ZIP for archive regeneration validation.

After capture, archive/export regeneration, media collection, and common signed-driver
verification, run the bundled `render-trace-receipt.py finalize`. Its exact ordered schema is
`RECEIPT_KEYS` in that script. The HMAC is SHA-256 over
`SPACETERM_RENDER_TRACE_RECEIPT_V1\0` followed by all preceding UTF-8 LF-terminated TSV rows.
It directly binds the manifest/case tuple; PID, kernel `sec:usec` start identity and live code
identity; run/render/driver evidence; zero-workload or sustained-output-v3 producer hashes;
trace metadata/archive/TOC/regenerated exports/verification; action video and screenshot; epoch,
continuous, and clock-anchor intervals; and path/device/inode/hash tuples for every invoked tool,
including the common driver-receipt, trace, archive, video, and receipt verifiers.

All mutable campaign inputs and repository-owned invoked tools are canonical, singleton,
non-symbolic and single-linked. Repository tools must come from the frozen bundle. Canonical
root-owned tools under the macOS sealed system prefixes may have multiple hard links, but must be
nonwritable by group/other and remain bound by path, device, inode, size, modification time, and
hash. The secret is read only from its file by `/usr/bin/python3`; key bytes never enter argv
or the environment. Analysis independently recomputes the HMAC and current tuples instead of
trusting helper stdout. The common trace metadata remains its exact 25-key v3 schema; this receipt
is a separate artifact.

## Frozen scenarios

Generate each plan with:

```sh
scripts/acceptance/performance-plan.sh \
  --scenario <case-id> \
  --plan <case-id>-plan.tsv \
  --metadata <case-id>-plan-metadata.tsv
```

The files are published read-only. The cadence and counts below are protocol constants; editing
metadata to reduce them makes the analyzer return NOT-RUN.

| Case ID | Warm-up | Recorded duration | Required action |
| --- | ---: | ---: | --- |
| `perf-render-idle-cursor-blink` | 15 s | 120 s | Observe 60 complete Cursor-blink cycles while terminal rows remain unchanged. |
| `perf-render-text-blink` | 15 s | 120 s | Present a deterministic SGR text-blink fixture and observe 60 complete cycles without changing its rows. |
| `perf-render-sustained-output` | 30 s | 180 s | Keep deterministic output active and review 18 ten-second changed-row windows. |
| `perf-render-selection` | 15 s | 120 s | Create, alter, and clear Selection 30 times over unchanged terminal rows. |
| `perf-render-marked-text` | 15 s | 120 s | Use the campaign IME to compose, alter, and cancel or commit Marked Text 24 times over unchanged rows. |
| `perf-render-live-resize` | none | 180 s | Execute 180 one-second native drag-resize cycles across width, height, and both dimensions. |

Checkpoint rows freeze the observation/action cadence for Cursor blink, text blink, sustained
output, Selection, and Marked Text. They do not claim that a manual action occurred. The production
native driver must nevertheless execute every plan row against one authenticated PID and window,
and publish its immutable 11-column event stream. The operator executes the named manual action in
the packaged application at each checkpoint, retains the screen recording, and enters the observed
completed count in the manual review. For live resize, the native driver itself executes and
verifies every `resize-grid` row. Any missing, reordered, late, wrong-window, wrong-PID, or
unverified driver event, or any incomplete action count, is NOT-RUN.

Start Time Profiler, Allocations, and Hangs after the warm-up and cover the complete recorded
duration. The target must be the frozen process for that subject. Preserve:

- a ZIP archive containing exactly one non-empty `.trace` bundle;
- the `xctrace export --toc` output and the exact verification receipt produced by
  `scripts/verify-release-performance-trace.py`;
- the target-scoped Time Profiler export;
- the Allocations export;
- the Hangs export, including a valid empty-event table when no hangs occurred;
- the trace metadata, a representative call-tree screenshot, and a screen recording covering the
  complete required action cadence.

Freeze the subject run with `freeze-performance-run.sh`. Before capture, use
the bundled `freeze-render-profile-intent.sh` with `--render-tool-bundle-manifest`,
`--expected-source-commit`, and `--trusted-source-repository` to HMAC-authenticate the immutable plan, pair, run, manifests,
subject, campaign/session/nonce, action contract, and the still-absent driver/video/final-evidence
paths plus their physical parent identities. After the driver and screen recording finish, create
the render-workload metadata from the canonical driver output:

```sh
<frozen-tool-bundle>/scripts/acceptance/freeze-render-profile-intent.sh \
  --render-tool-bundle-manifest <frozen-tool-bundle>/tool-bundle-manifest.tsv \
  --expected-source-commit <reviewed-40-hex-HEAD> \
  --trusted-source-repository <canonical-reviewed-repository-root> \
  <all case, identity, pending-path, secret, and output arguments>
```

```sh
scripts/acceptance/freeze-render-profile-workload.sh \
  --subject <spaceterm-or-ghostty> \
  --scenario <case-id> \
  --plan <plan.tsv> \
  --plan-metadata <plan-metadata.tsv> \
  --pair-metadata <pair-metadata.tsv> \
  --subject-identity <subject-identity.tsv> \
  --driver-events <native-driver-events.tsv> \
  --action-video <complete-action-recording.mov> \
  --output <render-workload-metadata.tsv>
```

Then use the bundled `finalize-render-profile-evidence.sh` with the same three tool-trust
arguments to verify the intent and driver one-to-one against
the plan and HMAC-authenticate the immutable driver, video, render-workload record, measured bounds,
counts, and result. The trace recorder's mutually exclusive `render-profile-v1` mode accepts the
pre-frozen intent and pending final-evidence path. It waits up to 600 seconds by default for the
finalizer to hash a large recording and publish immutable evidence; operators may set
`--render-evidence-timeout-seconds` from 1 through 3600 without changing the common workload-v3
producer's five-second publication wait. `supplemental_evidence_sha256` always binds that final
render evidence.

```sh
<frozen-tool-bundle>/scripts/acceptance/finalize-render-profile-evidence.sh \
  --render-tool-bundle-manifest <frozen-tool-bundle>/tool-bundle-manifest.tsv \
  --expected-source-commit <reviewed-40-hex-HEAD> \
  --trusted-source-repository <canonical-reviewed-repository-root> \
  <all intent, capture, secret, and output arguments>
```

The bundled trace recorder receives the same three arguments in `render-profile-v1` mode and
rejects checkout or substituted execution before reading the campaign secret.

`perf-render-sustained-output` alone must also pass `--workload-metadata`,
`--workload-events`, and `--workload-ready-receipt` from the common workload-v3 producer. The
recorder and analyzer recompute both workload HMACs, bind campaign/session/nonce, frozen subject
and run, producer and TTY identity, and require a canonical successful producer event stream with
positive seed and emitted byte counts. Its measurement interval must agree with the render evidence
within 250 ms. The trace's existing `workload_metadata_sha256` and
`workload_ready_receipt_sha256` fields contain the nonzero hashes of those authenticated files.
Missing, mixed, tampered, zero-output, or wrong-session evidence is NOT-RUN.

The other five render cases reject all workload-v3 inputs and require both trace workload hashes
to be the all-zero SHA-256 sentinel. This prevents an output producer from invalidating an idle,
overlay-only, or live-resize measurement.

The driver stream schema is exactly `sequence`, `continuous_ns`, `event_id`, `action`,
`target_pid`, `window_number`, `requested_a`, `requested_b`, `observed_a`, `observed_b`, and
`result`. The freezer validates the stream one-to-one against the immutable plan and hash-binds it,
the pair, subject identity, action video, counts, and continuous measured bounds. It accepts a
verified live-resize row only when each observed delta is within eight pixels of its
requested delta. A verified checkpoint or stop row must observe the target onscreen and must encode
its focus observation as a boolean. Trace v3 metadata must hash-bind the frozen run and authenticated
final render evidence. Its continuous capture start may
precede the measured start by at most two seconds, and its end may follow the measured end by at
most two seconds; the capture span and verified trace duration must agree within 250 ms.

For sustained output, the common producer event stream is separately fixed to `sequence`,
`continuous_ns`, `kind`, `event_id`, `byte_count`, `rows`, `columns`, `pixel_width`,
`pixel_height`, and `status`. Its authenticated lifecycle contains `started`, initial `geometry`,
exactly one `seed-complete`, and a final successful `producer-end`; a metadata-only output claim is
not accepted. The same private campaign secret file authenticates all protocols. Workload-v3 and
the common driver receipt use the file's raw 65-byte ASCII-hex record (including LF), while
render-profile-v1 and the render trace receipt decode its 64 lowercase hexadecimal digits to the
32-byte key. Their key identifiers and HMAC values are therefore protocol-specific.

The analyzer preflights ZIP paths, types, sizes, and compression ratios before bounded extraction.
It requires exactly one `.trace` root, regenerates the TOC and all three exports with the recorder's
exact `xctrace` paths, byte-compares them to the preserved artifacts, reruns the hash-bound trace
verifier on those regenerated exports, and requires its receipt to match byte-for-byte. It validates
the representative PNG with `sips`; an explicitly supplied, receipt-bound `ffprobe` must prove a real video stream with positive dimensions,
packets, decoded head/tail frames, and full duration. Names and non-empty arbitrary bytes are not evidence. At least two Time Profiler rows and one Allocations row
are required. Zero Hangs rows is valid only when the Hangs instrument, target, and full duration are
proven. A main-thread hang over 250 ms is FAIL.

## Campaign trace index

After all 12 captures exist, create one read-only TSV in exactly this scenario order, with
`spaceterm` before `ghostty` for every scenario:

```text
scenario subject subject_identity_sha256 pair_metadata_sha256 campaign_id session_id nonce campaign_manifest_sha256 trace_anchor_receipt_sha256 driver_intent_sha256 driver_receipt_sha256 trace_receipt_sha256 trace_metadata_sha256 trace_artifact_sha256 trace_toc_sha256 trace_verification_sha256 trace_verifier_sha256 time_profiler_artifact_sha256 allocations_artifact_sha256 hangs_artifact_sha256 representative_stack_screenshot_sha256 action_video_sha256
```

The real file uses tabs and contains exactly twelve data rows. All rows use one campaign and
session with a globally unique nonce. Both subjects for a scenario use the same pair-metadata
hash, and each subject identity remains identical across all six cases. Every manifest, anchor,
driver intent/receipt, trace receipt, archived trace, combined Time
Profiler/Allocations/Hangs export-hash tuple, representative screenshot, and action video hash must
be unique: reusing or merely re-archiving one recording or screenshot for another subject or
scenario makes the entire index NOT-RUN.
One individual export may legitimately match (for example, two empty Hangs tables); all three
exports matching is treated as reuse. Each row also binds the preserved TOC, verification receipt,
and verifier executable.

Each render intent binds the immutable exact19 production run intent. After the recorder publishes
its authenticated provisional receipt, the native lifecycle publishes exact35
`performance-run-metadata/v4`; the render trace metadata and anchor bind that final file. Every
subject also supplies an exact14 case report with `format_version=2`, and both subjects are sealed
by one authenticated exact62 pair result with `format_version=3`, `evidence_mode=production`, and
`status=complete`. The pair result trace-archive fields use the canonical
`spaceterm.performance.trace-tree/v1` digest of nested trace files, not the ZIP byte hash.

## Manual call-tree review

Create one TSV per subject and scenario with exactly these keys:

```text
format_version
scenario
subject
plan_sha256
pair_metadata_sha256
run_metadata_sha256
render_intent_sha256
render_workload_metadata_sha256
render_evidence_sha256
campaign_manifest_sha256
trace_anchor_receipt_sha256
driver_intent_sha256
driver_receipt_sha256
trace_receipt_sha256
trace_index_sha256
trace_metadata_sha256
trace_artifact_sha256
trace_toc_sha256
trace_verification_sha256
trace_verifier_sha256
time_profiler_artifact_sha256
allocations_artifact_sha256
hangs_artifact_sha256
representative_stack_screenshot_sha256
action_video_sha256
instruments_version
sampling_settings
call_tree_filters
render_root_symbol
render_root_sample_count
action_video_review
time_profiler_call_tree_checked
allocations_call_tree_checked
hangs_timeline_checked
render_root_text_shaping_stack_present
render_root_path_construction_stack_present
render_root_symbol_plan_construction_stack_present
render_root_row_plan_construction_stack_present
render_root_image_placement_geometry_stack_present
render_root_normal_frame_allocation_stack_present
unchanged_row_reshaping_present
changed_row_proportionality_result
overlay_change_proportionality_result
completed_action_count
exceptional_error_allocations_excluded
reviewer
result
```

Record the exact Instruments version and sampling settings. For SpaceTerm, `render_root_symbol`
must be exactly `TerminalGridElement::paint`. For Ghostty, record the actual analogous render root;
do not claim that the SpaceTerm-only symbol exists. `call_tree_filters` must name that subject's
render root and `render_root_sample_count` must be positive, so an empty filtered call tree cannot
pass. Review the complete action video, the target-only Time Profiler call tree, the Allocations
call tree, and the Hangs timeline. Do not infer a negative stack result from source search.

A PASS review establishes all of the following from the captured call trees, rooted at the
subject-specific render root recorded above:

- no text-shaping stack is rooted in the render root;
- no path, symbol plan, row plan, or image-placement geometry construction is rooted there;
- no normal-frame heap allocation stack is rooted there;
- Cursor-only, text-blink-only, Selection, and Marked Text frames do not reshape unchanged rows;
- shaping and geometry work stay proportional to visible changed rows and genuine overlay changes.

Set each `*_checked` field and both proportionality results to `PASS`. Set every forbidden-stack
and unchanged-row-reshaping field to `false`. Describe how exceptional error-reporting intervals
were excluded from the normal-frame allocation review; `none` or `unchecked` is not evidence.
Any forbidden stack or failed proportionality is FAIL. A missing check, artifact, identity,
duration, action, or manual decision is NOT-RUN.

## Analyzer

Run the analyzer separately for each indexed subject record:

```sh
<frozen-tool-bundle>/scripts/acceptance/analyze-release-render-profile-case.sh \
  --expected-source-commit <reviewed-40-hex-HEAD> \
  --render-tool-bundle-manifest <frozen-tool-bundle>/tool-bundle-manifest.tsv \
  --trusted-source-repository <canonical-reviewed-repository-root> \
  --subject <spaceterm-or-ghostty> \
  --scenario <case-id> \
  --plan <plan.tsv> \
  --plan-metadata <plan-metadata.tsv> \
  --pair-metadata <pair-metadata.tsv> \
  --pair-result <authenticated-pair-result.tsv> \
  --case-report <subject-case-report.tsv> \
  --subject-identity <subject-identity.tsv> \
  --comparison-subject-identity <opposite-subject-identity.tsv> \
  --run-metadata <subject-run-metadata.tsv> \
  --render-intent <render-intent.tsv> \
  --render-evidence <render-evidence.tsv> \
  --render-workload-metadata <render-workload-metadata.tsv> \
  --campaign-secret-file <private-campaign-secret.hex> \
  --driver-events <native-driver-events.tsv> \
  --trace-index <render-profile-trace-index.tsv> \
  --trace-metadata <trace-metadata.tsv> \
  --trace-artifact <archived-trace.zip> \
  --trace-toc <trace-toc.xml> \
  --trace-verification <trace-verification.tsv> \
  --campaign-manifest <authenticated-case-manifest.tsv> \
  --trace-anchor-receipt <authenticated-render-trace-anchor-receipt.tsv> \
  --driver-intent <authenticated-native-driver-intent.tsv> \
  --driver-receipt <authenticated-native-driver-receipt.tsv> \
  --trace-receipt <authenticated-render-trace-receipt.tsv> \
  --driver-binary <immutable-native-driver> \
  --driver-source <native-driver-source> \
  --driver-controller <native-driver-controller> \
  --window-identity <frozen-window-identity.tsv> \
  --driver-plan-start-continuous-ns <positive-uint> \
  --time-profiler-artifact <time-profiler-export> \
  --allocations-artifact <allocations-export> \
  --hangs-artifact <hangs-export> \
  --manual-review <manual-review.tsv> \
  --stack-screenshot <representative-stack-screenshot> \
  --action-video <complete-action-recording> \
  --ffprobe <receipt-bound-absolute-ffprobe-path>
```

The verdict contains no terminal contents or stack symbols beyond the fixed gate names. Retain
the underlying private trace exports and publish only artifacts that pass the campaign privacy
review.

The production analyzer uses the root-owned `/usr/bin/xcrun`, `/usr/bin/sips`, and
`/usr/bin/python3` entry points and requires an explicit absolute `ffprobe` path authenticated by
the render trace receipt; no Homebrew path shape is trusted by itself. Test-command overrides
always force a final NOT-RUN, even when all synthetic fixture checks succeed, so the fixture suite
cannot manufacture profile evidence.
