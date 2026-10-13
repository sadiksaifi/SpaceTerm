#!/usr/bin/env -S python -I
# MISE description="Test capability deadlines and sanitization without native effects or privacy grants"
"""Build the unsigned fixture and execute synthetic C regressions only.

This task never signs, installs, runs a native capability, or modifies privacy,
Keychain, launchd, PAM, or clipboard state. The native executable is build-only.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402

SOURCES = ROOT / "tests" / "server_native" / "macos" / "capabilities"
REQUIRED_CASES = (
    "descriptor_sigpipe_preserves_other_thread_mask_and_pending_signal",
    "probe_timeout_does_not_reset_after_partial_output",
    "probe_output_rejects_paths_environment_values_and_native_errors",
    "preflight_success_without_effect_is_not_passed",
    "gui_and_daemon_results_are_never_coalesced",
    "missing_volume_and_agent_are_unrun_or_unavailable",
    "capture_pixels_and_microphone_samples_are_discarded",
    "touch_id_case_executes_only_usr_bin_true",
    "completed_fixed_tree_is_not_unconditionally_cleanup_unresolved",
    "short_and_extra_output_reject_before_success",
    "cleanup_uncertainty_overrides_effect_success",
    "resource_worker_timeout_does_not_claim_cleanup",
    "unknown_codes_and_reserved_schema_reject",
    "normalized_environment_discloses_counts_not_values",
    "owned_worker_cancellation_is_not_denial_or_success",
    "escaped_descendant_never_counts_as_complete_cleanup",
    "closed_configuration_reader_emits_failure_and_retires_worker",
    "login_shell_values_never_cross_a_worker_process",
    "local_environment_comparison_detects_values_and_order_without_export",
    "public_shell_mode_metadata_is_bounded_and_never_contains_argv",
    "sigpipe_suppression_handles_sockets_and_preserves_existing_pending_signal",
)
CLIPBOARD_CASES = (
    "general_clipboard_synthetic_read_preserves_every_format",
    "intervening_clipboard_change_is_never_restored_or_credited",
    "cancelled_or_expired_clipboard_probe_has_no_effect",
    "unprepared_clipboard_is_not_a_synthetic_effect",
)
TREE_CASES = (
    "retained_fork_parent_identity_finds_reparented_member",
    "triple_fork_unknown_window_parent_remains_unresolved",
    "pid_reuse_never_signals_the_replacement",
    "unrelated_window_activity_is_not_a_member",
    "exec_shadow_bit_does_not_break_ancestry",
    "denied_retirement_never_becomes_complete_cleanup",
    "retirement_has_one_absolute_deadline",
    "post_sentinel_foreign_activity_is_outside_the_fixed_command_window",
)
TREE_NATIVE_CASES = (
    "native_reparented_member_is_identity_retired",
    "native_triple_fork_uncertainty_is_preserved",
    "unprivileged_process_identity_observer_reads_cross_uid_ancestry",
    "native_signal_rechecks_uniqueid_and_pidversion",
)
AGENT_CASES = (
    "modern_gpg_agent_is_queried_without_legacy_environment",
    "replacement_session_refuses_modern_agent_completion",
    "expired_agent_lease_performs_no_discovery_or_query",
)
EVENT_CASES = ("url_receipt_is_independent_of_automation_denial",)
TARGET_CONTRACT_CASES = (
    "native_url_format_matches_the_retained_target_and_rejects_replay",
    "native_accessibility_marker_matches_the_retained_set_only_target",
    "native_clipboard_marker_matches_the_retained_paste_target",
)
TARGET_CASES = (
    "targets_reject_unbound_companion_window",
    "targets_reject_hidden_bytes_after_volume_terminator",
    "targets_reject_unknown_schema_invalid_ids_and_traversal",
    "closed_configuration_pipe_restores_actual_foreground_pty",
    "native_responsible_process_observation_is_not_ancestry_assumption",
    "native_agent_account_and_session_observation_is_stable",
    "native_clipboard_budget_precedes_materialization",
    "native_shell_mode_boundary_never_acquires_parent_environment",
)
NATIVE_FRAMEWORKS = (
    "AppKit",
    "AVFoundation",
    "ScreenCaptureKit",
    "Security",
    "CoreFoundation",
    "ApplicationServices",
    "Carbon",
    "CoreMedia",
    "OpenDirectory",
    "SystemConfiguration",
)


def build_environment(environment):
    """Strip signing inputs and every unrelated variable before native tooling."""
    clean = signing_environment(environment)
    result = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C"}
    for key in ("HOME", "TMPDIR"):
        if key in clean:
            result[key] = clean[key]
    return result


def verify_test_output(output, cases=REQUIRED_CASES):
    """Accept exact executed cases, not raw output or claimed zero-test success."""
    if len(output) > 65536:
        raise TaskError("capability-test-output-limit-exceeded")
    try:
        lines = output.decode("ascii").splitlines()
    except UnicodeError:
        raise TaskError("capability-test-output-invalid") from None
    expected = [f"passed {case}" for case in cases]
    expected.append(f"executed={len(cases)} proof=synthetic native-effects=0")
    if not cases or lines != expected:
        raise TaskError("capability-test-output-invalid")
    return len(cases)


def checked(command, environment, *, timeout=60):
    """Never relay compiler, native exception, or test-child output as diagnostics."""
    try:
        result = subprocess.run(
            command, cwd=ROOT, env=environment, capture_output=True, timeout=timeout
        )
    except subprocess.TimeoutExpired:
        raise TaskError("capability-test-timed-out") from None
    if result.returncode:
        raise TaskError("capability-test-command-failed")
    if len(result.stdout) + len(result.stderr) > 65536:
        raise TaskError("capability-test-output-limit-exceeded")
    return result.stdout


def compiler_arguments(*, sanitizers=False):
    arguments = [
        "/usr/bin/xcrun",
        "--sdk",
        "macosx",
        "clang",
        "-std=c11",
        "-arch",
        "arm64",
        "-mmacosx-version-min=26.0",
        "-Wall",
        "-Wextra",
        "-Werror",
    ]
    if sanitizers:
        arguments.extend(["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"])
    return arguments


def build_fixture(destination, *, sanitizers=False, environment=None):
    """Build-only integration surface for the evidence assembler. No signing."""
    clean = build_environment(os.environ if environment is None else environment)
    arguments = compiler_arguments(sanitizers=sanitizers)
    arguments.extend(
        [
            "-fobjc-arc",
            "-fblocks",
            str(SOURCES / "capability_probe.c"),
            str(SOURCES / "capability_io.c"),
            str(SOURCES / "capability_environment.c"),
            str(SOURCES / "capability_native.m"),
            str(SOURCES / "capability_tree.c"),
            str(SOURCES / "capability_process.c"),
            str(SOURCES / "capability_agents.c"),
            str(SOURCES / "capability_clipboard.c"),
            str(SOURCES / "capability_terminal.c"),
            str(SOURCES / "capability_targets.c"),
            str(SOURCES / "capability_events.c"),
            str(SOURCES / "probe_main.c"),
            *(argument for name in NATIVE_FRAMEWORKS for argument in ("-framework", name)),
            "-Wl,-sectcreate,__TEXT,__info_plist," + str(SOURCES / "ProbeInfo.plist"),
            "-o",
            str(destination),
        ]
    )
    checked(arguments, clean)


def python_tests():
    """Run owned task tests without adding a competing shared-library test file."""
    path = SOURCES / "task_tests.py"
    specification = importlib.util.spec_from_file_location("capability_task_tests", path)
    if specification is None or specification.loader is None:
        raise TaskError("capability-python-tests-unavailable")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    suite = unittest.defaultTestLoader.loadTestsFromModule(module)
    count = suite.countTestCases()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if count == 0 or not result.wasSuccessful():
        raise TaskError("capability-python-tests-failed")
    print(f"executed-python={count} native-effects=0")
    return count


def run_task(*, sanitizers=False):
    if sys.platform != "darwin":
        raise TaskError("capability-macos-tests-unavailable")
    clean = build_environment(os.environ)
    with tempfile.TemporaryDirectory(prefix="spaceterm-capability-tests-") as temporary:
        binary = Path(temporary) / "capability-tests"
        arguments = compiler_arguments(sanitizers=sanitizers)
        arguments.extend(
            [
                str(SOURCES / "capability_probe.c"),
                str(SOURCES / "capability_io.c"),
                str(SOURCES / "capability_environment.c"),
                str(SOURCES / "capability_tests.c"),
                "-o",
                str(binary),
            ]
        )
        checked(arguments, clean)
        count = verify_test_output(checked([str(binary)], clean, timeout=10))
        clipboard = Path(temporary) / "clipboard-tests"
        checked(
            [
                *compiler_arguments(sanitizers=sanitizers),
                str(SOURCES / "capability_clipboard.c"),
                str(SOURCES / "capability_targets.c"),
                str(SOURCES / "capability_probe.c"),
                str(SOURCES / "clipboard_tests.c"),
                "-Wl,-dead_strip",
                "-o",
                str(clipboard),
            ],
            clean,
        )
        count += verify_test_output(checked([str(clipboard)], clean, timeout=10), CLIPBOARD_CASES)
        for name, sources, cases in (
            ("tree-tests", ("capability_tree.c", "tree_tests.c"), TREE_CASES),
            (
                "tree-native-tests",
                (
                    "capability_tree.c",
                    "capability_process.c",
                    "capability_probe.c",
                    "tree_native_tests.c",
                ),
                TREE_NATIVE_CASES,
            ),
        ):
            tree = Path(temporary) / name
            checked(
                [
                    *compiler_arguments(sanitizers=sanitizers),
                    *(str(SOURCES / source) for source in sources),
                    "-Wl,-dead_strip",
                    "-o",
                    str(tree),
                ],
                clean,
            )
            count += verify_test_output(checked([str(tree)], clean, timeout=10), cases)
        agents = Path(temporary) / "agent-tests"
        checked(
            [
                *compiler_arguments(sanitizers=sanitizers),
                *(
                    str(SOURCES / source)
                    for source in (
                        "capability_agents.c",
                        "capability_probe.c",
                        "capability_io.c",
                        "agent_tests.c",
                    )
                ),
                "-Wl,-dead_strip",
                "-o",
                str(agents),
            ],
            clean,
        )
        count += verify_test_output(checked([str(agents)], clean, timeout=10), AGENT_CASES)
        contracts = Path(temporary) / "target-contract-tests"
        checked(
            [
                *compiler_arguments(sanitizers=sanitizers),
                str(SOURCES / "capability_targets.c"),
                str(SOURCES / "target_contract_tests.c"),
                str(SOURCES.parent / "gui" / "companion_receipts.c"),
                str(SOURCES.parent / "gui" / "companion_model.c"),
                "-o",
                str(contracts),
            ],
            clean,
        )
        count += verify_test_output(
            checked([str(contracts)], clean, timeout=10), TARGET_CONTRACT_CASES
        )
        events = Path(temporary) / "event-tests"
        arguments = compiler_arguments(sanitizers=sanitizers)
        arguments.extend(
            [
                str(SOURCES / "capability_events.c"),
                str(SOURCES / "event_tests.c"),
                "-o",
                str(events),
            ]
        )
        checked(arguments, clean)
        count += verify_test_output(checked([str(events)], clean, timeout=10), EVENT_CASES)
        # The target program uses memory recognition and an isolated synthetic
        # PTY. No worker effects, sudo, privacy, Keychain, or supervision APIs.
        targets = Path(temporary) / "target-tests"
        arguments = compiler_arguments(sanitizers=sanitizers)
        arguments.extend(
            [
                "-fobjc-arc",
                "-fblocks",
                str(SOURCES / "capability_probe.c"),
                str(SOURCES / "capability_io.c"),
                str(SOURCES / "capability_native.m"),
                str(SOURCES / "capability_terminal.c"),
                str(SOURCES / "capability_clipboard.c"),
                str(SOURCES / "capability_targets.c"),
                str(SOURCES / "capability_tree.c"),
                str(SOURCES / "capability_process.c"),
                str(SOURCES / "capability_agents.c"),
                str(SOURCES / "capability_environment.c"),
                str(SOURCES / "capability_events.c"),
                str(SOURCES / "target_tests.m"),
                *(argument for name in NATIVE_FRAMEWORKS for argument in ("-framework", name)),
                "-o",
                str(targets),
            ]
        )
        checked(arguments, clean)
        count += verify_test_output(checked([str(targets)], clean, timeout=10), TARGET_CASES)
        terminal = Path(temporary) / "terminal-tests"
        checked(
            [
                *compiler_arguments(sanitizers=sanitizers),
                str(SOURCES / "capability_terminal.c"),
                str(SOURCES / "capability_probe.c"),
                str(SOURCES / "capability_io.c"),
                str(SOURCES / "terminal_tests.c"),
                "-Wl,-dead_strip",
                "-o",
                str(terminal),
            ],
            clean,
        )
        count += verify_test_output(
            checked([str(terminal)], clean, timeout=10),
            ("isolated_sudo_preexec_has_foreground_tty_input_and_signal_delivery",),
        )
        # Build the real effect executable, but never run it during this test gate.
        build_fixture(
            Path(temporary) / "capability-probe", sanitizers=sanitizers, environment=clean
        )
        print(f"executed-c={count} proof=synthetic native-effects=0 native-fixture=build-only")
    python_tests()
    return 0


def entry():
    arguments = sys.argv[1:]
    if arguments == ["--python-tests"]:
        python_tests()
        return 0
    if arguments not in ([], ["--sanitizers"]):
        raise TaskError("capability-test-invalid-arguments")
    return run_task(sanitizers=bool(arguments))


if __name__ == "__main__":
    main(entry)
