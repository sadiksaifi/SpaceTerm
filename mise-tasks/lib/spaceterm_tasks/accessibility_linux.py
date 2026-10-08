"""Own private Linux accessibility processes and bounded diagnostic observers."""

from __future__ import annotations

import json
import math
import os
import re
import shutil
import signal
import stat
import statistics
import subprocess
import tempfile
import time
from collections import Counter
from pathlib import Path


class SmokeFailure(Exception):
    """A bounded, content-free failure classification."""


def require(condition, classification):
    if not condition:
        raise SmokeFailure(classification)


def require_prerequisites(backend):
    """Reject missing harness dependencies before building or starting a private session."""
    tools = [
        "dbus-daemon",
        "dbus-run-session",
        "gsettings",
        "tic",
        "zsh",
        "speech-dispatcher",
        "aplay",
        "Xvfb",
        "xdpyinfo",
        "xkbcomp",
        "/usr/libexec/at-spi-bus-launcher",
        "/usr/libexec/dconf-service",
        "/usr/lib/speech-dispatcher-modules/sd_dummy",
    ]
    if backend in ("both", "x11"):
        tools.extend(("xdotool", "import"))
    if backend in ("both", "wayland"):
        tools.append("gnome-shell")
    for tool in tools:
        require(shutil.which(tool) is not None, "prerequisite_tool_missing:" + Path(tool).name)
    schema_names = ["org.gnome.desktop.interface", "org.gnome.desktop.a11y.applications"]
    if backend in ("both", "wayland"):
        schema_names.append("org.gnome.shell")
    # These bindings belong to the system interpreter, not mise's Python.
    probe = subprocess.run(
        [
            "/usr/bin/python3",
            "-I",
            "-c",
            "import dbus, pyatspi, speechd, orca, gi, sys; "
            "gi.require_version('Gdk', '3.0'); "
            "from gi.repository import Gdk, Gio, GLib; "
            "schemas = Gio.SettingsSchemaSource.get_default(); "
            "assert schemas is not None; "
            "assert all(schemas.lookup(name, True) is not None for name in sys.argv[1:])",
            *schema_names,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=10,
    )
    require(probe.returncode == 0, "prerequisite_system_python_bindings_or_schemas_missing")
    require(
        any(Path("/usr/share/vulkan/icd.d").glob("lvp_icd*.json")),
        "lavapipe_driver_missing",
    )


class Processes:
    def __init__(self, registry):
        self.children = []
        self.registry = registry
        self.identities = []
        self.spawning = False
        self.interrupted = False
        self.environment = {key: os.environ[key] for key in ("HOME", "XDG_RUNTIME_DIR")}
        self.save()

    def save(self):
        pending = self.registry.with_suffix(".next")
        pending.write_text(
            json.dumps({"private_environment": self.environment, "children": self.identities})
        )
        pending.replace(self.registry)

    def start(self, command, stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL, env=None):
        # Preserve direct-child ownership until registration completes.
        self.spawning = True
        try:
            child = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                env=env,
                start_new_session=True,
            )
            self.children.append(child)
            identity = process_identity(child.pid)
            require(identity is not None, "direct_child_identity_missing")
            self.identities.append({"pid": child.pid, "start_ticks": identity[2]})
            self.save()
        finally:
            self.spawning = False
        if self.interrupted:
            raise SmokeFailure("session_interrupted")
        return child

    def close(self):
        # Only unreaped direct Popen children are signaled here. The outer
        # wrapper retires private descendants/daemons after this report is saved.
        failed = False
        for child in reversed(self.children):
            try:
                retire_direct_child(child)
            except (OSError, subprocess.TimeoutExpired):
                failed = True
        self.children.clear()
        require(not failed, "direct_child_cleanup_incomplete")

    def stop(self, child):
        retire_direct_child(child)
        self.children.remove(child)


def retire_direct_child(child):
    # If poll returns None, an exiting direct child retains its PID as a zombie
    # until this Popen reaps it. Popen also checks exit before each signal.
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)


def process_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[2]), int(fields[3]), int(fields[19])
    except (OSError, IndexError, ValueError):
        return None


def private_processes(home, runtime):
    # No process names or global same-user command searches. Only the exact
    # initially exported private environment establishes descendant ownership.
    markers = {f"HOME={home}".encode(), f"XDG_RUNTIME_DIR={runtime}".encode()}
    found = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if entry.stat().st_uid != os.getuid():
                continue
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if fields[0] == "Z":
                continue
            environment = set((entry / "environ").read_bytes().split(bytes([0])))
            if markers <= environment:
                found[int(entry.name)] = int(fields[19])
        except (OSError, IndexError, ValueError):
            continue
    return found


def signal_private_process(pid, started, home, runtime, kind):
    try:
        descriptor = os.pidfd_open(pid)
    except ProcessLookupError:
        return "vanished"
    try:
        entry = Path(f"/proc/{pid}")
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if fields[0] == "Z":
                return "vanished"
            environment = set((entry / "environ").read_bytes().split(bytes([0])))
        except (FileNotFoundError, ProcessLookupError):
            return "vanished"
        except PermissionError:
            return "rejected"
        markers = {f"HOME={home}".encode(), f"XDG_RUNTIME_DIR={runtime}".encode()}
        if int(fields[19]) != started or not markers <= environment:
            return "rejected"
        try:
            signal.pidfd_send_signal(descriptor, kind)
            return "signaled"
        except ProcessLookupError:
            return "vanished"
    finally:
        os.close(descriptor)


def create_private_runtime(output):
    runtime = Path(tempfile.mkdtemp(prefix="spaceterm-a11y-")).resolve()
    try:
        identity = runtime.stat()
        scope = output.stat()
        ownership = {
            "runtime_device": identity.st_dev,
            "runtime_inode": identity.st_ino,
            "output_device": scope.st_dev,
            "output_inode": scope.st_ino,
        }
        descriptor = os.open(
            output / "private-runtime.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
        )
        with os.fdopen(descriptor, "w") as stream:
            json.dump(ownership, stream)
    except Exception:
        shutil.rmtree(runtime)
        raise
    return runtime


def require_private_runtime(runtime, output):
    try:
        identity = runtime.lstat()
        scope = output.stat()
        record = output / "private-runtime.json"
        record_identity = record.lstat()
        require(
            runtime.is_absolute()
            and stat.S_ISDIR(identity.st_mode)
            and identity.st_uid == os.getuid()
            and stat.S_IMODE(identity.st_mode) == 0o700
            and stat.S_ISREG(record_identity.st_mode)
            and record_identity.st_uid == os.getuid()
            and stat.S_IMODE(record_identity.st_mode) == 0o600,
            "private_runtime_ownership_missing",
        )
        require(
            json.loads(record.read_text())
            == {
                "runtime_device": identity.st_dev,
                "runtime_inode": identity.st_ino,
                "output_device": scope.st_dev,
                "output_inode": scope.st_ino,
            },
            "private_runtime_ownership_missing",
        )
    except (OSError, ValueError):
        raise SmokeFailure("private_runtime_ownership_missing") from None


def cleanup_private_processes(runtime, home, output):
    require_private_runtime(runtime, output)
    require(home == output / "home", "cleanup_private_scope_invalid")
    proof = {
        "pidfd_identity_checked": True,
        "exact_home_and_runtime_required": True,
        "private_environment": {"HOME": str(home), "XDG_RUNTIME_DIR": str(runtime)},
        "process_group_signals_used": False,
        "passed": False,
    }
    seen = set()
    terminated = set()
    forced = set()
    rejected = set()
    try:
        # Rescan during both stages so daemonization cannot escape one snapshot.
        for kind, signaled in ((signal.SIGTERM, terminated), (signal.SIGKILL, forced)):
            deadline = time.monotonic() + 3
            while True:
                owned = private_processes(home, runtime)
                seen.update(owned.items())
                if not owned:
                    break
                for pid, started in owned.items():
                    identity = (pid, started)
                    if identity not in signaled:
                        result = signal_private_process(pid, started, home, runtime, kind)
                        if result == "signaled":
                            signaled.add(identity)
                        elif result == "rejected":
                            rejected.add(identity)
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.05)
        registry = output / "owned-processes.json"
        records = json.loads(registry.read_text())["children"] if registry.exists() else []
        registered = {(record["pid"], record["start_ticks"]) for record in records}

        def remaining_identities(identities):
            count = 0
            for pid, started in identities:
                identity = process_identity(pid)
                count += identity is not None and identity[2] == started
            return count

        # A terminated orphan may briefly remain as a zombie awaiting reaping.
        # Do not claim zero identities until it is gone; allow a bounded grace.
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and remaining_identities(seen | registered):
            time.sleep(0.05)
        remaining = private_processes(home, runtime)
        registered_remaining = remaining_identities(registered)
        observed_remaining = remaining_identities(seen)
        proof.update(
            observed_private_processes=len(seen),
            terminated_private_processes=len(terminated),
            forced_private_processes=len(forced),
            rejected_identity_checks=len(rejected),
            remaining_private_processes=len(remaining),
            registered_children=len(records),
            remaining_registered_children=registered_remaining,
            remaining_observed_identities=observed_remaining,
        )
        require(
            not remaining and not registered_remaining and not observed_remaining,
            "private_process_cleanup_incomplete",
        )
        proof["passed"] = True
        return proof
    finally:
        (output / "cleanup.json").write_text(json.dumps(proof, indent=2) + "\n")


class LineStream:
    """Consume bounded lines from a FIFO without retaining raw diagnostics."""

    def __init__(self, fifo, probe, consume, discard=None):
        self.fifo = fifo
        os.mkfifo(self.fifo, 0o600)
        self.descriptor = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.buffer = b""
        self.discarding = False
        self.probe = probe
        self.consume = consume
        self.discard = discard
        self.discarded_lines = 0
        probe.observers.append(self.drain)

    def drain(self):
        for _ in range(32):
            try:
                data = os.read(self.descriptor, 65536)
            except BlockingIOError:
                break
            if not data:
                break
            for segment in data.splitlines(keepends=True):
                if not self.discarding:
                    self.buffer += segment
                    if len(self.buffer) > 65536:
                        self.discarded_lines += 1
                        self.buffer = b""
                        self.discarding = True
                        if self.discard is not None:
                            self.discard()
                if segment.endswith(b"\n"):
                    if not self.discarding:
                        self.consume(self.buffer.rstrip(b"\r\n"))
                    self.buffer = b""
                    self.discarding = False

    def close(self):
        self.drain()
        self.probe.observers.remove(self.drain)
        os.close(self.descriptor)
        self.fifo.unlink(missing_ok=True)


class OrcaOutput:
    """Reduce Orca's debug stream to counts and fixture speech verification."""

    def __init__(self, output, probe, marker):
        self.probe = probe
        self.marker = marker.encode()
        self.started = False
        self.terminal_script = False
        self.marker_spoken = False
        self.typed_character_spoken = False
        self.terminal_keyboard_event = False
        self.counts = Counter()
        self.phases = {}
        self.speech_record = False
        self.speech_record_phase = None
        self.speech_record_bytes = 0
        self.speech_backend = Counter()
        self.keyboard_counts = Counter()
        self.input_counts = Counter()
        self.routing_counts = Counter()
        self.routing_phases = {}
        self.fixture_tokens = Counter()
        self.process_counts = Counter()
        self.process_sites = []
        self.process_stream = LineStream(output / "orca-process.fifo", probe, self.process_line)
        self.process_writer = os.open(self.process_stream.fifo, os.O_WRONLY)
        self.stream = LineStream(output / "orca-debug.fifo", probe, self.line, self.discard_record)
        self.fifo = self.stream.fifo

    def discard_record(self):
        # A dropped header may belong to a new non-speech logical record.
        # Its continuations must never inherit the previous speech classification.
        self.speech_record = False
        self.speech_record_phase = None
        self.speech_record_bytes = 0
        self.counts["discarded_record_resets"] += 1

    def line(self, line):
        for kind in ("PRESSED", "RELEASED"):
            if b"vvvvv PROCESS ATSPI_KEY_" + kind.encode() + b"_EVENT:" in line:
                self.input_counts[kind.lower() + "_events"] += 1
        self.input_counts["key_watcher_starts"] += int(
            b"INPUT EVENT MANAGER: Starting key watcher." in line
        )
        for name, marker in (
            (
                "terminal_command_insertions",
                b"TERMINAL: Insertion is believed to be due to terminal command",
            ),
            ("default_routed_insertions", b"TERMINAL: Passing along event to default script."),
            (
                "default_unpresented_insertions",
                b"DEFAULT: Not speaking inserted string due to lack of cause",
            ),
            ("terminal_adjusted_insertions", b"TERMINAL: Adjusted insertion:"),
            (
                "terminal_unadjusted_single_line_insertions",
                b"TERMINAL: Not adjusting single-line insertion.",
            ),
            ("terminal_adjustment_failures", b"TERMINAL: Adjustment failed. Returning any_data."),
        ):
            if marker in line:
                self.routing_counts[name] += 1
                if self.probe.phase is not None:
                    self.routing_phases.setdefault(self.probe.phase, Counter())[name] += 1
        if b"TERMINAL: Adjusted insertion:" in line and self.marker in line:
            self.routing_counts["adjusted_insert_contains_exact_fixture_marker"] += 1
        self.started |= b"ORCA: Starting Atspi main event loop" in line
        self.terminal_script |= (
            b"SCRIPT MANAGER: Setting active script to" in line
            and b"module=orca.scripts.terminal.script" in line
        )
        if self.probe.phase == "keyboard_echo":
            for kind in ("PRESSED", "RELEASED"):
                if b"vvvvv PROCESS ATSPI_KEY_" + kind.encode() + b"_EVENT: 'q' " in line:
                    self.keyboard_counts["typed_key_" + kind.lower() + "_events"] += 1
            self.terminal_keyboard_event |= (
                b"TERMINAL: Presenting keyboard event" in line and line.endswith(b"q")
            )
        # Installed debug.py timestamps logical records and indents every
        # embedded newline by exactly 18 spaces. Inspect only verified speech
        # record parts; retain counts/booleans, never the speech text.
        record = re.match(rb"^[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{6} - (.*)$", line)
        if record is not None:
            body = record[1]
            self.speech_backend["factory_success"] += int(
                body.startswith(b"SPEECH: Using speech server factory:")
            )
            self.speech_backend["not_available"] += int(body.startswith(b"SPEECH: Not available"))
            self.speech_record = body.startswith(b"SPEECH OUTPUT:")
            self.speech_record_phase = self.probe.phase
            self.speech_record_bytes = 0
            if self.speech_record:
                self.counts["speech_records"] += 1
                self.typed_character_spoken |= (
                    self.probe.phase == "keyboard_echo" and body.startswith(b"SPEECH OUTPUT: 'q'")
                )
        elif self.speech_record and line.startswith(b" " * 18):
            body = line[18:]
            self.counts["speech_continuation_lines"] += 1
        else:
            self.speech_record = False
            return
        if not self.speech_record:
            return
        self.speech_record_bytes += len(body)
        self.counts["max_speech_record_bytes"] = max(
            self.counts["max_speech_record_bytes"], self.speech_record_bytes
        )
        self.marker_spoken |= self.marker in body
        for name, marker in (
            ("spaceterm", b"spaceterm"),
            ("a11y", b"a11y"),
            ("spaced_marker", b"spaceterm a11y"),
            ("spoken_eleven", b"eleven"),
            ("spoken_hyphen", b"hyphen"),
            ("spoken_dash", b"dash"),
        ):
            self.fixture_tokens[name] += int(marker in body.lower())
        self.counts["speech_lines"] += 1
        self.counts["max_speech_line_bytes"] = max(self.counts["max_speech_line_bytes"], len(line))
        if self.speech_record_phase is not None:
            counts = self.phases.setdefault(self.speech_record_phase, Counter())
            counts["speech_lines"] += 1
            counts["max_speech_line_bytes"] = max(counts["max_speech_line_bytes"], len(line))

    def process_line(self, line):
        self.process_counts["lines"] += 1
        self.process_counts["tracebacks"] += int(
            line.startswith(b"Traceback (most recent call last):")
        )
        self.process_counts["fatal_python_errors"] += int(line.startswith(b"Fatal Python error:"))
        exception = re.match(rb"^([A-Za-z_][A-Za-z0-9_]*(?:Error|Exception)):", line)
        if exception:
            self.process_counts[exception[1].decode("ascii")] += 1
        site = re.match(rb"^\s+File .*, line ([0-9]+), in ([A-Za-z0-9_<>]+)$", line)
        if site and len(self.process_sites) < 12:
            self.process_sites.append({"function": site[2].decode("ascii"), "line": int(site[1])})

    def close_process_writer(self):
        if self.process_writer is not None:
            os.close(self.process_writer)
            self.process_writer = None

    def summary(self):
        self.stream.drain()
        self.process_stream.drain()
        return {
            "atspi_main_loop_started": self.started,
            "terminal_script_selected": self.terminal_script,
            "fixture_output_spoken": self.marker_spoken,
            "speech_backend": dict(self.speech_backend),
            "speech_backend_initialized": self.speech_backend["factory_success"] > 0
            and self.speech_backend["not_available"] == 0,
            "typed_character_spoken": self.typed_character_spoken,
            "terminal_keyboard_event_observed": self.terminal_keyboard_event,
            "keyboard_monitor": dict(self.keyboard_counts),
            "all_input_monitor": dict(self.input_counts),
            "insert_routing": dict(self.routing_counts),
            "insert_routing_phases": {
                name: dict(counts) for name, counts in self.routing_phases.items()
            },
            "fixture_speech_tokens": dict(self.fixture_tokens),
            "oversized_debug_lines": self.stream.discarded_lines,
            "speech": dict(self.counts),
            "phases": {name: dict(counts) for name, counts in self.phases.items()},
            "process_output": dict(self.process_counts),
            "process_error_sites": self.process_sites,
        }

    def close(self):
        self.close_process_writer()
        self.process_stream.close()
        self.stream.close()


class FrameOutput:
    """Collect the existing GPUI draw/present wall-time measurement."""

    PATTERN = re.compile(rb"^frame duration: ([0-9]+(?:\.[0-9]+)?)(ns|\xc2\xb5s|\xce\xbcs|ms|s)$")
    UNIT_NS = {b"ns": 1, "µs".encode(): 1000, "μs".encode(): 1000, b"ms": 1000000, b"s": 1000000000}

    def __init__(self, output, probe):
        self.phase = None
        self.samples = []
        self.total_frames = 0
        self.startup_errors = 0
        self.window_creation_errors = 0
        self.panic_lines = 0
        self.truncated = False
        self.stream = LineStream(output / "frame-timing.fifo", probe, self.line)
        self.writer = os.open(self.stream.fifo, os.O_WRONLY)

    def line(self, line):
        self.startup_errors += int(line.startswith(b"failed to start SpaceTerm:"))
        self.window_creation_errors += int(
            line.startswith(b"failed to restore the default SpaceTerm window:")
        )
        self.panic_lines += int(b"panicked at" in line)
        match = self.PATTERN.fullmatch(line)
        if match is None:
            return
        self.total_frames += 1
        if self.phase is None:
            return
        if len(self.samples) < 20000:
            self.samples.append(round(float(match[1]) * self.UNIT_NS[match[2]]))
        else:
            self.truncated = True

    def begin(self, phase):
        self.stream.drain()
        self.phase = phase
        self.samples.clear()
        self.truncated = False

    def end(self):
        self.stream.drain()
        self.phase = None
        values = sorted(self.samples)
        require(values, "frame_timing_samples_missing")
        return {
            "frames": len(values),
            "median_ms": round(statistics.median(values) / 1000000, 3),
            "p95_ms": round(values[math.ceil(len(values) * 0.95) - 1] / 1000000, 3),
            "max_ms": round(values[-1] / 1000000, 3),
            "truncated": self.truncated,
            "draw_present_wall_time": True,
            "gpu_timestamp_time": False,
        }

    def close_writer(self):
        if self.writer is not None:
            os.close(self.writer)
            self.writer = None

    def close(self):
        self.close_writer()
        self.stream.close()
