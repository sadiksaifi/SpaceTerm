#!/usr/bin/python3
# MISE description="Test real Linux accessibility FIFOs and owned-process cleanup"
# USAGE flag "--output-dir <output_dir>" help="Evidence directory; defaults to a fresh temporary directory"
"""Linux regressions for the accessibility smoke observer and private cleanup.

Requires system Python 3.11+, the installed Orca debug.py source and kernel pidfds.
No display, D-Bus, application or screen reader is launched. Retained evidence
stays under --output-dir, or in a fresh temporary directory by default.
"""

from __future__ import annotations

import argparse
import ast
import ctypes
import hashlib
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace

# Loading the smoke module must not write bytecode beside it.
sys.dont_write_bytecode = True

SMOKE = Path(__file__).resolve().parents[1] / "linux.py"


def check(condition, classification):
    if not condition:
        raise AssertionError(classification)


def save(path, proof):
    path.write_text(json.dumps(proof, indent=2) + "\n")


def load_smoke():
    spec = importlib.util.spec_from_file_location("accessibility_smoke_linux", SMOKE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FragmentWriter:
    """Send genuine formatter output through the actual FIFO in bounded pieces."""

    def __init__(self, descriptor, stream, width):
        self.descriptor = descriptor
        self.stream = stream
        self.width = width
        self.fragments = 0

    def writelines(self, strings):
        for string in strings:
            data = string.encode()
            for start in range(0, len(data), self.width):
                piece = memoryview(data)[start : start + self.width]
                while piece:
                    written = os.write(self.descriptor, piece)
                    check(written > 0, "fifo_write_incomplete")
                    piece = piece[written:]
                self.fragments += 1
                self.stream.drain()


def parser_regression(smoke, directory, proof):
    # Compile the unchanged installed formatter only. Importing the Orca package
    # would initialize dependencies unrelated to this bus-free observer test.
    package = importlib.util.find_spec("orca")
    check(
        package is not None and package.submodule_search_locations, "installed_orca_package_missing"
    )
    candidates = [Path(location) / "debug.py" for location in package.submodule_search_locations]
    paths = [path for path in candidates if path.is_file()]
    check(len(paths) == 1, "installed_orca_debug_source_missing")
    orca_debug = paths[0]
    source = orca_debug.read_bytes()
    tree = ast.parse(source)
    definitions = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "_print_text"
    ]
    check(len(definitions) == 1, "installed_formatter_definition_missing")
    formatter = definitions[0]
    proof.update(
        formatter_source=str(orca_debug),
        formatter_function="_print_text",
        formatter_source_sha256=hashlib.sha256(source).hexdigest(),
        formatter_definition_sha256=hashlib.sha256(
            ast.dump(formatter, include_attributes=False).encode()
        ).hexdigest(),
        formatter_definition_line=formatter.lineno,
        real_fifo=True,
        cases={},
    )
    cases = (
        ("singleline", ["SPEECH OUTPUT: 'spaceterm-a11y'"], 4096, True),
        (
            "multiline",
            ["SPEECH OUTPUT: 'first fixture line\nspaceterm-a11y\nlast fixture line'"],
            4096,
            True,
        ),
        (
            "nonspeech_marker",
            ["SPEECH OUTPUT: 'other fixture'", "DEFAULT: nonspeech\nspaceterm-a11y"],
            4096,
            False,
        ),
        (
            "oversized_nonspeech_marker",
            ["SPEECH OUTPUT: 'other fixture'", "DEFAULT: " + "x" * 65537 + "\nspaceterm-a11y"],
            4096,
            False,
        ),
        (
            "fragmented",
            ["SPEECH OUTPUT: 'first fixture line\nspaceterm-a11y\nlast fixture line'"],
            1,
            True,
        ),
    )
    for name, records, width, expected in cases:
        output_dir = directory / name
        output_dir.mkdir()
        # These are the reducers' observation data, with no AT-SPI registry or
        # simulated events. Both real LineStreams are drained through actual FDs.
        context = SimpleNamespace(phase="parser_fixture", observers=[])
        output = None
        writer = None
        evidence = {"passed": False, "expected_marker_spoken": expected}
        proof["cases"][name] = evidence
        try:
            output = smoke.OrcaOutput(output_dir, context)
            writer = os.open(output.fifo, os.O_WRONLY)
            fragmenter = FragmentWriter(writer, output.stream, width)
            namespace = {"datetime": datetime, "debugLevel": 0, "debugFile": fragmenter}
            exec(
                compile(ast.Module(body=[formatter], type_ignores=[]), str(orca_debug), "exec"),
                namespace,
            )
            namespace["_print_text"](
                0, "SPEECH: Using speech server factory: speechdispatcherfactory", True
            )
            for record in records:
                namespace["_print_text"](0, record, True)
            summary = output.summary()
            evidence.update(
                marker_spoken=summary["fixture_output_spoken"],
                discarded_lines=summary["oversized_debug_lines"],
                fragments_written=fragmenter.fragments,
                speech=summary["speech"],
                backend=summary["speech_backend"],
                capture_phase_preserved="parser_fixture" in summary["phases"],
            )
            check(evidence["marker_spoken"] == expected, name + "_marker_mismatch")
            check(summary["speech_backend_initialized"], name + "_factory_record_missing")
            check(evidence["capture_phase_preserved"], name + "_phase_missing")
            if name in ("multiline", "fragmented"):
                check(
                    summary["speech"].get("speech_continuation_lines", 0) == 2,
                    name + "_continuations_missing",
                )
            if name == "oversized_nonspeech_marker":
                check(
                    evidence["discarded_lines"] == 1
                    and summary["speech"].get("discarded_record_resets", 0) == 1,
                    "oversized_header_reset_missing",
                )
            if name == "fragmented":
                check(fragmenter.fragments > 100, "fragmented_delivery_missing")
            evidence["passed"] = True
        finally:
            if writer is not None:
                os.close(writer)
            if output is not None:
                output.close()
            else:
                # Close any real stream created before a constructor failure.
                for observer in list(context.observers):
                    observer.__self__.close()
            evidence["remaining_fifos"] = len(list(output_dir.glob("*.fifo")))
            check(
                not context.observers and not evidence["remaining_fifos"],
                name + "_fifo_cleanup_incomplete",
            )
            save(directory / "proof.json", proof)
    proof["passed"] = True


def retire_owned_child(child):
    """Failure cleanup retains ownership through the direct unreaped Popen."""

    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)


def cleanup_regression(smoke, directory, proof, fail_after_spawn=False):
    # Adopt only this fixture's descendant. This flag neither replaces the
    # production cleaner nor permits signaling unrelated children/processes.
    libc = ctypes.CDLL(None, use_errno=True)
    previous = ctypes.c_int()
    check(libc.prctl(37, ctypes.byref(previous), 0, 0, 0) == 0, "subreaper_state_unavailable")
    home = directory / "home"
    home.mkdir(mode=0o700)
    runtime = smoke.create_private_runtime(directory)
    control_home = directory / "control-home"
    control_runtime = directory / "control-runtime"
    owner = None
    controls = []
    descendant_pid = None
    descendant_started = None
    reaper = None
    reaped = threading.Event()
    stop_reaper = threading.Event()
    original = {key: os.environ.get(key) for key in ("HOME", "XDG_RUNTIME_DIR")}
    environment = dict(os.environ, HOME=str(home), XDG_RUNTIME_DIR=str(runtime))
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_STARTER_ADDRESS", "AT_SPI_BUS_ADDRESS"):
        environment.pop(key, None)
    environment.update(
        DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(runtime / "unused-session"),
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=" + str(runtime / "unused-system"),
    )
    child_code = (
        "import pathlib,signal,sys,time; "
        "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
        "pathlib.Path(sys.argv[1]).touch(); time.sleep(60)"
    )
    launcher = directory / "launcher.py"
    proof.update(no_display_bus_application_or_reader=True)

    def start_reaper():
        def reap():
            while not stop_reaper.is_set():
                try:
                    pid, _ = os.waitpid(descendant_pid, os.WNOHANG)
                except ChildProcessError:
                    pid = 0
                if pid == descendant_pid:
                    reaped.set()
                    return
                time.sleep(0.01)

        thread = threading.Thread(target=reap, daemon=True)
        thread.start()
        return thread

    try:
        control_home.mkdir(mode=0o700)
        control_runtime.mkdir(mode=0o700)
        launcher.write_text(
            "import pathlib,subprocess,sys,time\n"
            "child=subprocess.Popen([sys.executable,'-I','-c',sys.argv[1],sys.argv[2]],"
            "start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,"
            "stderr=subprocess.DEVNULL)\n"
            "ledger=pathlib.Path(sys.argv[3])\n"
            "pending=ledger.with_suffix('.next')\n"
            "pending.write_text(str(child.pid)); pending.replace(ledger)\n"
            "time.sleep(60)\n"
        )
        check(libc.prctl(36, 1, 0, 0, 0) == 0, "subreaper_activation_failed")
        os.environ.update(HOME=str(home), XDG_RUNTIME_DIR=str(runtime))
        owner = smoke.Processes(directory / "owned-processes.json")
        parent = owner.start(
            [
                "/usr/bin/python3",
                "-I",
                str(launcher),
                child_code,
                str(directory / "descendant.ready"),
                str(directory / "descendant.pid"),
            ],
            env=environment,
        )
        deadline = time.monotonic() + 5
        while (
            not (
                (directory / "descendant.ready").exists()
                and (directory / "descendant.pid").exists()
            )
            and time.monotonic() < deadline
        ):
            check(parent.poll() is None, "fixture_parent_exited")
            time.sleep(0.01)
        check(
            (directory / "descendant.ready").exists() and (directory / "descendant.pid").exists(),
            "fixture_descendant_not_ready",
        )
        descendant_pid = int((directory / "descendant.pid").read_text())
        identity = smoke.process_identity(descendant_pid)
        check(
            identity is not None and identity[:2] == (descendant_pid, descendant_pid),
            "fixture_descendant_not_detached",
        )
        descendant_started = identity[2]
        check(
            smoke.private_processes(home, runtime).get(descendant_pid) == descendant_started,
            "fixture_descendant_not_private",
        )
        proof["actual_detached_descendant"] = True
        for overrides in ({"HOME": str(control_home)}, {"XDG_RUNTIME_DIR": str(control_runtime)}):
            control = subprocess.Popen(
                ["/usr/bin/python3", "-I", "-c", "import time; time.sleep(60)"],
                env=dict(environment, **overrides),
                start_new_session=True,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            controls.append(control)
        check(
            smoke.signal_private_process(
                descendant_pid, descendant_started + 1, home, runtime, signal.SIGTERM
            )
            == "rejected",
            "wrong_start_identity_not_rejected",
        )
        for control in controls:
            identity = smoke.process_identity(control.pid)
            check(identity is not None, "negative_control_identity_missing")
            check(
                smoke.signal_private_process(
                    control.pid, identity[2], home, runtime, signal.SIGTERM
                )
                == "rejected",
                "single_marker_control_not_rejected",
            )
        check(all(control.poll() is None for control in controls), "negative_control_not_alive")
        proof.update(
            wrong_start_identity_rejected=True, both_single_marker_controls_rejected_alive=True
        )
        smoke.require(
            smoke.process_identity(descendant_pid)[2] == descendant_started,
            "descendant_identity_changed",
        )
        owner.stop(parent)
        proof["direct_parent_reaped"] = parent.returncode is not None
        reaper = start_reaper()
        check(not fail_after_spawn, "controlled_post_spawn_failure")
        cleanup = smoke.cleanup_private_processes(runtime, home, directory)
        reaper.join(timeout=5)
        check(reaped.is_set(), "detached_descendant_not_reaped")
        check(
            cleanup["passed"]
            and cleanup["terminated_private_processes"] >= 1
            and cleanup["forced_private_processes"] >= 1
            and not cleanup["process_group_signals_used"],
            "genuine_pidfd_cleanup_missing",
        )
        check(
            all(control.poll() is None for control in controls),
            "negative_control_retired_by_private_cleanup",
        )
        proof.update(
            genuine_pidfd_term_and_kill=True,
            negative_controls_alive_after_cleanup=True,
            cleanup=cleanup,
            passed=True,
        )
    finally:
        errors = []
        if owner is not None:
            children = list(owner.children)
            try:
                owner.close()
            except Exception as error:
                errors.append(type(error).__name__)
                for child in children:
                    try:
                        retire_owned_child(child)
                    except Exception as cleanup_error:
                        errors.append(type(cleanup_error).__name__)
        for control in controls:
            try:
                retire_owned_child(control)
            except Exception as error:
                errors.append(type(error).__name__)
        # Recover the owned launcher's ledger even if a readiness assertion failed.
        ledger = directory / "descendant.pid"
        if descendant_pid is None and ledger.exists():
            descendant_pid = int(ledger.read_text())
            identity = smoke.process_identity(descendant_pid)
            if (
                identity is not None
                and smoke.private_processes(home, runtime).get(descendant_pid) == identity[2]
            ):
                descendant_started = identity[2]
        # If setup failed before the ledger appeared, exact private markers
        # still identify this one detached fixture child after its parent retires.
        owned = smoke.private_processes(home, runtime)
        if descendant_pid is None and len(owned) == 1:
            descendant_pid, descendant_started = next(iter(owned.items()))
        if descendant_pid is not None:
            try:
                if descendant_started is not None:
                    smoke.signal_private_process(
                        descendant_pid, descendant_started, home, runtime, signal.SIGKILL
                    )
                if reaper is None:
                    reaper = start_reaper()
                reaper.join(timeout=5)
            except Exception as error:
                errors.append(type(error).__name__)
        stop_reaper.set()
        if reaper is not None:
            reaper.join(timeout=1)
        remaining = smoke.private_processes(home, runtime)
        identity = smoke.process_identity(descendant_pid) if descendant_pid is not None else None
        proof.update(
            remaining_private_processes=len(remaining),
            remaining_descendant_identity=identity is not None
            and (descendant_started is None or identity[2] == descendant_started),
            negative_controls_reaped=all(control.returncode is not None for control in controls),
            cleanup_errors=errors,
        )
        if not remaining and not proof["remaining_descendant_identity"]:
            try:
                shutil.rmtree(runtime)
            except Exception as error:
                errors.append(type(error).__name__)
        proof["runtime_removed"] = not runtime.exists()
        for key, value in original.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        check(libc.prctl(36, previous.value, 0, 0, 0) == 0, "subreaper_restore_failed")
        proof["passed"] = bool(
            proof["passed"]
            and not remaining
            and not proof["remaining_descendant_identity"]
            and proof["negative_controls_reaped"]
            and proof["runtime_removed"]
            and not errors
        )
        save(directory / "proof.json", proof)
    check(proof["passed"], "fixture_process_cleanup_incomplete")


def cleanup_failure_regression(smoke, directory, proof):
    try:
        cleanup_regression(smoke, directory, proof, fail_after_spawn=True)
    except AssertionError as error:
        check(str(error) == "controlled_post_spawn_failure", "unexpected_failure_cleanup_error")
        check(
            proof["actual_detached_descendant"]
            and proof["both_single_marker_controls_rejected_alive"]
            and proof["direct_parent_reaped"],
            "controlled_failure_not_reached",
        )
        check(
            not proof["remaining_private_processes"]
            and not proof["remaining_descendant_identity"]
            and proof["negative_controls_reaped"]
            and proof["runtime_removed"]
            and not proof["cleanup_errors"],
            "controlled_failure_cleanup_incomplete",
        )
        proof.update(expected_post_spawn_failure_observed=True, passed=True)
    else:
        raise AssertionError("controlled_failure_not_raised")


def storage_regression(smoke, directory, proof):
    # Copy just the script into a clean checkout with no sibling review folder.
    checkout = directory / "checkout"
    script = checkout / "mise-tasks/test/accessibility/linux.py"
    script.parent.mkdir(parents=True)
    shutil.copy2(SMOKE, script)
    environment = dict(os.environ)
    temporary = directory / "temporary"
    temporary.mkdir(mode=0o700)
    environment["TMPDIR"] = str(temporary)
    # The missing SHA256 pin stops at the existing binary gate, after output
    # setup and before any build, bus, display, application or reader launch.
    for output in (directory / "outside checkout", Path("relative output"), None):
        arguments = [] if output is None else ["--output-dir", str(output)]
        result = subprocess.run(
            [sys.executable, str(script), "--binary", "unused", *arguments],
            cwd=checkout,
            env=environment,
            capture_output=True,
            text=True,
        )
        check(
            result.returncode != 0 and "existing_binary_sha256_required" in result.stderr,
            "output_rejected_before_binary_gate",
        )
        if output is not None:
            destination = output if output.is_absolute() else checkout / output
            check(any(destination.glob("run-*")), "custom_output_not_created")
    defaults = list(temporary.glob("spaceterm-accessibility-smoke-*"))
    check(len(defaults) == 1 and any(defaults[0].glob("run-*")), "default_output_not_temporary")
    check(not (directory / ".linux-port").exists(), "sibling_folder_created")
    runtime = smoke.create_private_runtime(directory)
    other_runtime = Path(tempfile.mkdtemp(prefix="spaceterm-a11y-"))
    link = directory / "runtime-link"
    link.symlink_to(runtime, target_is_directory=True)

    def rejected(candidate, output=directory):
        try:
            smoke.require_private_runtime(candidate, output)
        except smoke.SmokeFailure as error:
            check(str(error) == "private_runtime_ownership_missing", "wrong_ownership_failure")
        else:
            raise AssertionError("unowned_runtime_accepted")

    try:
        smoke.require_private_runtime(runtime, directory)
        rejected(other_runtime)
        rejected(link)
        wrong_output = directory / "other-output"
        wrong_output.mkdir()
        shutil.copy2(directory / "private-runtime.json", wrong_output / "private-runtime.json")
        rejected(runtime, wrong_output)
        runtime.chmod(0o755)
        rejected(runtime)
        runtime.chmod(0o700)
        smoke.require_private_runtime(runtime, directory)
        # Cleanup must reject an unowned scope before attempting any signals.
        try:
            smoke.cleanup_private_processes(other_runtime, directory / "home", directory)
        except smoke.SmokeFailure as error:
            check(str(error) == "private_runtime_ownership_missing", "wrong_cleanup_scope_failure")
        else:
            raise AssertionError("unowned_cleanup_scope_accepted")
        proof["unowned_symlink_shared_and_wrong_output_runtimes_rejected"] = True
    finally:
        shutil.rmtree(runtime)
        shutil.rmtree(other_runtime)

    proof.update(clean_checkout_output_accepted=True, default_output_temporary=True, passed=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="Evidence directory; defaults to a fresh temporary directory",
    )
    args = parser.parse_args()
    check(sys.platform == "linux", "linux_required")
    check(sys.version_info >= (3, 11), "system_python_3_11_required")
    output = (
        args.output_dir.resolve()
        if args.output_dir
        else Path(tempfile.mkdtemp(prefix="spaceterm-accessibility-regressions-"))
    )
    check(hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"), "pidfd_required")
    descriptor = os.pidfd_open(os.getpid())
    os.close(descriptor)
    output.mkdir(parents=True, exist_ok=True)
    run_dir = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    smoke = load_smoke()
    proof = {
        "linux": True,
        "passed": False,
        "no_display_bus_application_or_reader": True,
        "storage": {"passed": False},
        "parser": {"passed": False},
        "process_cleanup": {"passed": False},
        "process_failure_cleanup": {"passed": False},
    }
    regressions = (
        ("storage", storage_regression),
        ("parser", parser_regression),
        ("process_cleanup", cleanup_regression),
        ("process_failure_cleanup", cleanup_failure_regression),
    )
    for name, regression in regressions:
        directory = run_dir / name
        directory.mkdir()
        try:
            regression(smoke, directory, proof[name])
        except Exception as error:
            proof[name]["passed"] = False
            proof[name]["failure"] = (
                str(error) if isinstance(error, AssertionError) else type(error).__name__
            )
        finally:
            save(directory / "proof.json", proof[name])
            save(run_dir / "proof.json", proof)
    proof["passed"] = all(proof[name]["passed"] for name, _ in regressions)
    save(run_dir / "proof.json", proof)
    print(json.dumps({"proof": str(run_dir / "proof.json"), "passed": proof["passed"]}))
    return int(not proof["passed"])


if __name__ == "__main__":
    raise SystemExit(main())
