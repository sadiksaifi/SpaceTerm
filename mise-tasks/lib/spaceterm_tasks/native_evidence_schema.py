"""ND520/1 content-free JSONL. Validation supplies facts, never native authority.

Every facts variant has a closed field set. RequiredCase lists name actual native
observations; synthetic or preflight results cannot satisfy them. A complete file
can still contain denials. Callers choose expected outcomes explicitly.
"""

import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, NoReturn, cast

from spaceterm_tasks import TaskError

RECORD_BYTES = 8192
RUN_RECORDS = 4096
RUN_BYTES = 32 * 1024 * 1024
CONTEXTS = frozenset(
    ("gui-parented", "daemon-parented", "headless", "disposable-macos", "disposable-jammy")
)
RESULTS = frozenset(
    (
        "passed",
        "denied",
        "cancelled",
        "unavailable",
        "timed-out",
        "unrun",
        "failed",
        "pending",
        "indeterminate",
        "cleanup-unresolved",
    )
)
FAILURES = frozenset(
    """unsupported-host native-unavailable custody-unavailable account-identity-unavailable
    registration-missing storage-locked storage-unavailable unsafe-native-state invalid-record
    unsupported-schema legacy-registration-requires-migration identity-mismatch generation-mismatch
    stale-assessment confirmation-required authorization-denied authorization-cancelled
    authorization-unavailable installation-unavailable artifact-rejected credential-unavailable
    credential-policy-rejected interaction-required supervisor-unavailable background-items-suppressed
    process-identity-mismatch privilege-drop-unproven execution-binding-unproven timed-out
    limit-exceeded outcome-pending indeterminate cleanup-unresolved io-unavailable""".split()
)
CAPABILITIES = (
    "protected-folders",
    "selected-volume",
    "microphone",
    "screen-recording",
    "accessibility",
    "clipboard-tools",
    "url-opening",
    "apple-events",
    "login-keychain",
    "ssh-agent",
    "gpg-agent",
    "touch-id-sudo",
    "shell-environment",
)
# These identifiers are shared with native emitters. No producer-supplied text is accepted.
CASES_BY_JOB = {
    "native-installation": frozenset(
        """stopped-before-credential-and-retention schema-one-rejects-schema-two-and-trailing-bytes
        schema-two-rejects-truncation-reserved-and-invalid-optionals
        schema-two-identity-mismatch-requires-a-well-formed-record
        uid-reuse-cannot-inherit-native-account-binding discovery-reuses-first-approved-xdg-roots
        stopped-precedes-credential-policy-and-installation-retention
        startup-never-selects-staged-installation installation-binding-covers-full-descriptor
        fresh-copy-does-not-adopt-account-writable-inodes destination-authentication-rejects-substitution
        publication-serializes-competing-installers file-or-directory-sync-failure-is-indeterminate
        repeated-sync-does-not-clear-uncertain-durability journal-crash-at-commit-boundary
        precommit-failure-preserves-old-selection postcommit-failure-never-silently-rolls-back
        stopped-activation-has-no-credential-or-start-effects concurrent-stop-retires-pending-release
        linux-missing-issuer-and-account-incarnation-fail-closed
        systemd-creds-rejects-invalid-secret-output systemd-creds-missing-host-key-never-initializes
        boot-before-login running-survives-logout stopped-survives-reboot storage-unlock-recovery
        desktop-replacement-preserves-server server-update-retains-identity""".split()
    ),
    "macos-keychain": frozenset(
        """persisted-policy-rejects-alternate-decrypt-grant persisted-policy-rejects-prompt-fallback
        persisted-policy-rejects-wrong-retriever-requirement
        persisted-item-rejects-wrong-account-incarnation-and-slot unsupported-auxiliary-acl-is-not-ignored
        interaction-required-retrieval-has-no-prompt-retry policy-inspection-never-reads-secret
        retrieval-requires-live-release-permit framework-secret-allocation-is-wiped-on-every-exit
        ordinary-tests-never-open-or-mutate-system-keychain exact-startup-retrieval
        unprivileged-copy-denied unsigned-copy-denied wrong-identifier-denied other-account-denied
        stopped-retrieval-denied stale-generation-denied""".split()
    ),
    "macos-launcher": frozenset(
        """tracked-launcher-exec-preserves-original-pid-identity
        pid-reuse-and-replacement-exec-reject-release native-proof-authorizes-release
        stop-or-update-between-admission-and-delivery-rejects-release
        secret-delivery-fragmentation-preserves-absolute-deadline confirmation-binds-every-startup-field
        confirmation-failure-prevents-readiness retriever-death-before-ack-prevents-readiness
        exchange-timeout-wipes-and-retires-owned-processes exchange-descriptors-are-cloexec
        terminal-child-inherits-no-secret-or-custody-descriptor unresolved-reaping-overrides-success
        linux-readable-exec-cannot-claim-continuous-nondumpability privilege-drop-failure-never-execs
        missing-native-saved-id-proof-is-unavailable privilege-and-inheritance-proof""".split()
    ),
    "macos-management": frozenset(
        """authorization-denied-cancelled-or-unavailable-has-no-effect
        right-is-bound-to-exact-operation-account-and-build sudo-uid-is-not-native-caller-proof
        sudo-executes-only-fixed-admitted-helper sudo-noninteractive-uses-n
        helper-replacement-before-exec-is-rejected native-error-text-is-not-relayed
        request-bounds-reject-invalid-variants stop-commits-stopped-before-effects
        stop-drains-started-git-write-before-bootout uncertain-stop-is-not-completed
        concurrent-start-stop-and-activate-use-one-native-journal
        restart-rejects-stopped-and-never-selects-staged cancel-retains-original-request-identity
        management-helper-exits-after-bounded-request background-items-suppression
        systemd-249-template locked-storage-wait authorization-denied authorization-cancelled
        authorization-unavailable authorization-granted crash-restart explicit-start approved-restart
        fresh-signed-update second-account-isolation post-filevault-unlock-before-gui-login""".split()
    ),
    "macos-capabilities": frozenset(CAPABILITIES)
    | frozenset(("session-invalidation", "normalized-shell-comparison")),
    "macos-companion": frozenset(
        """permission-explanation-names-requesting-process
        authorization-is-separate-from-destructive-confirmation visible-failure-states
        keyboard-navigation synthetic-target-isolation url-and-apple-event-receipts
        companion-quit-does-not-claim-daemon-cleanup no-automatic-grants""".split()
    ),
    "macos-evidence": frozenset(
        """default-inspection-never-installs-unlocks-or-prompts
        invalid-manifest-never-produces-runnable-runbook collision-refuses-before-first-install-effect
        receipt-is-durable-before-first-effect partial-install-cleanup
        signal-cancellation failed-output-channel cleanup-uncertainty
        bootstrap-authenticates-protected-admin-before-root-exec
        disposable-interactive-cases-unrun signing-input-isolation package-preflight
        receipt-cleanup""".split()
    ),
    "server-distribution": frozenset(
        """client-feed-compatibility client-and-server-feeds-cannot-substitute
        server-feed-signature-precedes-interpretation server-feed-rejects-invalid-fields
        archive-authentication extraction-rejects-unsafe-entries manifest-matches-executable
        server-identifiers release-identity desktop-designated-requirement server-entitlements
        headless-archive credential-tool-source-pin credential-tool-closure lgpl-material
        parallel-signing-serialization publication-assets server-checksum-selection
        jammy-package-execution host-key-mode tpm-mode""".split()
    ),
}
OPERATIONS = frozenset(
    """install stage activate start stop restart uninstall repair collect remote-access
    owner-enrollment""".split()
)
FIELDS = frozenset(
    """schema scope run_id sequence job case host revision dirty artifact_sha256 context proof
    result failure facts""".split()
)
FACT_FIELDS = {
    "lifecycle": "desired_state credential_calls installation_retention_calls process_absent endpoint_absent",
    "credential": "policy interaction retrieved secret_wiped release_rechecked retriever_absent",
    "credential-denial": "reason attempt_observed release_denied retrieved secret_wiped retriever_absent",
    "privilege": "real_ids_verified effective_ids_verified saved_ids_verified supplementary_groups_cleared execution_bound hardened_runtime_verified descriptor_scope_verified secret_channels_closed custody_descriptors_absent retriever_absent root_process_absent root_listener_absent",
    "startup": "desired_state selection trigger readiness_authenticated original_process_retired endpoint_bound",
    "drain": "desired_state started_writes settled_writes phase_order retriever_absent",
    "update": "fresh_version_authenticated approval_durable candidate_running identity_retained pairings_retained organization_retained roots_retained remote_access_retained previous_process_absent sessions_retired started_writes_settled",
    "publication": "fresh_copy destination_authenticated closure_authenticated durability approved_selected",
    "authorization": "operation attempt_observed granted caller_bound helper_absent",
    "capability": "capability preflight effect responsible_process cleanup",
    "environment-comparison": "differences login interactive startup_order_verified",
    "package-verification": "manifest_matches signatures_verified runtime_verified entitlements_verified dependencies_verified",
    "bootstrap": "candidate_manifest_verified candidate_signatures_verified protected_admin_digest_verified protected_admin_signature_verified protected_admin_policy_verified verified_before_root_exec receipt_durable_before_effects root_execution_observed fresh_copy collision_check_complete",
    "cleanup": "receipt_retained scope_verified process_absent retriever_absent sessions_absent endpoint_absent remaining_objects",
}


CREDENTIAL_DENIALS = frozenset(
    "unprivileged-copy-denied unsigned-copy-denied wrong-identifier-denied other-account-denied stopped-retrieval-denied stale-generation-denied".split()
)
STOP_PHASES = (
    "stopped-durable",
    "admissions-closed",
    "writes-settled",
    "sessions-retired",
    "process-absent",
    "endpoint-absent",
    "supervisor-removed",
)
# Only these case-specific contracts can credit decisive native observations.
# Other allowlisted cases can report Unrun or preflight until their schema exists.
CASE_FACTS = {
    ("native-installation", "stopped-before-credential-and-retention"): "lifecycle",
    ("native-installation", "startup-never-selects-staged-installation"): "startup",
    ("macos-keychain", "exact-startup-retrieval"): "credential",
    **dict.fromkeys((("macos-keychain", case) for case in CREDENTIAL_DENIALS), "credential-denial"),
    ("macos-launcher", "privilege-and-inheritance-proof"): "privilege",
    ("macos-management", "stop-drains-started-git-write-before-bootout"): "drain",
    **dict.fromkeys(
        (
            ("macos-management", case)
            for case in ("crash-restart", "explicit-start", "approved-restart")
        ),
        "startup",
    ),
    ("macos-management", "fresh-signed-update"): "update",
    **dict.fromkeys(
        (
            ("macos-management", case)
            for case in (
                "authorization-denied",
                "authorization-cancelled",
                "authorization-unavailable",
                "authorization-granted",
            )
        ),
        "authorization",
    ),
    ("macos-evidence", "bootstrap-authenticates-protected-admin-before-root-exec"): "bootstrap",
    ("macos-evidence", "receipt-cleanup"): "cleanup",
    **dict.fromkeys((("macos-capabilities", case) for case in CAPABILITIES), "capability"),
    ("macos-capabilities", "normalized-shell-comparison"): "environment-comparison",
}


def reject(failure="invalid-record") -> NoReturn:
    raise TaskError(failure) from None


def exact_fields(value: Any, fields):
    if type(value) is not dict or set(value) != set(fields):
        reject()


def integer(value: Any, minimum=0, maximum=4096):
    if type(value) is not int or not minimum <= value <= maximum:
        reject()


def boolean(value: Any):
    if type(value) is not bool:
        reject()


def choice(value: Any, choices):
    if type(value) is not str or value not in choices:
        reject()


def hexadecimal(value: Any, length: int):
    if type(value) is not str or re.fullmatch(f"[0-9a-f]{{{length}}}", value) is None:
        reject()
    if not int(value, 16):
        reject()


def version(value: Any):
    if type(value) is not list or len(value) != 3:
        reject()
    for part in value:
        integer(part, maximum=65535)


def validate_host(host: Any):
    exact_fields(
        host,
        "platform architecture os_version kernel_version libc_version supervisor supervisor_version".split(),
    )
    choice(host["platform"], ("macos", "linux"))
    version(host["os_version"])
    version(host["kernel_version"])
    if host["platform"] == "macos":
        choice(host["architecture"], ("arm64",))
        choice(host["supervisor"], ("launchd",))
        if host["libc_version"] is not None or host["supervisor_version"] is not None:
            reject()
    else:
        choice(host["architecture"], ("x86_64",))
        choice(host["supervisor"], ("systemd",))
        version(host["libc_version"])
        version(host["supervisor_version"])


def validate_facts(facts: Any):
    if type(facts) is not dict:
        reject()
    choice(facts.get("kind"), FACT_FIELDS)
    kind = facts["kind"]
    exact_fields(facts, ("kind", *FACT_FIELDS[kind].split()))
    match kind:
        case "lifecycle":
            choice(facts["desired_state"], ("running", "stopped", "unknown"))
            integer(facts["credential_calls"])
            integer(facts["installation_retention_calls"])
        case "credential":
            choice(facts["policy"], ("accepted", "rejected", "unavailable", "unchecked"))
            choice(facts["interaction"], ("disabled", "required", "unchecked"))
        case "credential-denial":
            choice(facts["reason"], CREDENTIAL_DENIALS)
        case "startup":
            choice(facts["desired_state"], ("running", "stopped", "unknown"))
            choice(facts["selection"], ("approved", "staged", "unknown"))
            choice(facts["trigger"], ("crash", "start", "restart", "none"))
        case "drain":
            choice(facts["desired_state"], ("running", "stopped", "unknown"))
            integer(facts["started_writes"])
            integer(facts["settled_writes"])
            phases = facts["phase_order"]
            if type(phases) is not list or len(phases) > len(STOP_PHASES):
                reject()
            for phase in phases:
                choice(phase, STOP_PHASES)
            if len(set(phases)) != len(phases):
                reject()
        case "publication":
            choice(facts["durability"], ("durable", "not-applied", "indeterminate", "unchecked"))
        case "authorization":
            choice(facts["operation"], OPERATIONS)
        case "capability":
            integer(facts["capability"], 1, 13)
            choice(facts["preflight"], RESULTS)
            choice(facts["effect"], RESULTS)
            choice(
                facts["responsible_process"],
                ("desktop", "server", "probe", "companion", "other", "unknown"),
            )
            choice(facts["cleanup"], ("complete", "unresolved", "not-needed"))
        case "environment-comparison":
            differences = facts["differences"]
            if (
                type(differences) is not list
                or len(differences) > 13
                or any(type(item) is not str for item in differences)
                or differences != sorted(set(cast(list[str], differences)))
            ):
                reject()
            for difference in differences:
                choice(
                    difference,
                    (
                        "path",
                        "locale",
                        "login-shell",
                        "startup-order",
                        "gui-session",
                        "ssh-agent",
                        "gpg-agent",
                        "dbus",
                        "graphical-tools",
                    ),
                )
        case "cleanup":
            integer(facts["remaining_objects"])
    non_boolean = {
        "kind",
        "desired_state",
        "credential_calls",
        "installation_retention_calls",
        "policy",
        "interaction",
        "durability",
        "operation",
        "capability",
        "preflight",
        "effect",
        "responsible_process",
        "cleanup",
        "differences",
        "remaining_objects",
        "reason",
        "selection",
        "trigger",
        "started_writes",
        "settled_writes",
        "phase_order",
    }
    for key in facts.keys() - non_boolean:
        boolean(facts[key])


def validate_record(record: Any):
    exact_fields(record, FIELDS)
    if type(record["schema"]) is not int or record["schema"] != 1:
        reject("unsupported-schema")
    choice(record["scope"], ("native-deployment",))
    hexadecimal(record["run_id"], 32)
    integer(record["sequence"], 1, RUN_RECORDS)
    choice(record["job"], CASES_BY_JOB)
    choice(record["case"], CASES_BY_JOB[record["job"]])
    if len(record["case"]) > 64:
        reject()
    validate_host(record["host"])
    hexadecimal(record["revision"], 40)
    boolean(record["dirty"])
    hexadecimal(record["artifact_sha256"], 64)
    choice(record["context"], CONTEXTS)
    choice(record["proof"], ("synthetic", "preflight", "native"))
    choice(record["result"], RESULTS)
    if record["failure"] is not None:
        choice(record["failure"], FAILURES)
    if record["result"] == "passed" and record["failure"] is not None:
        reject()
    if record["result"] in ("indeterminate", "cleanup-unresolved", "timed-out"):
        if record["failure"] != record["result"]:
            reject()
    validate_facts(record["facts"])
    facts = record["facts"]
    kinds = {
        "native-installation": (
            "lifecycle",
            "publication",
            "credential",
            "package-verification",
            "startup",
        ),
        "macos-keychain": ("credential", "credential-denial"),
        "macos-launcher": ("lifecycle", "credential", "privilege"),
        "macos-management": ("lifecycle", "authorization", "startup", "drain", "update"),
        "macos-capabilities": ("capability", "environment-comparison"),
        "macos-companion": ("capability", "authorization", "cleanup"),
        "macos-evidence": (
            "lifecycle",
            "publication",
            "package-verification",
            "cleanup",
            "bootstrap",
        ),
        "server-distribution": ("package-verification", "publication", "credential"),
    }
    choice(facts["kind"], kinds[record["job"]])
    if record["context"] == "disposable-macos" and record["host"]["platform"] != "macos":
        reject()
    if record["context"] == "disposable-jammy" and record["host"]["platform"] != "linux":
        reject()
    if record["job"] == "macos-capabilities" and record["case"] in CAPABILITIES:
        if facts["kind"] != "capability" or CAPABILITIES[facts["capability"] - 1] != record["case"]:
            reject()
    validate_case_observation(record)
    if record["proof"] == "native" and record["result"] == "passed":
        host = record["host"]
        if host["platform"] == "macos":
            if host["os_version"][0] < 26:
                reject()
        elif (
            host["libc_version"] < [2, 35, 0]
            or host["kernel_version"] < [5, 15, 0]
            or host["supervisor_version"][0] < 249
        ):
            reject()
        match facts["kind"]:
            case "capability":
                if (
                    facts["effect"] != "passed"
                    or facts["cleanup"] == "unresolved"
                    or facts["responsible_process"] == "unknown"
                ):
                    reject()
            case "package-verification":
                if not all(value for key, value in facts.items() if key != "kind"):
                    reject()
            case "cleanup":
                if (
                    not all(
                        facts[key]
                        for key in (
                            "scope_verified",
                            "process_absent",
                            "retriever_absent",
                            "sessions_absent",
                            "endpoint_absent",
                        )
                    )
                    or facts["remaining_objects"]
                ):
                    reject()
            case "publication":
                if (
                    not all(
                        facts[key]
                        for key in (
                            "fresh_copy",
                            "destination_authenticated",
                            "closure_authenticated",
                        )
                    )
                    or facts["durability"] != "durable"
                ):
                    reject()
            case "authorization":
                if not facts["helper_absent"] or (facts["granted"] and not facts["caller_bound"]):
                    reject()
            case "environment-comparison":
                if not all(
                    facts[key] for key in ("login", "interactive", "startup_order_verified")
                ):
                    reject()
            case "credential":
                if facts["retrieved"] and (
                    facts["policy"] != "accepted"
                    or facts["interaction"] != "disabled"
                    or not all(
                        facts[key]
                        for key in ("secret_wiped", "release_rechecked", "retriever_absent")
                    )
                ):
                    reject()
            case "lifecycle":
                if facts["desired_state"] == "stopped" and (
                    facts["credential_calls"]
                    or facts["installation_retention_calls"]
                    or not facts["process_absent"]
                    or not facts["endpoint_absent"]
                ):
                    reject()


def validate_case_observation(record):
    facts = record["facts"]
    result = record["result"]
    if (
        facts["kind"] in ("credential", "credential-denial")
        and result
        in (
            "denied",
            "cancelled",
            "unavailable",
            "unrun",
        )
        and facts["retrieved"]
    ):
        reject()
    if record["proof"] != "native":
        return
    expected = CASE_FACTS.get((record["job"], record["case"]))
    if expected is None:
        if result in ("passed", "denied", "cancelled"):
            reject()
        return
    if facts["kind"] != expected:
        reject()
    # Missing observations remain reportable, but cannot satisfy successful cases.
    if expected == "credential-denial":
        if facts["reason"] != record["case"]:
            reject()
        if result == "passed" or (
            result == "denied"
            and (
                not all(
                    facts[key]
                    for key in (
                        "attempt_observed",
                        "release_denied",
                        "secret_wiped",
                        "retriever_absent",
                    )
                )
                or facts["retrieved"]
                or record["failure"] is None
            )
        ):
            reject()
    elif expected == "authorization":
        outcome = {
            "authorization-denied": ("denied", "authorization-denied"),
            "authorization-cancelled": ("cancelled", "authorization-cancelled"),
            "authorization-unavailable": ("unavailable", "authorization-unavailable"),
            "authorization-granted": ("passed", None),
        }[record["case"]]
        if result in ("passed", "denied", "cancelled", "unavailable"):
            if (
                (result, record["failure"]) != outcome
                or not facts["helper_absent"]
                or not facts["attempt_observed"]
            ):
                reject()
            if facts["granted"] != (result == "passed"):
                reject()
            if result == "passed" and not facts["caller_bound"]:
                reject()
    elif expected == "capability" and result in (
        "passed",
        "denied",
        "cancelled",
        "unavailable",
        "timed-out",
        "unrun",
    ):
        if facts["effect"] != result or facts["cleanup"] == "unresolved":
            reject()
        if (
            result in ("passed", "denied", "cancelled")
            and facts["responsible_process"] == "unknown"
        ):
            reject()
    if result != "passed":
        return
    match expected:
        case "credential":
            if (
                not facts["retrieved"]
                or facts["policy"] != "accepted"
                or facts["interaction"] != "disabled"
                or not all(
                    facts[key] for key in ("secret_wiped", "release_rechecked", "retriever_absent")
                )
            ):
                reject()
        case "privilege":
            # Linux also needs filesystem IDs, capabilities and continuous
            # nondumpability. This macOS observation record cannot credit it.
            if record["host"]["platform"] != "macos" or not all(
                value for key, value in facts.items() if key != "kind"
            ):
                reject()
        case "update" | "bootstrap":
            if not all(value for key, value in facts.items() if key != "kind"):
                reject()
        case "lifecycle":
            if facts["desired_state"] != "stopped":
                reject()
        case "startup":
            trigger = {
                "startup-never-selects-staged-installation": "none",
                "crash-restart": "crash",
                "explicit-start": "start",
                "approved-restart": "restart",
            }[record["case"]]
            if (
                facts["desired_state"] != "running"
                or facts["selection"] != "approved"
                or facts["trigger"] != trigger
                or not all(
                    facts[key]
                    for key in (
                        "readiness_authenticated",
                        "original_process_retired",
                        "endpoint_bound",
                    )
                )
            ):
                reject()
        case "drain":
            if (
                facts["desired_state"] != "stopped"
                or facts["started_writes"] < 1
                or facts["settled_writes"] != facts["started_writes"]
                or facts["phase_order"] != list(STOP_PHASES)
                or not facts["retriever_absent"]
            ):
                reject()


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            reject()
        result[key] = value
    return result


def decode_json(data: bytes, limit=RECORD_BYTES) -> dict[str, Any]:
    if len(data) > limit:
        reject("limit-exceeded")
    try:
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=_pairs,
            parse_constant=lambda _: reject(),
        )
    except (UnicodeError, ValueError, RecursionError):
        reject()
    if type(value) is not dict:
        reject()
    return value


def encode_record(record: dict[str, Any]) -> bytes:
    validate_record(record)
    data = (
        json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()
        + b"\n"
    )
    if len(data) > RECORD_BYTES:
        reject("limit-exceeded")
    return data


@dataclass(frozen=True, slots=True)
class RequiredCase:
    job: str
    case: str
    context: str
    result: str | None = "passed"

    def __post_init__(self):
        choice(self.job, CASES_BY_JOB)
        choice(self.case, CASES_BY_JOB[self.job])
        choice(self.context, CONTEXTS)
        if self.result is not None:
            choice(
                self.result, RESULTS - {"pending", "indeterminate", "cleanup-unresolved", "failed"}
            )


@dataclass(frozen=True, slots=True)
class Collection:
    records: int
    bytes: int
    run_id: str
    passed: int
    unrun: int
    unresolved: int


class Collector:
    """Bounded incremental validation. The sink receives only validated records.

    Failed writes propagate without claiming completion. The orchestration owner
    must run cleanup independently of this output channel.
    """

    def __init__(self):
        self.records = 0
        self.bytes = 0
        self._identity = None
        self._seen: dict[tuple[str, str, str, str], str] = {}
        self.run_id = ""
        self._failure: str | None = None

    def append(self, line: bytes, sink: BinaryIO | None = None):
        if self._failure is not None:
            reject(self._failure)
        try:
            self._append(line, sink)
        except TaskError as error:
            self._failure = str(error) if str(error) in FAILURES else "invalid-record"
            reject(self._failure)
        except (OSError, ValueError):
            self._failure = "io-unavailable"
            reject(self._failure)

    def _append(self, line: bytes, sink: BinaryIO | None):
        if type(line) is not bytes:
            reject()
        if (
            self.records >= RUN_RECORDS
            or len(line) > RECORD_BYTES
            or self.bytes + len(line) > RUN_BYTES
        ):
            reject("limit-exceeded")
        if not line.endswith(b"\n") or b"\n" in line[:-1]:
            reject()
        record = decode_json(line)
        validate_record(record)
        if record["sequence"] != self.records + 1:
            reject()
        identity = tuple(
            record[key] for key in ("run_id", "host", "revision", "dirty", "artifact_sha256")
        )
        if self._identity is not None and identity != self._identity:
            reject("identity-mismatch")
        key = tuple(record[field] for field in ("job", "case", "context", "proof"))
        if key in self._seen:
            reject()
        if sink is not None:
            data = encode_record(record)
            if sink.write(data) != len(data):
                reject("io-unavailable")
        self._identity = identity
        self._seen[key] = record["result"]
        self.run_id = record["run_id"]
        self.records += 1
        self.bytes += len(line)

    def finish(self, required: list[RequiredCase]) -> Collection:
        if self._failure is not None:
            reject(self._failure)
        if not self.records:
            reject("invalid-record")
        for case in required:
            if type(case) is not RequiredCase:
                reject()
            key = (case.job, case.case, case.context, "native")
            if key not in self._seen:
                reject("invalid-record")
            actual = self._seen[key]
            if case.result is None:
                if actual not in ("passed", "denied", "cancelled", "unavailable", "timed-out"):
                    reject("outcome-pending")
            elif actual != case.result:
                reject("outcome-pending")
        values = list(self._seen.values())
        return Collection(
            self.records,
            self.bytes,
            self.run_id,
            sum(result == "passed" and key[3] == "native" for key, result in self._seen.items()),
            values.count("unrun"),
            sum(
                value in ("failed", "pending", "indeterminate", "cleanup-unresolved")
                for value in values
            ),
        )


class EvidenceOutput:
    """An exclusive account-owned output, separate from native cleanup authority."""

    def __init__(self, path: Path):
        self._path = path
        self._file: BinaryIO | None = None
        self._collector = Collector()
        self._finished = False

    def __enter__(self):
        try:
            descriptor = os.open(
                self._path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
            )
            self._file = os.fdopen(descriptor, "wb")
        except FileExistsError:
            reject("unsafe-native-state")
        except OSError:
            reject("io-unavailable")
        return self

    def __exit__(self, _type, _value, _traceback):
        if self._file is not None:
            try:
                self._file.close()
            except OSError:
                reject("io-unavailable")

    def append(self, line: bytes):
        if self._file is None or self._file.closed or self._finished:
            reject("io-unavailable")
        self._collector.append(line, self._file)

    def finish(self, required: list[RequiredCase]) -> Collection:
        if self._file is None or self._file.closed:
            reject("io-unavailable")
        summary = self._collector.finish(required)
        try:
            self._file.flush()
            os.fsync(self._file.fileno())
        except OSError:
            reject("io-unavailable")
        self._finished = True
        return summary


def collect(source: BinaryIO, required: list[RequiredCase]) -> Collection:
    collector = Collector()
    while line := source.readline(RECORD_BYTES + 1):
        collector.append(line)
    return collector.finish(required)
