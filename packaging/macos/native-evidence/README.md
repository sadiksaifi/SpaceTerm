# Native evidence candidates

The judged evidence route uses the actual packaged server and a genuine signed update, one local macOS owner sequence and one bounded Jammy sequence. It does not install an evidence administrator, ship an evidence companion, automate logout/reboot or provision a spare Mac.

`assembly.py` builds the real server, launcher, manager, credential-free spawner and evidence-only shell entry from Cargo-reported executable paths. It reuses the probe builder and the target views in `tests/server_native/macos/gui/`. `candidate.json` binds the package metadata, exact file bytes/modes and native entry points. These comparison facts supply neither custody nor native startup approval.

## Coordinator preparation

Run trusted task code with `python -I` from the repository. Do not run an interpreter or build tool from an artifact directory. Default `server:native:macos:build-evidence` builds only unsigned inputs in a new directory. Default preparation remains nonmutating and reports unavailable without input.

Build the baseline and newer candidate at their real source revisions, with separate `--output` directories. The pair must have one stable Preflight identity, different version metadata and different server bytes. Copying or relabeling one build cannot satisfy this check.

The coordinator's single signing command is documented in `local-sequence.md`. It uses the existing serialized signing helper and certificate. It imports no new identity. The helper alone receives signing inputs. All other children receive either the explicit build allowlist or the closed native environment. Temporary imports and search-list restoration stay with that helper.

`server:native:macos:prepare-evidence --fixtures <signed-pair-directory>` copies and verifies both candidates and reconstructs their actual server archives. Full signature, runtime, entitlement, arm64 and system-only dependency checks are bound to retained copied files. Unexpected native resources and replace-and-restore races reject. It generates `preflight.json`, `local-sequence.md` and `jammy-sequence.md` for the exact pair. No staging executable is run as root. Protected native publication must authenticate fresh destination copies again before privileged execution.

The schema-1 inspection interface remains readable for earlier callers. Its old admin/disposable shell templates are not the judged execution route. Do not give those templates to the owner in place of the generated local sequence.

## Owner checkpoints and collection

`server:native:macos:run-evidence --profile local` verifies the same prepared pair and prints the next owner checkpoint. Its direct Python entry accepts `--candidate`, `--state`, `--step` and `--outcome`. It records small, append-only owner notes separately from native evidence. One zero-effect Install denial or cancellation permits one retained step-1 retry. An unavailable Install or a second refusal ends the run with exit 2. Cleanup uncertainty requires reviewed recovery; its separately recorded resolution preserves the original failure and supplies no native authority. Step 13 records one bounded authentication-method note per context. It never supplies OS authorization or converts a note into native Passed. Resuming after logout or reboot keeps the candidate, run ID, notes and observation parts. Steps 8 and 14 share one Background Items interaction.

The signed shell entry execs the fixed protected probe in its own PID, preserving the originating shell as parent. The shell supplies only two locally normalized mode bits. The one-attempt target connector consumes the existing companion's channel and accepted nonce-bound receipt. It adds no dashboard or protocol. Accessibility and URL acknowledgement require that actual receipt; URL never uses an Automation fallback.

`collect_evidence.py --observation` retains standalone 20/32-byte wire input as synthetic Pending. The wire has no authenticated producer, run or artifact provenance, so it cannot receive native credit by naming a verified pair. Execution-bound native production and shell-integration startup-order observations are unfinished. `--parts` validates the baseline parts. `--after-update-parts` validates update-produced parts separately against the second package revision, only after the retained step 10 checkpoint. Server-grant retention requires earlier native daemon-parented privacy effects to pass again under the exact update with server responsibility unchanged, plus a native activation observation. Desktop-only effects are comparison evidence, not retention. Earlier files stay unchanged; duplicates within either phase reject. Final collection remains incomplete until native publication, boot, logout, stopped-reboot, Background Items, storage recovery and update-retention facts exist. Partial collection always returns a nonpassing status. Owner notes are not native evidence.

Interrupted setup may require the coordinator's reviewed manual recovery. Preserve the small product-owned inventory and exact file/item/service/process identities. Never remove a protected tree recursively, infer retirement from a utility exit or delete a replacement matching only a name/digest. The local and Jammy sequences describe authorization, collection, cleanup and recovery checkpoints.

## Current execution prerequisites

Independent GPT review of the implementation and exact signed preflight, plus one Opus review of the owner sequence and existing target views, remains required. Nothing here claims that review or native evidence has run.

The current integration snapshot still lacks real `--distribution-metadata` dispatch and store compatibility in the server, the authenticated publisher/product manager composition, credential lifecycle composition and the Jammy custody bodies. Building fails closed at the real metadata query rather than inventing storage versions. These are implementation prerequisites, not owner-run evidence. Do not execute privileged steps until the coordinator integrates them and reviews the exact signed pair.

No signing, System Keychain access, sudo, service installation, TCC change, logout or reboot is part of the ordinary gates. Production native admission remains source-gated and uncredited.
