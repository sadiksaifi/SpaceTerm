# Local macOS owner sequence

Do not run privileged or disruptive steps in this snapshot. The real server metadata dispatch, authenticated publisher, product manager, credential lifecycle and native scenario producers are absent. The coordinator must integrate those operations, supply their fixed invocations and observation destinations, review the exact signed pair, and obtain one Opus review of this sequence and the existing target views before asking the owner to proceed. No evidence below is credited by preparation or owner notes.

Candidate: `@CANDIDATE@`.
Run ID: `@RUN_ID@`.
Pair manifest SHA-256: `@PAIR_DIGEST@`.

## Coordinator preparation

1. From the trusted checkout, build the baseline and a genuine newer revision with `server:native:macos:build-evidence --output /absolute/new-unsigned-directory`. Keep separate outputs. Both packages must have the Preflight server identity, different version metadata and different server bytes. Do not relabel one build as an update. Missing real distribution metadata refuses assembly.
2. Use the existing `spaceterm_tasks.macos_signing.release_keychain` procedure for the owner's existing signing backup. It reads only `MACOS_SIGNING_CERTIFICATE_P12` and `MACOS_SIGNING_CERTIFICATE_PASSWORD` for the pinned identity. Supply these through the existing private signing-input procedure, never literal shell commands, argv, fixtures or logs. `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and `UPDATE_SIGNING_KEY` are not inputs to this signing command. Do not export them for it.

   The coordinator's one serialized signing command is:

   ```sh
   CARGO_BUILD_JOBS=3 mise exec -- python -I mise-tasks/server/native/macos/build_evidence.py \
     --sign --candidate /absolute/baseline-unsigned --update /absolute/update-unsigned \
     --output /absolute/new-signed-pair
   ```

   Do not generate a signing identity or import material manually. The helper owns temporary imports and search-list restoration. Compiler children receive the build allowlist. Inspection and signing-tool children receive the closed native environment. No product operation runs as root from this staging directory.
3. Run `server:native:macos:prepare-evidence --fixtures /absolute/new-signed-pair` from the trusted checkout. Keep the prepared candidate on persistent storage through logout and reboot. Review both manifests, signatures, preflight and source changes. Preflight records both package revisions and always leaves native admission uncredited.
4. Integrate authenticated fresh publication and fixed product management. The publisher must authenticate protected destination copies, executable dependencies, resources, configuration and ancestors before privileged execution. Approved executables receiving secrets must remain protected against replacement. Exclude account writers and writable aliases. Do not elevate the candidate's executable or an account-writable script, or adopt an account-created tree with chown.
5. Publish the evidence-only `evidence-client`, `capability-probe` and `SpaceTerm Evidence Companion.app` at `/Library/Application Support/SpaceTerm Native Evidence 520` through that authenticated publication interface. Publish the actual server package separately through normal installation. No fixture belongs in the production server archive. This publication operation is unavailable in this snapshot.
6. Prepare run-bound folder and selected-volume markers through the product-owned setup inventory before case 1 or 2. Their format and selection rules are in `tests/server_native/macos/capabilities/integration.txt`. The marker producer and protected inventory composition are unavailable here. Do not make guessed-path markers or proceed without retained created-object identities.
7. Before step 3, arrange a throwaway account through System Settings > Users & Groups with the owner's authorization and the product's scoped inventory. Before steps 5 and 6, arrange a named observer outside this Mac's GUI session that retains native observations across logout and reboot. No such observation route is supplied in this snapshot. Do not enable Remote Login, change FileVault, or provision another machine as an implicit prerequisite.

8. Supply an exact desktop comparator before step 12. The ordinary `preflight:package:macos` output is `dist/SpaceTerm Preflight.app` and is ad hoc signed, so it is not an admitted comparator. The coordinator must name the retained bundle path, source revision and SHA-256, sign that Preflight desktop with the same pinned certificate through the existing helper, and verify hardened runtime with no entitlements. No such signed desktop is included in this pair. The desktop-integration owner must also supply the real app actions that create each PTY kind and the native ancestry check that confirms the selected kind. Neither a Terminal.app shell nor an unsigned development build is a substitute. Until these are supplied, context 1 and the desktop-to-server PTY selection remain unavailable.

## Keep the run, notes and observations

Use an existing GUI-parented interactive login shell in the trusted checkout. Keep the same candidate and state directory throughout the run. This block sets names and creates account-owned output directories only:

```sh
CHECKOUT='@CHECKOUT@'
CANDIDATE='@CANDIDATE@'
RUN_ID='@RUN_ID@'
STATE='@STATE@'
CLIENT='/Library/Application Support/SpaceTerm Native Evidence 520/evidence-client'
umask 077
mkdir -p "$STATE/parts" "$STATE/after-update-parts"
set -C
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/run_evidence.py" \
  --profile local --candidate "$CANDIDATE" --state "$STATE/owner-outcomes.jsonl"
```

`run-evidence` prints the absolute note-file location and next owner step. The default note-file location is the same `$STATE/owner-outcomes.jsonl`. It performs no installation, authorization, logout or reboot. Record a checkpoint only after the named interaction:

```sh
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/run_evidence.py" \
  --profile local --candidate "$CANDIDATE" --state "$STATE/owner-outcomes.jsonl" \
  --step 1 --outcome done
```

Use `done` only when the interaction completed with its native observation retained. Other notes are `denied`, `cancelled`, `unavailable` and `cleanup-unresolved`. Notes never become native Passed. An Install denial or cancellation has zero effects and reports `classification=retry-install`, with step 1 still next. One new step-1 attempt is allowed; its note keeps the earlier refusal. A second refusal or an unavailable Install ends the run with `classification=run-ended` and exit 2. Only `cleanup-unresolved` requires recovery and halts advancement with `classification=recovery-required` and exit 2. A resolution note cannot turn denial, cancellation or unavailable into an installation.

The order is 1, 2, 3, 4, 12, 13, 8 (also covers 14), 9, 10, 5, 6, 7, 11. After logout or reboot, open a GUI login shell in the trusted checkout and rerun the whole block above. Run the reported next step against the original pair. Do not build, sign, prepare, reinstall or make another run ID. Step 1 may be retried only once after its explicit zero-effect denial or cancellation, never after completed Install or uncertain cleanup. Keep all previous notes and observation parts.

## Variables for each PTY

Paste this variable-only block into each desktop-started PTY, each server-started PTY and the separate connector terminal. The prepared sequence supplies the same literal run and state values in every shell. Do not change directory, activate mise, export or replace PATH, or run the coordinator setup block in a tested PTY.

```sh
CLIENT='/Library/Application Support/SpaceTerm Native Evidence 520/evidence-client'
RUN_ID='@RUN_ID@'
STATE='@STATE@'
umask 077
set -C
```

Case 13 uses freshly opened interactive login PTYs in which only this block ran before the mode producer. This keeps the setup itself out of the PATH comparison. Execute checkpoint and collection commands in the coordinator's trusted-checkout shell, not those tested PTYs. Reapply this block in new PTYs after logout or reboot; keep every earlier output.

## Owner checkpoints

For unavailable product operations below, there is no executable substitute in this snapshot. Do not invoke the manager under sudo, use a `security` CLI read, or infer a pass from status text. Once step 1 is unavailable, the run ends and no later step or resolution may be recorded. There is nothing to clean up after zero effects. The later unavailable instructions describe the outcome if an integrated installation exists but that step's producer is still absent.

### Step 1: install and inspect

Actor: coordinator, with the owner's explicit OS authorization.

Unavailable here: the authenticated publisher, registration and LaunchDaemon installation composition. The existing manager entry only returns authorization-unavailable for valid requests. No request encoder, fixed install invocation or native observation destination exists. Do not run it as root. Record `--step 1 --outcome unavailable` using the checkpoint command and stop at `classification=run-ended`. No installation or recovery is implied.

After integration, this step needs a fixed Install operation for the exact baseline, approved account roots and inventory; native protected-copy authentication before root execution; durable registration before supervision; and zero effects on denied or cancelled authorization. The coordinator must supply its invocation before this sequence is executable. If authorization is denied or cancelled with the required zero-effect observation, record that outcome and either stop or retry Install once against the same candidate. Record a new step-1 note after that real retry; never use `--resolve` for it. Uninstall belongs at step 11.

### Step 2: exact credential retrieval

Actor: coordinator's integrated credential lifecycle, with the owner's authorization to create only a synthetic System Keychain item.

Unavailable here: enrollment and the signed startup observation producer. Record `--step 2 --outcome unavailable` if an installation exists, and do not open the System Keychain manually. The integrated operation must retain a fresh slot and exact persistent item identity, prove exact signed retrieval with interaction disabled, revalidate release, wipe bytes and retire the retriever. Never print, paste, capture or store plaintext credentials.

### Step 3: credential denials

Actor: coordinator's signed native scenario producer. The owner authorizes the throwaway account through Users & Groups before its isolated test.

Unavailable here: the fixed scenario invocation and its observation output. Record `--step 3 --outcome unavailable`; do not attempt a substitute item read. The producer must exercise unsigned, wrong-identifier, unprivileged-copy, other-account, Stopped and stale-generation attempts separately, with zero secret release and exact retirement observations. Keep the throwaway account's retained scope for step 11. No real login password is a fixture.

### Step 4: privilege and inheritance

Actor: coordinator's authenticated startup observer.

Unavailable here: the exact service-incarnation observer and native output route. Record `--step 4 --outcome unavailable`. `ps` text cannot replace it. The observation must contain the six account IDs, kernel-visible groups, receiver self-reported groups, code/runtime binding, closed descriptor set, custody-descriptor absence and exact root retriever/listener absence. Terminal children retain ordinary nonroot account groups.

### Step 12: GUI and daemon capability comparison

Authorization checkpoint: the owner makes each permission decision for the actual requesting app. For context 1, select the signed SpaceTerm Preflight desktop app that started the PTY. For context 2, select the protected SpaceTerm Preflight Server app. Do not grant the companion instead, transfer a desktop grant to the server, reset TCC or change PAM.

The probe does not request microphone, Screen Recording, Accessibility or Apple Events authorization. It inspects existing authorization and then attempts the bounded effect where permitted. Do not wait for those prompts. The owner can pre-grant Screen Recording and Accessibility in the panes below. Microphone without a prior grant and Apple Events without an existing Automation grant record Unrun. Automation has no manual Add control. Actual request prompts require the capability owner's request-call implementation; that remains unfinished.

Run each case in the desktop-started interactive PTY (context 1) and the server-started interactive PTY (context 2). The signed protected probe is the same. Use new output names for each attempt and parenting context.

| Case | Capability | Entry | Extra arguments | Owner action |
|---:|---|---|---|---|
| 1 | Protected folders | `input` | None | Requires retained run markers. Check Privacy & Security > Files and Folders for the requesting app; answer a folder prompt only if an actual file open raises it. Missing marker setup is unavailable. |
| 2 | Selected volume | `input` | Selected volume pathname on stdin only | Explicitly select a volume. Check Files and Folders permissions for that app. Missing selection or marker setup is unavailable. |
| 3 | Microphone | `input` | None | Check Privacy & Security > Microphone for that app's existing grant. The probe never requests it. No prior grant means Unrun. |
| 4 | Screen Recording | `targets` | None | In Privacy & Security > Screen & System Audio Recording, enable that exact app before testing. The target window displays the synthetic pattern. |
| 5 | Accessibility | `targets` | None | In Privacy & Security > Accessibility, add or enable that exact app before testing. The target field has identifier `spaceterm-evidence-accessibility-target`. |
| 6 | Clipboard tools | `input` | None | This Darwin profile records unavailable before acquisition. Do not launch a clipboard target or paste personal content. |
| 7 | URL opening | `targets` | None | Keep the existing target open for its nonce-bound Launch Services receipt. There is no Automation fallback. |
| 8 | Apple Events | `targets` | None | Check Privacy & Security > Automation for the requesting app's existing companion grant. No prompt or manual Add operation is implemented. Without that grant, record Unrun. |
| 9 | Login Keychain | `input` | None | The probe uses only a synthetic item in the fixed login Keychain, with interaction disabled. Do not unlock or select another Keychain to force a pass. |
| 10 | SSH agent | `input` | None | Keep an already available local agent selector. No key enumeration, signing request or agent forwarding occurs. |
| 11 | GPG agent | `input` | None | Use the existing native default-location agent. Do not start one or change its configuration for the probe. |
| 12 | Touch ID sudo | `input` | None | Run only at step 13, with the owner present. |
| 13 | Shell environment | `input` | Mode descriptor after target descriptor | Produce the two public mode bits in the actual originating interactive login shell, as shown below. Startup-order evidence is absent and cannot be credited. |

For an input case other than 13:

```sh
"$CLIENT" input "$RUN_ID" 3 "$STATE/gui-3.targets"
"$CLIENT" --probe 3 1 3 3< "$STATE/gui-3.targets" > "$STATE/gui-3.bin"
```

Case 2 takes the owner's explicitly selected UTF-8 volume pathname from stdin, followed by EOF and no newline. Only its private target descriptor contains that path. Empty stdin leaves the case unavailable. Other input cases read no stdin. Case 6 needs no companion; it refuses native general-clipboard acquisition.

For target cases 4, 5, 7 and 8, pretype the probe command below in the tested PTY without pressing Enter. In another desktop-started terminal, start one attempt:

```sh
"$CLIENT" targets "$RUN_ID" 5 "$STATE/gui-5.targets"
```

The Targets window appears automatically when the connector arms the case. Return to the tested PTY without choosing Command-2 or focusing a clipboard control. Request Probe, Request Authorization and Stop Evidence Run abort this one-attempt connector and must not be used. Wait for `classification=armed`, then enter the pretyped command:

```sh
"$CLIENT" --probe 5 1 3 3< "$STATE/gui-5.targets" > "$STATE/gui-5.bin"
```

The attempt closes 60 seconds after the connector starts the launch. That one deadline includes application launch, switching PTYs and the effect. The ordinary probe has a five-second deadline of its own. Context 2 uses `2` in the probe command and new daemon-prefixed files. Capture keeps its pattern visible for the target window's deadline and has no accepted receipt. Accessibility and URL require the matching companion receipt before acknowledgement. Apple Events require the accepted reply echoing both IDs. Quit the target application through its normal UI after each attempt. This does not retire the daemon.

The connector copies no clipboard marker. Do not use the companion clipboard field or its stale Paste prompt. `general_clipboard_acquisition_refuses_oversized_and_preserves_formats` is unavailable in this snapshot: no owner-authorized multi-format preparation and comparison operation is supplied. Case 6 alone records refusal, not that preservation regression or successful read. The coordinator owns the minimal target-view handoff to an Opus implementer: replace the stale prompt and accessibility help in `tests/server_native/macos/gui/companion_windows.m` before owner handoff. This does not restore the dropped dashboard or an accessibility acceptance program.

For case 13, compute mode bits in the actual originating shell, never a script or shell started with `-c`. In zsh:

```sh
mode=00
[[ -o login && -o interactive && -z $ZSH_EXECUTION_STRING && -z $ZSH_SCRIPT ]] && mode=11
"$CLIENT" input "$RUN_ID" 13 "$STATE/gui-13.targets"
"$CLIENT" --probe 13 1 3 0 3< "$STATE/gui-13.targets" \
  < <(printf '%s' "$mode") > "$STATE/gui-13.bin"
"$CLIENT" --environment 1 0 < <(printf '%s' "$mode") > "$STATE/gui-environment.bin"
```

For bash, replace the first two lines with:

```sh
mode=00
if shopt -q login_shell && [[ $- == *i* && ! ${BASH_EXECUTION_STRING+x} && ${#BASH_SOURCE[@]} == 0 ]]; then mode=11; fi
```

Repeat with context 2 and daemon-prefixed outputs in the server-started shell. The client execs the probe in its own PID and leaves the mode descriptor unread. It does not read parent `KERN_PROCARGS2`. The environment output contains only local PATH/locale classifications against the public baseline. It proves neither private-value equality between contexts nor shell-integration startup order. There is no startup-order producer or invocation in this snapshot. Record this checkpoint unavailable until that observation and execution-bound collection exist; do not mark it done from valid wire bytes.

Standalone wire files have no authenticated producer, run or artifact provenance. This read-only import retains them as synthetic Pending, never native Passed or Denied:

```sh
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/collect_evidence.py" \
  --candidate "$CANDIDATE" --input "$STATE/gui-5.bin" --observation \
  --output "$STATE/parts/gui-5.jsonl"
```

The import returns 2. A signed producer must supply execution-bound native JSONL before this matrix can complete collection. Do not relabel synthetic parts or construct native JSONL by hand.

### Step 13: Touch ID and password fallback

Actor: owner in each originating PTY. Make exactly one case-12 attempt per context. Use Touch ID if macOS offers it; otherwise enter the OS password. Cancellation is allowed and is not a completed authentication. The native terminal owns all password bytes.

```sh
"$CLIENT" input "$RUN_ID" 12 "$STATE/gui-12.targets"
"$CLIENT" --probe 12 1 3 3< "$STATE/gui-12.targets" > "$STATE/gui-12.bin"
```

Repeat once with context 2 and daemon-prefixed files. Record the actual method for each context in the step-13 note: `--gui-authentication touch-id|password|none --daemon-authentication touch-id|password|none`. Choose one literal value for each, not the pipe-separated list. Use `none` if that attempt was cancelled, denied or unavailable. A `done` note requires both methods to be `touch-id` or `password`, with native observations retained. These are operator notes, not proof of the method. Only `/usr/bin/sudo -k` and `/usr/bin/sudo /usr/bin/true` run. No AskPass, stdin password parser or password capture is permitted. The native probe requires a fresh controlling PTY, sentinel-bounded uniqueid ancestry, identity-checked signalling, fixed-point retirement and original-terminal restoration. A zero sudo exit does not prove Touch ID. A lost retirement observation is cleanup-unresolved. Standalone wire import remains synthetic Pending, so record unavailable if no execution-bound native producer is integrated.

### Steps 8 and 14: Background Items

Actor: owner. Authorization checkpoint: after the coordinator's native suppression/recovery observer is ready, open System Settings > General > Login Items & Extensions. Toggle the exact SpaceTerm Preflight item off, observe native suppression, then toggle it back on and observe recovery. Record `--step 8 --outcome done` once for both owner IDs only after those records exist. Do not reset all Background Items.

Unavailable here: the product native status producer and its output invocation. Do not toggle settings without that observer. Record `--step 8 --outcome unavailable` if an installation exists.

### Step 9: locked Keychain and unavailable storage

Actor: coordinator's scoped native fixture, with the owner's explicit authorization to affect only synthetic Keychain/storage state.

Unavailable here: the locked synthetic login-Keychain fixture, storage inventory, waiting/recovery producer and their invocations. Record `--step 9 --outcome unavailable`. Do not lock the owner's real login Keychain or unmount real data manually. The product observation must prove no prompt fallback, unprivileged waiting, no underlay creation, unchanged identities and recovery against the original registered roots after unlock.

### Step 10: signed update and retained privacy grants

Actor: coordinator's fixed Stage/Activate product interface. The owner must confirm that activating the update ends every Terminal Session on this server for every client, then supply OS authority.

Unavailable here: the fixed staging/activation invocation, native update records and execution-bound repeated effects. Record `--step 10 --outcome unavailable`. Do not swap installed files manually or select staged code on ordinary Start or crash recovery.

After integration, retain the baseline native matrix in `$STATE/parts`. Keep update-produced native records in `$STATE/after-update-parts`, labelled with the exact update revision from preflight. Record step 10 done only after activation and its native identity, drainage and prior-retirement facts exist. Repeat every previously passed privacy effect in the same context without regranting. Use new names and keep earlier observations. Collection validates the two phases separately, requires the step 10 checkpoint before accepting update records, and computes retained server grants only from execution-bound daemon-parented effects attributed to the exact baseline and updated server, with server responsibility unchanged. Desktop-only effects remain comparison evidence. Decisive retention also requires the native `fresh-signed-update` activation observation; the step-10 note alone supplies no proof. A baseline revision on update output rejects. Unavailable or missing repeated effects leave retention pending.

### Step 5: boot before GUI login

Actor: owner for reboot and FileVault unlock; coordinator's separately arranged observer for native boot/session ordering.

Unavailable here: no external observation route is configured. Record `--step 5 --outcome unavailable` without rebooting. The coordinator must name its existing authorized route, exact read-only invocation and durable output destination before asking for this interaction. A GUI-session process on this Mac cannot observe through reboot.

When that route exists, save work, authorize reboot yourself, unlock FileVault and remain at the login window. The observer must record this registration's authenticated startup before GUI login. FileVault unlock is not GUI login. Log in only after that observation is retained, then rerun the whole resume block. Do not recreate the candidate.

### Step 6: survive logout

Actor: owner for logout; the same independently arranged observer for native process survival and GUI-session adapter invalidation.

Unavailable here: no outside-GUI observation route or native producer exists. Record `--step 6 --outcome unavailable` without logging out. Once the coordinator supplies the route and output, save work and log out yourself. Require current native observations, not cached pre-logout status. Log back in and resume the reported next step, never step 1.

### Step 7: Stop through reboot

Actor: coordinator's fixed Stop/Start interface and reboot-surviving observer, with the owner's confirmation and OS authorization.

Unavailable here: the assessed Stop/Start invocation, reboot observer and native output. Record `--step 7 --outcome unavailable`; do not reboot as a substitute. The product must first prove durable Stopped, fenced releases, closed admissions, settled writes and exact process/Session/retriever/endpoint absence. The owner then authorizes reboot. Native observations must prove Stopped after reboot and client launch, and running only after explicit authorized Start against the same approved installation.

### Step 11: exact cleanup

Actor: coordinator's authenticated product Uninstall/recovery operation, with the owner's scoped destructive confirmation and OS authorization.

Unavailable here: fixed cleanup invocation and protected retained inventory/item owners. If any earlier effect occurred, record `--step 11 --outcome cleanup-unresolved` and follow recovery. With no effects at all, record unavailable if this checkpoint is reachable. Do not attempt pathname-only or recursive cleanup.

The integrated operation must drain writes, prove exact prior process/Session/retriever/endpoint absence, retain and revalidate identities through deletion, and preserve replacements and uncertain objects. Only then remove the scoped LaunchDaemon, registration, synthetic items and protected fixture objects. Delete the throwaway account through Users & Groups only after its work is retired and its exact inventory binding is resolved. Restore only the Background Items and synthetic storage states changed by this run.

## Collection and recovery

Keep the candidate, owner notes and JSONL parts for PR evidence. Initial read-only collection is:

```sh
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/collect_evidence.py" \
  --candidate "$CANDIDATE" --parts "$STATE/parts" --profile local --partial
```

After update activation and retained step 10 notes, collect both sets:

```sh
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/collect_evidence.py" \
  --candidate "$CANDIDATE" --parts "$STATE/parts" \
  --after-update-parts "$STATE/after-update-parts" \
  --state "$STATE/owner-outcomes.jsonl" --profile local --partial
```

Final collection omits `--partial`. It returns 2 with `classification=incomplete` until all required native scenarios, protected-copy publication facts and update retention exist. Boot-before-login, logout survival, Stopped through reboot, Background Items and storage recovery cannot be satisfied by notes. Their decisive observation contracts remain missing in this snapshot, so final collection cannot pass.

On installation, output, identity-publication, retirement or removal uncertainty, stop further effects. Retain the exact protected inventory, signed pair, notes and parts. Never replay completed Install, overwrite a part, delete a receipt, remove a guessed run directory recursively, or treat a successful utility exit as retirement.

The coordinator must review the retained native identities and use an already authenticated product recovery operation or an explicitly reviewed manual recovery step. No such privileged operation is supplied here. A halt is not permission to invent one. A zero-effect cancelled or denied first Install can be retried once as described at step 1; it has no cleanup obligation. An unavailable Install ends the run, and a resolution cannot claim it installed. After recovery establishes a cleanup-unresolved checkpoint's completed outcome, the coordinator records a separate note without deleting the original failure:

```sh
mise -C "$CHECKOUT" exec -- python -I "$CHECKOUT/mise-tasks/server/native/macos/run_evidence.py" \
  --profile local --candidate "$CANDIDATE" --state "$STATE/owner-outcomes.jsonl" \
  --resolve --step 2 --outcome done
```

Replace `2` only with the reported halted step. This is an operator note, not authority, an installation effect or native proof. It rejects another step and preserves earlier bytes. Without a reviewed resolution, the task remains halted.

A refused, timed-out, signalled or malformed target attempt returns through identity-checked cleanup of its socket entries and nonce-named directory. SIGINT, SIGHUP and SIGTERM are blocked in the single connector thread and observed during bounded waits. SIGKILL, machine failure or power loss cannot run cleanup. After a denied or Unrun Accessibility/URL probe, the connector receives no request; it waits until the 60-second deadline and prints `classification=timed-out`. This is expected. Wait for it; do not interrupt or close that terminal. Concurrent same-case Accessibility/URL attempts refuse the probe's existing fixed receipt-socket name. A replaced entry is preserved and reported cleanup-unresolved. Quit any remaining target window through its normal UI. Closing it does not claim protected-product cleanup. Resume only after resolving any uncertainty, against the same candidate and retained outcomes.

If an abrupt loss leaves `/private/tmp/spaceterm-native-520-<run>.<case>.receipt.sock`, stop attempts for that exact run and case. The coordinator must confirm that no live connector retains or listens on that socket, inspect its retained attempt identity and remove only that exact stale socket through a reviewed local recovery step. If holder absence or entry identity cannot be proved, preserve it and record cleanup-unresolved. Never delete all matching sockets or a guessed directory. This exception is for an account-owned capability receipt socket, not a protected product inventory or executor receipt.
