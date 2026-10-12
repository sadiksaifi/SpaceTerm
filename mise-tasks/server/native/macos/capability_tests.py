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
    "probe_timeout_does_not_reset_after_partial_output",
    "probe_output_rejects_paths_environment_values_and_native_errors",
    "preflight_success_without_effect_is_not_passed",
    "gui_and_daemon_results_are_never_coalesced",
    "missing_volume_and_agent_are_unrun_or_unavailable",
    "clipboard_restore_does_not_overwrite_newer_user_change",
    "capture_pixels_and_microphone_samples_are_discarded",
    "touch_id_case_executes_only_usr_bin_true",
    "short_and_extra_output_reject_before_success",
    "cleanup_uncertainty_overrides_effect_success",
    "resource_worker_timeout_does_not_claim_cleanup",
    "unknown_codes_and_reserved_schema_reject",
    "normalized_environment_discloses_counts_not_values",
    "owned_worker_cancellation_is_not_denial_or_success",
    "escaped_descendant_never_counts_as_complete_cleanup",
    "closed_configuration_reader_emits_failure_and_retires_worker",
    "environment_comparison_detects_values_and_order_with_equal_counts",
    "actual_shell_observation_does_not_accept_controlled_command_or_missing_pty",
    "private_environment_transport_preserves_context_and_emits_only_differences",
    "sigpipe_suppression_handles_sockets_and_preserves_existing_pending_signal",
    "private_environment_transport_rejects_oversize_nul_trailing_and_timeout",
)
EVENT_CASES = ("url_receipt_is_independent_of_automation_denial",)
TARGET_CASES = (
    "targets_reject_unbound_companion_window",
    "targets_reject_hidden_bytes_after_volume_terminator",
    "targets_reject_unknown_schema_reserved_and_traversal",
    "closed_configuration_pipe_restores_actual_foreground_pty",
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
