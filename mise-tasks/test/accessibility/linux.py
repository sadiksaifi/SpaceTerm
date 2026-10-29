#!/usr/bin/python3
# MISE description="Verify AT-SPI Terminal text, selection, focus and bounded event churn on private Linux displays"
# USAGE flag "--backend <backend>" help="Display backend" default="both" {
# USAGE   choices "both" "x11" "wayland"
# USAGE }
# USAGE flag "--binary <binary>" help="Existing source executable; skips the client build"
# USAGE flag "--expected-binary-sha256 <sha256>" help="Required SHA256 pin for --binary"
# USAGE flag "--output-dir <output_dir>" help="Evidence directory; defaults to a fresh temporary directory"
# USAGE flag "--session-timeout <seconds>" help="Maximum runtime per backend in seconds" default="600"
# USAGE flag "--isolation-only" help="Verify private bus and dconf ownership without launching a display"
"""Exercise the source Terminal accessibility tree on private Linux displays.

Requires system Python 3.11+, python3-pyatspi, python3-dbus, python3-speechd, at-spi2-core,
Xvfb, xdpyinfo, xdotool, ImageMagick, gnome-shell, Orca and Speech Dispatcher.
Measurements contain counts and timings only.
"""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import traceback
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path

import spaceterm_tasks
from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.accessibility_linux import (
    FrameOutput,
    OrcaOutput,
    Processes,
    Readiness,
    ReadinessStep,
    SmokeFailure,
    cleanup_private_processes,
    create_private_runtime,
    fixture_lines,
    orca_registered,
    require,
    require_prerequisites,
    require_private_runtime,
    retire_direct_child,
    source_session,
    wait_until,
)
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.development_linux import stage_prefix
from spaceterm_tasks.development_supervision import source_environment

TASK_LIBRARY = Path(spaceterm_tasks.__file__).resolve().parents[1]

DISPLAY_NUMBER = 94
WAYLAND_SOCKET = "wayland-spaceterm-94"
MARKER = "spaceterm-a11y"
SCROLLBACK_LIMIT_ROWS = 10000


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=("both", "x11", "wayland"), default="both")
    parser.add_argument(
        "--binary", type=Path, help="Existing source-built executable; skips the client build"
    )
    parser.add_argument("--expected-binary-sha256", help="Required SHA256 pin for --binary")
    parser.add_argument("--server-binary", type=Path, help=argparse.SUPPRESS)
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="Evidence directory; defaults to a fresh temporary directory",
    )
    parser.add_argument(
        "--session-timeout",
        type=int,
        default=600,
        help="Maximum runtime per display backend in seconds",
    )
    parser.add_argument(
        "--isolation-only",
        action="store_true",
        help="Verify private bus and dconf ownership without launching a display",
    )
    parser.add_argument("--private-session", choices=("x11", "wayland"), help=argparse.SUPPRESS)
    parser.add_argument("--source-session", choices=("x11", "wayland"), help=argparse.SUPPRESS)
    return parser.parse_args()


def run(command, timeout=30, **kwargs):
    result = subprocess.run(
        command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=timeout, **kwargs
    )
    require(result.returncode == 0, "subprocess_failed")


class Probe:
    def __init__(self):
        import pyatspi
        from gi.repository import GLib

        self.atspi = pyatspi
        self.context = GLib.MainContext.default()
        self.terminal = None
        self.application_pid = None
        self.server_workspace_before = None
        self.server_workspace_row = None
        self.server_workspace_name = None
        self.server_workspace_named = False
        self.phase = None
        self.counts = Counter()
        self.source_counts = Counter()
        self.insert_marker = False
        self.marker_event_offsets = []
        self.observers = []
        pyatspi.Registry.registerEventListener(
            self.event,
            "object:text-changed",
            "object:text-caret-moved",
            "object:text-selection-changed",
            "object:state-changed:focused",
            "focus:",
        )

    def drain(self):
        # Bound each drain so continuous output cannot starve timeout handling.
        for _ in range(1000):
            if not self.context.pending():
                break
            self.context.iteration(False)
        for observer in self.observers:
            observer()

    def wait(self, predicate, classification, timeout=30):
        return wait_until(self.drain, predicate, classification, time.monotonic() + timeout)

    def settle(self, seconds=0.3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.drain()
            time.sleep(0.01)

    def find_terminal(self):
        # A process-owned terminal can still belong to retained account work.
        # Only the exact newly created and named Workspace is a workload target.
        if not self.server_workspace_named:
            return None
        selected = [
            row
            for row in self.workspace_rows()
            if row.getState().contains(self.atspi.STATE_SELECTED)
        ]
        if len(selected) != 1 or selected[0].name != self.server_workspace_name:
            return None
        window = self.find_window(self.application_pid)
        if window is None:
            return None
        stack = [window]
        visited = 0
        terminals = []
        while stack and visited < 5000:
            accessible = stack.pop()
            visited += 1
            if (
                accessible.getRole() == self.atspi.ROLE_TERMINAL
                and accessible.name == "Terminal Pane"
            ):
                terminals.append(accessible)
            stack.extend(accessible)
        require(len(terminals) <= 1, "server_workspace_terminal_ambiguous")
        return terminals[0] if terminals else None

    def find_window(self, pid):
        for application in self.atspi.Registry.getDesktop(0):
            if application.get_process_id() != pid:
                continue
            for child in application:
                if child.getRole() == self.atspi.ROLE_FRAME:
                    return child
        return None

    def find_named_control(self, name, root=None):
        desktop = self.atspi.Registry.getDesktop(0)
        desktop.clear_cache()
        for application in desktop:
            application.clear_cache()
            if application.get_process_id() != self.application_pid:
                continue
            stack = [root] if root is not None else list(application)
            visited = 0
            while stack and visited < 5000:
                accessible = stack.pop()
                accessible.clear_cache()
                visited += 1
                if accessible.name == name:
                    return accessible
                stack.extend(accessible)
        return None

    def activate_control(self, name, root=None):
        control_id = {
            "New Workspace": "new_workspace",
            "Switch Workspace": "switch_workspace",
            "Local Workspace": "local_workspace",
            "Rename Workspace": "rename_workspace",
            "Kill Workspace": "kill_workspace",
        }[name]
        control = self.wait(
            lambda: self.find_named_control(name, root),
            "server_workspace_control_missing:" + control_id,
        )
        self.press_control(control)

    def press_control(self, control):
        actions = control.queryAction()
        for index in range(actions.nActions):
            if actions.getName(index).lower() in ("click", "press", "activate"):
                require(actions.doAction(index), "server_workspace_control_action_failed")
                return
        raise SmokeFailure("server_workspace_control_action_missing")

    def workspace_rows(self):
        tree = self.wait(
            lambda: self.find_named_control("Workspaces"), "server_workspace_list_missing"
        )
        rows, stack, visited = [], [tree], 0
        while stack and visited < 5000:
            node = stack.pop()
            visited += 1
            node.clear_cache()
            if node.getRole() == self.atspi.ROLE_TREE_ITEM:
                rows.append(node)
            stack.extend(node)
        return rows

    def created_workspace_row(self):
        if self.server_workspace_before is None:
            return None
        rows = [
            row
            for row in self.workspace_rows()
            if row.name == self.server_workspace_name
            and row.getState().contains(self.atspi.STATE_SELECTED)
        ]
        require(len(rows) <= 1, "server_workspace_creation_ambiguous")
        return rows[0] if rows else None

    def open_workspace_menu(self, row, input_driver):
        def row_has_state(state):
            row.clear_cache()
            return row.getState().contains(state)

        row.clear_cache()
        if not row.getState().contains(self.atspi.STATE_SELECTED):
            self.press_control(row)
        self.wait(
            lambda: row_has_state(self.atspi.STATE_SELECTED),
            "server_workspace_not_selected",
        )
        require(row.queryComponent().grabFocus(), "server_workspace_focus_rejected")
        self.wait(
            lambda: row_has_state(self.atspi.STATE_FOCUSED),
            "server_workspace_not_focused",
        )
        input_driver.context_menu()

    def retire_server_workspace(self, input_driver):
        if self.server_workspace_before is None:
            return
        # Native proxies can be replaced. Only the unique name supplied at creation proves
        # fixture ownership; a proxy absent from an earlier tree can still be retained work.
        input_driver.escape()
        self.wait(
            lambda: self.find_named_control("Workspace name") is None,
            "server_workspace_rename_not_dismissed",
        )
        rows = self.workspace_rows()
        named = [row for row in rows if row.name == self.server_workspace_name]
        require(len(named) <= 1, "server_workspace_cleanup_ambiguous")
        require(named, "server_workspace_cleanup_identity_missing")
        row = named[0]
        name = row.name
        self.open_workspace_menu(row, input_driver)
        self.activate_control("Kill Workspace")
        dialog = self.wait(
            lambda: self.find_named_control(f'Kill Workspace "{name}"?'),
            "server_workspace_confirmation_missing",
        )
        self.activate_control("Kill Workspace", dialog)
        self.wait(
            lambda: all(
                current != row and current.name != self.server_workspace_name
                for current in self.workspace_rows()
            ),
            "server_workspace_cleanup_not_confirmed",
        )

    def open_server_workspace(self, input_driver):
        # Exercise the native creation controls. The fixture's named client owns its
        # presentation; this source run's foreground Server owns the PTY.
        self.server_workspace_name = os.environ["SPACETERM_DEVELOPMENT_CLIENT"]
        rows = self.workspace_rows()
        require(
            all(row.name != self.server_workspace_name for row in rows),
            "server_workspace_fixture_name_in_use",
        )
        # Supply the unique name in the creation request. Native proxy replacement cannot
        # prove which unnamed row is new, and renaming a guessed row can touch retained work.
        self.activate_control("Switch Workspace")
        field = self.wait(
            lambda: self.find_named_control("Workspace name"), "server_workspace_query_missing"
        )
        input_driver.workspace_query(self.server_workspace_name, field)
        self.wait(
            lambda: field.queryText().getText(0, -1) == self.server_workspace_name,
            "server_workspace_query_not_published",
        )
        self.server_workspace_before = rows
        self.activate_control("Local Workspace")
        self.server_workspace_row = self.wait(
            self.created_workspace_row, "server_workspace_created_row_missing"
        )
        self.server_workspace_named = True

        # WorkspaceManager::render_chrome derives this selected row and the single presented
        # Tab manager from the same Active Workspace. This fresh client never navigates the
        # sidebar cursor, so the named selection identifies the content of this Window too.
        def fixture_pane():
            require(self.created_workspace_row() is not None, "fixture_workspace_not_selected")
            window = self.find_window(self.application_pid)
            return self.find_terminal() or (
                self.find_named_control("Start New Shell", window) if window is not None else None
            )

        pane = self.wait(
            fixture_pane,
            "server_workspace_pane_missing",
        )
        if pane.name == "Start New Shell":
            self.press_control(pane)
        self.terminal = self.wait(self.find_terminal, "server_terminal_object_missing")
        require(self.terminal.queryComponent().grabFocus(), "server_terminal_focus_rejected")

    def event(self, event):
        if self.terminal is not None and event.type.startswith("object:text-changed"):
            try:
                if event.source.getRole() == self.atspi.ROLE_TERMINAL:
                    self.source_counts["terminal_text_events"] += 1
                    if (
                        self.application_pid is not None
                        and event.source.get_application().get_process_id() == self.application_pid
                    ):
                        self.source_counts["process_owned_terminal_text_events"] += 1
                    self.source_counts[
                        "matching_source_events"
                        if event.source == self.terminal
                        else "other_source_events"
                    ] += 1
                    self.source_counts[
                        "string_payload_events"
                        if isinstance(event.any_data, str)
                        else "other_payload_events"
                    ] += 1
            except Exception:
                self.source_counts["source_query_errors"] += 1
        if self.terminal is None or event.source != self.terminal:
            return
        kind = event.type
        if kind.startswith("object:text-changed:insert") and isinstance(event.any_data, str):
            self.insert_marker |= MARKER in event.any_data
            if MARKER in event.any_data and len(self.marker_event_offsets) < 8:
                self.marker_event_offsets.append(
                    {
                        "start": int(event.detail1),
                        "count": int(event.detail2),
                        "payload_scalars": len(event.any_data),
                    }
                )
        if self.phase is None:
            return
        self.counts["events"] += 1
        if kind.startswith("object:text-changed:"):
            operation = "insert" if ":insert" in kind else "delete"
            count = max(0, int(event.detail2))
            self.counts[operation + "_events"] += 1
            self.counts[operation + "_characters"] += count
            self.counts["max_" + operation + "_characters"] = max(
                self.counts["max_" + operation + "_characters"], count
            )
        elif kind.startswith("object:text-caret-moved"):
            self.counts["caret_events"] += 1
        elif kind.startswith("object:text-selection-changed"):
            self.counts["selection_events"] += 1
        else:
            self.counts["focus_events"] += 1

    def begin(self, phase):
        self.settle()
        self.phase = phase
        self.counts.clear()
        self.started = time.monotonic()
        self.document_characters = self.terminal.queryText().characterCount

    def end(self):
        self.settle()
        result = dict(self.counts)
        result["elapsed_ms"] = round((time.monotonic() - self.started) * 1000)
        result["document_characters_before"] = self.document_characters
        result["document_characters_after"] = self.terminal.queryText().characterCount
        self.phase = None
        return result

    def contains(self, marker):
        return marker in self.terminal.queryText().getText(0, -1)


class Input:
    def __init__(self, backend, probe):
        self.backend = backend
        self.probe = probe
        self.remote = None
        if backend == "wayland":
            import dbus

            self.bus = dbus.SessionBus()
            manager = dbus.Interface(
                self.bus.get_object(
                    "org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop"
                ),
                "org.gnome.Mutter.RemoteDesktop",
            )
            path = manager.CreateSession()
            self.remote = dbus.Interface(
                self.bus.get_object("org.gnome.Mutter.RemoteDesktop", path),
                "org.gnome.Mutter.RemoteDesktop.Session",
            )
            introspection = dbus.Interface(
                self.bus.get_object("org.gnome.Mutter.RemoteDesktop", path),
                "org.freedesktop.DBus.Introspectable",
            )
            definition = ET.fromstring(introspection.Introspect())
            method = definition.find(
                ".//interface[@name='org.gnome.Mutter.RemoteDesktop.Session']"
                "/method[@name='NotifyKeyboardKeysym']"
            )
            require(method is not None, "wayland_keyboard_method_missing")
            signature = "".join(
                argument.attrib["type"]
                for argument in method.findall("arg")
                if argument.attrib.get("direction", "in") == "in"
            )
            require(signature in ("ub", "a{sv}ub"), "wayland_keyboard_signature_unexpected")
            self.key_options = (
                dbus.Dictionary({}, signature="sv") if signature == "a{sv}ub" else None
            )
            self.remote.Start()
            # Mutter drops the first key of a newly created virtual keyboard.
            self.key(0xFFE1)
            # Private GNOME starts in its overview; return to the app Window.
            self.key(0xFF1B)
            self.probe.drain()

    def key(self, keysym):
        for pressed in (True, False):
            arguments = (
                (keysym, pressed)
                if self.key_options is None
                else (self.key_options, keysym, pressed)
            )
            self.remote.NotifyKeyboardKeysym(*arguments)

    def escape(self):
        if self.backend == "x11":
            run(["xdotool", "key", "--clearmodifiers", "Escape"])
        else:
            self.key(0xFF1B)

    def context_menu(self):
        if self.backend == "x11":
            run(["xdotool", "key", "--clearmodifiers", "Menu"])
        else:
            self.key(0xFF67)

    def character(self, pressed):
        self.require_fixture()
        if self.backend == "x11":
            run(["xdotool", "keydown" if pressed else "keyup", "--clearmodifiers", "q"])
        else:
            arguments = (
                (ord("q"), pressed)
                if self.key_options is None
                else (self.key_options, ord("q"), pressed)
            )
            self.remote.NotifyKeyboardKeysym(*arguments)

    def backspace(self):
        self.require_fixture()
        if self.backend == "x11":
            run(["xdotool", "key", "--clearmodifiers", "BackSpace"])
        else:
            self.key(0xFF08)

    def keyboard_barrier(self):
        self.require_fixture()
        if self.backend == "x11":
            run(["xdotool", "key", "--clearmodifiers", "Shift_R"])
        else:
            self.key(0xFFE2)

    def command(self, command, *, wait_for_echo=False):
        self.require_fixture()
        self._submit_text(command, wait_for_echo=wait_for_echo)

    def require_fixture(self):
        require(
            self.probe.server_workspace_named and self.probe.terminal is not None,
            "fixture_terminal_not_selected",
        )

    def workspace_query(self, name, field):
        require(
            field.queryComponent().grabFocus(),
            "server_workspace_query_focus_rejected",
        )

        def focused():
            field.clear_cache()
            return field.getState().contains(self.probe.atspi.STATE_FOCUSED)

        self.probe.wait(focused, "server_workspace_query_not_focused")
        self._type_text(name)

    def _type_text(self, command):
        if self.backend == "x11":
            run(["xdotool", "type", "--clearmodifiers", "--delay", "2", "--", command])
        else:
            for char in command:
                code = ord(char)
                self.key(code if code < 256 else 0x01000000 | code)
                self.probe.drain()
                time.sleep(0.003)

    def _submit_text(self, command, *, wait_for_echo=False):
        self._type_text(command)

        def command_echoed():
            self.probe.terminal.clear_cache()
            text = self.probe.terminal.queryText()
            caret = text.caretOffset
            return caret >= len(command) and text.getText(caret - len(command), caret) == command

        # Native key delivery precedes the paced Terminal text publication.
        # Keep the command echo separate from its output insertion for Orca.
        if wait_for_echo:
            self.probe.wait(command_echoed, "shell_command_echo_missing")
        if self.backend == "x11":
            run(["xdotool", "key", "--clearmodifiers", "Return"])
        else:
            self.key(0xFF0D)

    def screenshot(self, destination):
        if self.backend == "x11":
            run(["import", "-window", "root", str(destination)])
        else:
            import dbus

            # Shell permits MediaKeys on both installed screenshot APIs; newer
            # installations no longer permit org.gnome.Screenshot. This name
            # belongs only to our verified private bus. Never replace or queue
            # behind another owner.
            try:
                name = dbus.service.BusName(
                    "org.gnome.SettingsDaemon.MediaKeys", bus=self.bus, do_not_queue=True
                )
            except dbus.DBusException:
                raise SmokeFailure("private_screenshot_client_name_unavailable") from None
            screenshot = dbus.Interface(
                self.bus.get_object("org.gnome.Shell", "/org/gnome/Shell/Screenshot"),
                "org.gnome.Shell.Screenshot",
            )
            try:
                success, _ = screenshot.Screenshot(False, False, str(destination))
                require(success, "screenshot_failed")
            except dbus.DBusException:
                raise SmokeFailure("screenshot_api_failed") from None
            finally:
                del name
        require(destination.is_file(), "screenshot_missing")

    def window_geometry(self):
        result = subprocess.run(
            ["xdotool", "getwindowfocus", "getwindowgeometry", "--shell"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
        )
        require(result.returncode == 0 and len(result.stdout) < 1024, "x11_geometry_query_failed")
        values = {}
        for line in result.stdout.decode("ascii").splitlines():
            if line.isdecimal():
                # Some xdotool versions print getwindowfocus's ID before chained output.
                continue
            key, value = line.split("=", 1)
            values[key] = int(value)
        require(
            all(key in values for key in ("X", "Y", "WIDTH", "HEIGHT")),
            "x11_geometry_fields_missing",
        )
        return values

    def close(self):
        if self.remote is not None:
            try:
                self.remote.Stop(timeout=3)
            except Exception:
                pass


WORKLOAD = """import pathlib, sys, time
directory = pathlib.Path(sys.argv[1])
def ready(name):
    (directory / (name + ".done")).touch()
def wait(name):
    deadline = time.monotonic() + 120
    while not (directory / (name + ".go")).exists():
        if time.monotonic() > deadline:
            raise SystemExit(1)
        time.sleep(.02)
for n in range(10200):
    print("a11y-%06d" % n)
    if n % 100 == 99:
        sys.stdout.flush()
        time.sleep(.02)
sys.stdout.flush()
ready("seed")
wait("prime")
prime_count = int((directory / "prime-count").read_text())
next_row = 10200
for n in range(next_row, next_row + prime_count):
    print("a11y-%06d" % n)
    if n % 100 == 99:
        sys.stdout.flush()
next_row += prime_count
sys.stdout.flush()
ready("prime")
wait("trim")
for n in range(next_row, next_row + 60):
    print("a11y-%06d" % n, flush=True)
    time.sleep(.08)
ready("trim")
wait("alternate")
print("\\033[?1049h\\033[2J\\033[H", end="", flush=True)
for n in range(100):
    print("alt-%06d" % n, flush=True)
    time.sleep(.01)
ready("alternate-seed")
wait("alternate-scroll")
for n in range(100, 160):
    print("alt-%06d" % n, flush=True)
    time.sleep(.08)
ready("alternate-scroll")
wait("finish")
print("\\033[?1049l", end="", flush=True)
"""


def validate_tree(probe, input_driver, output, fixture_speech, readiness):
    terminal = probe.terminal
    require(
        terminal is not None and probe.find_terminal() == terminal, "fixture_terminal_not_selected"
    )
    atspi = probe.atspi
    probe.wait(lambda: terminal.getState().contains(atspi.STATE_FOCUSED), "terminal_not_focused")
    state = terminal.getState()
    require(state.contains(atspi.STATE_FOCUSABLE), "terminal_not_focusable")
    require(state.contains(atspi.STATE_SHOWING), "terminal_not_showing")

    def published_text():
        terminal.clear_cache()
        return terminal.queryText()

    text = probe.wait(published_text, "terminal_text_interface_missing")
    probe.wait(lambda: text.characterCount > 0, "terminal_text_missing")
    probe.wait(lambda: text.getText(0, -1).strip(), "terminal_prompt_missing")

    def viewport_ready():
        initial_document = text.getText(0, -1)
        require(len(initial_document) <= 4096, "initial_document_not_bounded")
        initial_pane = terminal.queryComponent().getExtents(atspi.WINDOW_COORDS)
        row_positions = set()
        end_position_available = False
        for offset in range(len(initial_document) + 1):
            try:
                _, row_y, _, row_height = text.getCharacterExtents(offset, atspi.WINDOW_COORDS)
            except Exception:
                if offset == len(initial_document):
                    continue
                raise
            if (
                row_height > 0
                and initial_pane.y <= row_y
                and row_y < initial_pane.y + initial_pane.height
            ):
                row_positions.add(row_y)
                end_position_available |= offset == len(initial_document)
        # An empty final row has no scalar character; include it if the Text API
        # cannot return the document-end geometry. Physical y positions include wraps.
        empty_final_row = initial_document.endswith("\n") and not end_position_available
        probe.viewport_rows = len(row_positions) + int(empty_final_row)
        probe.viewport_geometry = {
            "physical_row_positions": len(row_positions),
            "document_line_breaks": initial_document.count("\n"),
            "pane_height": initial_pane.height,
            "document_end_geometry_available": end_position_available,
            "empty_final_row_supplemented": empty_final_row,
        }
        return 10 <= probe.viewport_rows <= 200

    probe.wait(viewport_ready, "initial_viewport_rows_invalid")
    probe.begin("shell")
    # Encode the marker so echoed input cannot satisfy the output assertion.
    encoded = "".join(f"\\{ord(char):03o}" for char in MARKER + "\n")
    input_driver.command("printf '" + encoded + "'", wait_for_echo=True)
    probe.wait(lambda: probe.contains(MARKER), "shell_fixture_output_missing")
    probe.wait(lambda: probe.insert_marker, "shell_text_event_missing")
    # Orca derives inserted output from the CURRENT caret line. Let it finish
    # genuine output presentation before selection changes that caret.
    line_proof = {}

    def current_lines():
        nonlocal line_proof
        observed, line_proof = fixture_lines(
            text, MARKER, probe.marker_event_offsets, atspi.TEXT_GRANULARITY_LINE
        )
        return observed

    try:
        document, start, end, fixture_caret = probe.wait(
            current_lines, "terminal_line_boundary_invalid"
        )
    except SmokeFailure:
        (output / "fixture-line-failure.json").write_text(json.dumps(line_proof, indent=2) + "\n")
        raise
    (output / "fixture-line-query-plan.json").write_text(
        json.dumps({key: value for key, value in line_proof.items() if key != "queries"}, indent=2)
        + "\n"
    )
    (output / "fixture-line-verification.json").write_text(json.dumps(line_proof, indent=2) + "\n")
    line_queries = line_proof["queries"]
    readiness.wait(ReadinessStep.FIXTURE_SPEECH, fixture_speech)
    shell = probe.end()
    coordinates = atspi.DESKTOP_COORDS if input_driver.backend == "x11" else atspi.WINDOW_COORDS
    pane = terminal.queryComponent().getExtents(coordinates)
    caret = text.caretOffset
    require(0 <= caret <= text.characterCount, "caret_offset_invalid")
    character_x, character_y, _, _ = text.getCharacterExtents(caret, coordinates)
    require(pane.width > 0 and pane.height > 0, "pane_bounds_empty")
    if input_driver.backend == "x11":
        native = input_driver.window_geometry()
        require(
            native["X"] <= pane.x
            and native["Y"] <= pane.y
            and pane.x + pane.width <= native["X"] + native["WIDTH"]
            and pane.y + pane.height <= native["Y"] + native["HEIGHT"],
            "pane_outside_native_window",
        )
    require(
        pane.x <= character_x <= pane.x + pane.width
        and pane.y <= character_y <= pane.y + pane.height,
        "caret_outside_pane",
    )
    probe.begin("selection")
    require(text.addSelection(start, end), "selection_action_rejected")
    probe.wait(
        lambda: text.getNSelections() == 1 and tuple(text.getSelection(0)) == (start, end),
        "selection_not_applied",
    )
    probe.settle()
    input_driver.screenshot(output / "selection.png")
    require(text.setSelection(0, start + 1, end), "selection_update_rejected")
    probe.wait(
        lambda: tuple(text.getSelection(0)) == (start + 1, end), "selection_update_not_applied"
    )
    require(text.removeSelection(0), "selection_clear_rejected")
    probe.wait(lambda: text.getNSelections() == 0, "selection_not_cleared")
    selection = probe.end()
    require(selection.get("selection_events", 0) > 0, "selection_events_missing")
    return {
        "focused": True,
        "focusable": True,
        "showing": True,
        "bounds_valid": True,
        "x11_native_bounds_verified": input_driver.backend == "x11",
        "selection_roundtrip": True,
        "actual_caret_endpoint_verified": True,
        "caret_at_document_end": caret == text.characterCount,
        "viewport_rows": probe.viewport_rows,
        "viewport_geometry": probe.viewport_geometry,
        "selection": selection,
        "shell": shell,
        "fixture_line_queries": line_queries,
        "marker_event_offsets": probe.marker_event_offsets,
        "fixture_speech_before_selection": True,
    }


def verify_fixture_document(probe, prefix, last, minimum_rows):
    document = probe.terminal.queryText().getText(0, -1)
    pattern = re.compile(re.escape(prefix) + r"-([0-9]{6})")
    rows = [line for line in document.splitlines() if line]
    matches = [pattern.fullmatch(line) for line in rows]
    require(rows and all(matches), "fixture_document_contains_unexpected_rows")
    numbers = [int(match[1]) for match in matches]
    require(len(numbers) >= minimum_rows, "fixture_document_retention_incomplete")
    require(numbers == list(range(numbers[0], last + 1)), "fixture_document_rows_not_contiguous")
    text = probe.terminal.queryText()
    caret = text.caretOffset
    require(caret == len(document), "fixture_caret_not_at_document_end")
    pane = probe.terminal.queryComponent().getExtents(probe.atspi.WINDOW_COORDS)
    caret_x, caret_y, _, _ = text.getCharacterExtents(caret, probe.atspi.WINDOW_COORDS)
    require(
        pane.x <= caret_x <= pane.x + pane.width and pane.y <= caret_y <= pane.y + pane.height,
        "fixture_caret_endpoint_outside_pane",
    )
    return {
        "actual_caret_endpoint_verified": True,
        "caret_at_document_end": True,
        "scalar_characters": len(document),
        "fixture_rows": len(numbers),
        "first_row": numbers[0],
        "last_row": numbers[-1],
        "contiguous": True,
        "required_minimum_rows": minimum_rows,
    }


def wait_fixture_document(probe, prefix, last, minimum_rows):
    def complete_document():
        try:
            return verify_fixture_document(probe, prefix, last, minimum_rows)
        except SmokeFailure:
            return None

    return probe.wait(complete_document, "fixture_document_convergence_timeout", timeout=60)


def measure_churn(probe, input_driver, output, measurements):
    workload = output / "workload.py"
    workload.write_text(WORKLOAD)
    input_driver.command(
        "/usr/bin/python3 " + shlex.quote(str(workload)) + " " + shlex.quote(str(output))
    )
    probe.wait(lambda: (output / "seed.done").exists(), "scrollback_seed_timeout", timeout=180)
    probe.wait(lambda: probe.contains("a11y-010199"), "scrollback_seed_not_published", timeout=60)
    # Ghostty retains history in pages; the configured limit is not an exact
    # per-row cutoff. Its native retention test guarantees at least half.
    minimum_history = SCROLLBACK_LIMIT_ROWS // 2
    seed_document = wait_fixture_document(probe, "a11y", 10199, minimum_history)
    unprimed_document = seed_document
    # Leave history one row below its configured limit, so the measured sixty
    # rows must prune a page rather than only append after an earlier page trim.
    prime_rows = max(
        0, SCROLLBACK_LIMIT_ROWS + probe.viewport_rows - 2 - seed_document["fixture_rows"]
    )
    require(prime_rows <= minimum_history + probe.viewport_rows, "history_priming_bound_exceeded")
    (output / "prime-count").write_text(str(prime_rows))
    (output / "prime.go").touch()
    probe.wait(lambda: (output / "prime.done").exists(), "history_priming_timeout")
    seed_document = wait_fixture_document(probe, "a11y", 10199 + prime_rows, minimum_history)
    seed_document["priming_rows"] = prime_rows
    seed_document["unprimed_document"] = unprimed_document
    seed_document["priming_row_bound"] = minimum_history + probe.viewport_rows
    trim_last = 10259 + prime_rows
    for phase, marker, done, go in (
        ("trim", f"a11y-{trim_last:06d}", "trim", "trim"),
        ("alternate", "alt-000159", "alternate-scroll", "alternate-scroll"),
    ):
        if phase == "alternate":
            (output / "alternate.go").touch()
            probe.wait(lambda: (output / "alternate-seed.done").exists(), "alternate_seed_timeout")
            probe.wait(lambda: probe.contains("alt-000099"), "alternate_seed_not_published")
            phase_seed_document = wait_fixture_document(probe, "alt", 99, 10)
        else:
            phase_seed_document = seed_document
        probe.begin(phase)
        (output / (go + ".go")).touch()
        probe.wait(lambda: (output / (done + ".done")).exists(), phase + "_workload_timeout")
        probe.wait(lambda: probe.contains(marker), phase + "_output_not_published")
        document = wait_fixture_document(
            probe,
            "a11y" if phase == "trim" else "alt",
            trim_last if phase == "trim" else 159,
            minimum_history if phase == "trim" else 10,
        )
        measurements[phase] = probe.end()
        measurements[phase]["document"] = document
        measurements[phase]["seed_document"] = phase_seed_document
        input_driver.screenshot(output / (phase + ".png"))
    (output / "finish.go").touch()
    # Screen switches replace the document intentionally, so exclude them from churn.
    for phase, values in measurements.items():
        values["text_notifications_present"] = (
            values.get("insert_events", 0) > 0 and values.get("delete_events", 0) > 0
        )
        size = max(values["document_characters_before"], values["document_characters_after"])
        values["near_document_delete"] = values.get("max_delete_characters", 0) >= size // 2
        values["near_document_insert"] = values.get("max_insert_characters", 0) >= size // 2
        # Each phase emits sixty fixed-width rows. Bound total work as well as
        # individual events so splitting a whole-document diff cannot hide churn.
        before, after = values["seed_document"], values["document"]
        inserted_rows = after["last_row"] - before["last_row"]
        removed_rows = after["first_row"] - before["first_row"]
        require(inserted_rows == 60 and removed_rows >= 0, "fixture_churn_topology_invalid")
        if phase == "trim":
            require(removed_rows > 0, "primary_history_was_not_trimmed")
        row_width = 12 if phase == "trim" else 11
        values["rows_inserted"] = inserted_rows
        values["rows_removed"] = removed_rows
        values["insert_scalar_limit"] = inserted_rows * row_width * 2
        values["delete_scalar_limit"] = max(removed_rows, 1) * row_width * 2
        values["excessive_scalar_text_churn"] = (
            values.get("insert_characters", 0) > values["insert_scalar_limit"]
            or values.get("delete_characters", 0) > values["delete_scalar_limit"]
        )
    return measurements


ORCA_BOOTSTRAP = """import importlib.util, json, pathlib, sys
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
sys.prefix = "/usr"
sys.path.insert(1, "/usr/lib/python3/dist-packages")
# Preserve the installed CLI import order to avoid partially initialized KeyBindings.
from orca import debug
from orca import debugging_tools_manager
registry_api = importlib.util.find_spec("orca.gsettings_registry") is not None
if registry_api:
    from orca import gsettings_registry
from orca import messages
if not registry_api:
    from orca import settings
from orca import script_manager
if registry_api:
    from orca import speech_manager
else:
    from orca import settings_manager
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk
display = Gdk.Display.get_default()
display_type = display.__gtype__.name if display is not None else ""
display_proof = {"initialized": display is not None,
                 "wayland_display": display_type == "GdkWaylandDisplay",
                 "x11_display": display_type == "GdkX11Display",
                 "gsettings_registry_api": registry_api}
display_proof["expected_backend"] = display_proof[sys.argv[3] + "_display"]
pathlib.Path(sys.argv[4]).write_text(json.dumps(display_proof, indent=2))
if not display_proof["expected_backend"]:
    raise RuntimeError("private_reader_display_backend_invalid")
debug.debugLevel = debug.LEVEL_ALL
# Line-buffer the private observation FIFO so idle key-release evidence is delivered.
debug.debugFile = open(sys.argv[2], "w", buffering=1)
if registry_api:
    registry = gsettings_registry.get_registry()
    registry.set_runtime_value("speech", "enable", True)
    registry.set_runtime_value("braille", "enabled", False)
else:
    manager = settings_manager.get_manager()
    manager.activate(sys.argv[1], {"enableSpeech": True, "enableBraille": False})
    sys.path.insert(0, manager.get_prefs_dir())
from orca import orca
raise SystemExit(orca.main(prefs_dir=sys.argv[1]) if registry_api else orca.main())
"""


def start_orca(processes, probe, output, startup, readiness):
    configuration = output / "speech-config"
    configuration.mkdir()
    logs = output / "speech-logs"
    logs.mkdir()
    socket = Path(os.environ["XDG_RUNTIME_DIR"]) / "speechd.sock"
    modules = configuration / "modules"
    modules.mkdir()
    (modules / "dummy.conf").write_text("")
    # Processes owns shutdown, including the idle gap between readiness and Orca.
    (configuration / "speechd.conf").write_text(
        'LogLevel 0\nTimeout 0\nCommunicationMethod "unix_socket"\n'
        f'SocketPath "{socket}"\nLogDir "{logs}"\n'
        'AudioOutputMethod "alsa"\nAudioALSADevice "null"\n'
        'AddModule "dummy" "sd_dummy" "dummy.conf"\n'
        'DefaultModule "dummy"\nDefaultVoiceType "MALE1"\nDisableAutoSpawn\n'
    )
    # sd_dummy uses aplay for its test sound. Both it and fallback modules get a null device.
    alsa = configuration / "alsa.conf"
    alsa.write_text("pcm.null { type null }\npcm.!default { type null }\n")
    os.environ["ALSA_CONFIG_PATH"] = str(alsa)
    processes.start(
        [
            "speech-dispatcher",
            "--run-single",
            "--config-dir",
            str(configuration),
            "--socket-path",
            str(socket),
            "--log-dir",
            str(logs),
            "--pid-file",
            str(socket.parent / "speechd.pid"),
        ]
    )
    readiness.wait(ReadinessStep.SPEECH_DISPATCHER, socket.exists)
    os.environ["SPEECHD_ADDRESS"] = "unix_socket:" + str(socket)
    import speechd

    speech_client = speechd.SSIPClient("spaceterm-accessibility-smoke")
    try:
        speech_client.set_output_module("dummy")
        speech_client.speak("SpaceTerm accessibility smoke")
    finally:
        speech_client.close()
    # Speech output goes to the private null audio device.
    run(["gsettings", "set", "org.gnome.desktop.interface", "toolkit-accessibility", "true"])
    stream = OrcaOutput(output, probe, MARKER)
    startup["stream"] = stream
    preferences = output / "orca-preferences"
    preferences.mkdir()
    # Use the installed Orca core main with its CLI debug/settings initialization.
    # Detect the settings API: older installations use settings_manager, while
    # newer ones use gsettings_registry and pass preferences to core main.
    # The launcher --replace/--setup kills every same-user Orca, and even ordinary
    # launcher startup globally pgrep-reads other instances.
    # Isolated Python imports installed modules; private preferences contain no
    # user customization. No Orca handlers, predicates or speech are replaced.
    bootstrap = output / "orca-bootstrap.py"
    bootstrap.write_text(ORCA_BOOTSTRAP)
    environment = dict(os.environ)
    environment.update(DISPLAY=f":{DISPLAY_NUMBER}", GDK_BACKEND=os.environ["XDG_SESSION_TYPE"])
    keymap_helper = os.environ["XDG_SESSION_TYPE"] == "wayland"
    proof = {
        "core_entry_point": "orca.orca.main",
        "global_launcher_bypassed": True,
        "private_x11_keymap_helper": keymap_helper,
    }
    startup["proof"] = proof
    if keymap_helper:
        # Installed Orca unconditionally invokes xkbcomp on DISPLAY, including
        # Wayland. This owned helper serves only that keymap; the GPUI process
        # retains no DISPLAY and GNOME remains native Wayland with --no-x11.
        require("DISPLAY" not in os.environ, "wayland_application_x11_environment_present")
        require(not Path(f"/tmp/.X{DISPLAY_NUMBER}-lock").exists(), "private_display_in_use")
        helper = processes.start(
            ["Xvfb", f":{DISPLAY_NUMBER}", "-screen", "0", "128x128x24", "-nolisten", "tcp"]
        )
        readiness.wait(
            ReadinessStep.ORCA_KEYMAP_DISPLAY,
            lambda: (
                subprocess.run(
                    ["xdpyinfo"],
                    env=environment,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=2,
                ).returncode
                == 0
            ),
        )
        require(helper.poll() is None, "orca_private_keymap_display_exited")
    display_proof_file = output / "orca-display-proof.json"
    child = processes.start(
        [
            "/usr/bin/python3",
            "-I",
            str(bootstrap),
            str(preferences),
            str(stream.fifo),
            os.environ["XDG_SESSION_TYPE"],
            str(display_proof_file),
        ],
        env=environment,
        stdout=stream.process_writer,
        stderr=stream.process_writer,
    )
    startup["child"] = child
    stream.close_process_writer()
    expected = {
        key: value
        for key, value in environment.items()
        if key
        in (
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "DCONF_PROFILE",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "GDK_BACKEND",
            "DBUS_SESSION_BUS_ADDRESS",
            "DBUS_SYSTEM_BUS_ADDRESS",
            "SPEECHD_ADDRESS",
        )
    }

    def private_reader_environment():
        require(child.poll() is None, "orca_process_exited")
        entries = Path(f"/proc/{child.pid}/environ").read_bytes().split(bytes([0]))
        actual = dict(entry.split(b"=", 1) for entry in entries if b"=" in entry)
        checks = {
            key: actual.get(key.encode()) == value.encode() for key, value in expected.items()
        }
        checks.update(
            legacy_device_override_absent=b"ATSPI_USE_LEGACY_DEVICE" not in actual,
            a11y_manager_override_absent=b"ATSPI_USE_A11Y_MANAGER_DEVICE" not in actual,
        )
        proof["environment"] = checks
        return checks if all(checks.values()) else None

    readiness.wait(ReadinessStep.ORCA_ENVIRONMENT, private_reader_environment)
    if keymap_helper:
        helper_entries = Path(f"/proc/{helper.pid}/environ").read_bytes().split(bytes([0]))
        helper_environment = dict(entry.split(b"=", 1) for entry in helper_entries if b"=" in entry)
        helper_checks = {
            key: helper_environment.get(key.encode()) == value.encode()
            for key, value in expected.items()
            if key not in ("DISPLAY", "WAYLAND_DISPLAY", "GDK_BACKEND")
        }
        helper_arguments = Path(f"/proc/{helper.pid}/cmdline").read_bytes().split(bytes([0]))
        proof["helper_ownership"] = {
            "environment": helper_checks,
            "reserved_display_argument": f":{DISPLAY_NUMBER}".encode() in helper_arguments,
            "registered_child": any(record["pid"] == helper.pid for record in processes.identities),
        }
        require(
            all(helper_checks.values())
            and all(
                proof["helper_ownership"][key]
                for key in ("reserved_display_argument", "registered_child")
            ),
            "orca_keymap_helper_not_private",
        )

    def reader_display():
        require(child.poll() is None, "orca_process_exited")
        if not display_proof_file.exists():
            return None
        return json.loads(display_proof_file.read_text())

    proof["gdk_display"] = readiness.wait(ReadinessStep.ORCA_DISPLAY, reader_display)
    require(proof["gdk_display"]["expected_backend"], "orca_display_backend_invalid")

    def reader_signal(predicate):
        require(child.poll() is None, "orca_process_exited")
        require(stream.speech_backend["not_available"] == 0, "orca_speech_backend_unavailable")
        return predicate()

    # The display proof precedes Orca initialization. Its line-buffered debug
    # record is emitted after listener and keyboard watcher initialization.
    readiness.wait(ReadinessStep.ORCA_EVENT_LOOP, lambda: reader_signal(lambda: stream.started))
    import dbus

    session = dbus.SessionBus()
    accessibility = dbus.Interface(
        session.get_object("org.a11y.Bus", "/org/a11y/bus"), "org.a11y.Bus"
    )
    accessibility_bus = dbus.bus.BusConnection(str(accessibility.GetAddress(timeout=2)))
    try:
        registry = dbus.Interface(
            accessibility_bus.get_object("org.a11y.atspi.Registry", "/org/a11y/atspi/registry"),
            "org.a11y.atspi.Registry",
        )
        manager = dbus.Interface(
            accessibility_bus.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus"),
            "org.freedesktop.DBus",
        )
        readiness.wait(
            ReadinessStep.ORCA_ATSPI_REGISTRATION,
            lambda: reader_signal(
                lambda: orca_registered(
                    registry.GetRegisteredEvents(timeout=2),
                    child.pid,
                    lambda name: int(manager.GetConnectionUnixProcessID(name, timeout=2)),
                )
            ),
        )
    finally:
        accessibility_bus.close()
    readiness.wait(
        ReadinessStep.ORCA_SPEECH_BACKEND,
        lambda: reader_signal(lambda: stream.speech_backend["factory_success"] > 0),
    )
    startup["complete"] = True
    return child, stream, proof


KEYBOARD_WORKLOAD = """import pathlib, sys, termios
directory = pathlib.Path(sys.argv[1])
original = termios.tcgetattr(sys.stdin)
echo = termios.tcgetattr(sys.stdin)
echo[3] |= termios.ECHO | termios.ICANON
try:
    termios.tcsetattr(sys.stdin, termios.TCSANOW, echo)
    print("\\r\\nKEYBOARD-READY", flush=True)
    (directory / "keyboard.ready").touch()
    sys.stdin.readline()
finally:
    termios.tcsetattr(sys.stdin, termios.TCSANOW, original)
(directory / "keyboard.done").touch()
"""


PERFORMANCE_WORKLOAD = """import pathlib, sys, time
directory, phase = pathlib.Path(sys.argv[1]), sys.argv[2]
(directory / (phase + ".ready")).touch()
deadline = time.monotonic() + 60
while not (directory / (phase + ".go")).exists():
    if time.monotonic() > deadline:
        raise SystemExit(1)
    time.sleep(.01)
for n in range(5000):
    print("perf-%s-%06d" % (phase, n))
    if n % 50 == 49:
        sys.stdout.flush()
        time.sleep(.005)
sys.stdout.flush()
(directory / (phase + ".done")).touch()
"""


def process_cpu_ms(pid):
    # Fields 14 and 15 include all SpaceTerm threads, excluding shell subprocesses.
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) * 1000 / os.sysconf("SC_CLK_TCK")


def reacquire_terminal(probe, application, expected_marker=None):
    old_terminal = probe.terminal

    def published_terminal():
        require(application.poll() is None, "application_process_exited")
        # Deactivation can retire D-Bus objects and activation can register new
        # identities. Rediscover the actual process Window and its descendant.
        desktop = probe.atspi.Registry.getDesktop(0)
        desktop.clear_cache()
        for child in desktop:
            child.clear_cache()
        node = probe.find_terminal()
        if node is None:
            return None
        stack = [node]
        visited = 0
        while stack and visited < 5000:
            node = stack.pop()
            visited += 1
            node.clear_cache()
            if node.getRole() != probe.atspi.ROLE_TERMINAL or node.name != "Terminal Pane":
                stack.extend(node)
                continue
            text = node.queryText()
            document = text.getText(0, -1)
            count = text.characterCount
            caret = text.caretOffset
            if not document or len(document) != count:
                return None
            if expected_marker is not None and expected_marker not in document:
                return None
            state = node.getState()
            if not all(
                state.contains(flag)
                for flag in (
                    probe.atspi.STATE_FOCUSED,
                    probe.atspi.STATE_FOCUSABLE,
                    probe.atspi.STATE_SHOWING,
                )
            ):
                return None
            pane = node.queryComponent().getExtents(probe.atspi.WINDOW_COORDS)
            if not (0 <= caret <= count and pane.width > 0 and pane.height > 0):
                return None
            caret_x, caret_y, _, _ = text.getCharacterExtents(caret, probe.atspi.WINDOW_COORDS)
            if not (
                pane.x <= caret_x <= pane.x + pane.width
                and pane.y <= caret_y <= pane.y + pane.height
            ):
                return None
            return node, {
                "process_owned_window_rediscovered": True,
                "terminal_below_window": True,
                "full_text_queried": True,
                "document_scalars": count,
                "expected_marker_queried": expected_marker is not None,
                "focused": True,
                "showing": True,
                "focusable": True,
                "actual_caret_endpoint_verified": True,
                "terminal_identity_changed": node != old_terminal,
            }
        return None

    terminal, evidence = probe.wait(published_terminal, "reactivated_terminal_missing", timeout=60)
    probe.terminal = terminal
    return evidence


def measure_performance(
    probe, input_driver, output, application, properties, frame_output, results
):
    import dbus

    workload = output / "performance-workload.py"
    workload.write_text(PERFORMANCE_WORKLOAD)

    def persist():
        (output / "performance-verification.json").write_text(json.dumps(results, indent=2) + "\n")

    for active in (True, False):
        phase = "active" if active else "inactive"
        properties.Set("org.a11y.Status", "IsEnabled", dbus.Boolean(active))
        probe.settle(1)
        require(
            bool(properties.Get("org.a11y.Status", "IsEnabled")) == active,
            "performance_requested_activation_not_current",
        )
        activation = reacquire_terminal(probe, application) if active else None
        input_driver.command(
            "/usr/bin/python3 "
            + shlex.quote(str(workload))
            + " "
            + shlex.quote(str(output))
            + " "
            + phase
        )
        probe.wait(lambda: (output / (phase + ".ready")).exists(), "performance_ready_timeout")
        cpu_before = process_cpu_ms(application.pid)
        started = time.monotonic()
        probe.phase = "performance_" + phase
        probe.counts.clear()
        frame_output.begin(phase)
        source_before = probe.source_counts.copy()
        (output / (phase + ".go")).touch()
        probe.wait(
            lambda: (output / (phase + ".done")).exists(), "performance_output_timeout", timeout=90
        )
        emitted_ms = round((time.monotonic() - started) * 1000)
        values = {
            "emission_ms": emitted_ms,
            "rows_emitted": 5000,
            "accessibility_requested": active,
            "requested_activation_readback_verified": True,
        }
        results[phase] = values
        if activation is not None:
            values["activation"] = activation
        persist()
        # The same post-emission window includes pending emulator/render work in both trials.
        probe.settle(2)
        values["app_cpu_ms"] = round(process_cpu_ms(application.pid) - cpu_before)
        values["sample_window_ms"] = round((time.monotonic() - started) * 1000)
        text_events = probe.counts["insert_events"] + probe.counts["delete_events"]
        values["text_events"] = text_events
        values["accessibility_updates_observed"] = text_events > 0
        owned_events = (
            probe.source_counts["process_owned_terminal_text_events"]
            - source_before["process_owned_terminal_text_events"]
        )
        all_terminal_events = (
            probe.source_counts["terminal_text_events"] - source_before["terminal_text_events"]
        )
        source_errors = (
            probe.source_counts["source_query_errors"] - source_before["source_query_errors"]
        )
        values["process_owned_terminal_text_events"] = owned_events
        values["all_terminal_text_events"] = all_terminal_events
        values["text_source_query_errors"] = source_errors
        values["activation_readback_at_capture_end"] = (
            bool(properties.Get("org.a11y.Status", "IsEnabled")) == active
        )
        values["frames"] = frame_output.end()
        persist()
        require(
            values["activation_readback_at_capture_end"],
            "performance_activation_changed_during_capture",
        )
        require(not source_errors, "performance_event_source_unknown")
        if active:
            # Coherent semantic publication can follow the fixed CPU/frame sample.
            # Keep that sample unchanged and verify queued events with full output.
            def published_output():
                return (
                    probe.contains("perf-active-004999")
                    and probe.counts["insert_events"] + probe.counts["delete_events"] > 0
                    and probe.source_counts["process_owned_terminal_text_events"]
                    > source_before["process_owned_terminal_text_events"]
                    and probe.source_counts["terminal_text_events"]
                    > source_before["terminal_text_events"]
                )

            publication = {}
            values["publication"] = publication
            try:
                probe.wait(published_output, "performance_active_convergence_timeout", timeout=60)
                values["accessible_output_convergence_ms"] = round(
                    (time.monotonic() - started) * 1000
                )
                values["publication_tree"] = reacquire_terminal(
                    probe, application, "perf-active-004999"
                )
            finally:
                publication.update(
                    observation_ms=round((time.monotonic() - started) * 1000),
                    text_events=probe.counts["insert_events"] + probe.counts["delete_events"],
                    process_owned_terminal_text_events=(
                        probe.source_counts["process_owned_terminal_text_events"]
                        - source_before["process_owned_terminal_text_events"]
                    ),
                    all_terminal_text_events=(
                        probe.source_counts["terminal_text_events"]
                        - source_before["terminal_text_events"]
                    ),
                    text_source_query_errors=(
                        probe.source_counts["source_query_errors"]
                        - source_before["source_query_errors"]
                    ),
                )
                persist()
            publication["activation_readback_at_completion"] = bool(
                properties.Get("org.a11y.Status", "IsEnabled")
            )
            persist()
            require(not publication["text_source_query_errors"], "performance_event_source_unknown")
            require(
                publication["activation_readback_at_completion"],
                "performance_activation_changed_before_publication",
            )
        else:
            require(
                text_events == 0 and owned_events == 0 and all_terminal_events == 0,
                "performance_accessibility_activation_mismatch",
            )
        probe.phase = None
        if not active:
            reactivation = time.monotonic()
            properties.Set("org.a11y.Status", "IsEnabled", dbus.Boolean(True))
            require(
                bool(properties.Get("org.a11y.Status", "IsEnabled")),
                "performance_reactivation_not_current",
            )
            values["reactivation"] = reacquire_terminal(probe, application, "perf-inactive-004999")
            values["reactivation_convergence_ms"] = round((time.monotonic() - reactivation) * 1000)
        persist()
    results["measures_gpu_frame_time"] = False
    results["inactive_completion_measured_at_shell"] = True
    persist()
    return results


def native_windows(backend):
    """Count only the application's native windows on this private display."""
    try:
        if backend == "x11":
            result = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--class", "spaceterm"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=3,
            )
            require(
                result.returncode in (0, 1) and len(result.stdout) < 65536,
                "native_window_count_failed",
            )
            count = sum(line.isdigit() for line in result.stdout.splitlines())
        else:
            import dbus

            bus = dbus.SessionBus()
            introspection = dbus.Interface(
                bus.get_object("org.gnome.Shell.Introspect", "/org/gnome/Shell/Introspect"),
                "org.gnome.Shell.Introspect",
            )
            windows = introspection.GetWindows(timeout=3)
            count = sum(
                any(
                    "spaceterm" in str(properties.get(key, "")).lower()
                    for key in ("app-id", "wm-class")
                )
                for properties in windows.values()
            )
        return {"query_available": True, "count": count}
    except Exception:
        # Some GNOME versions restrict their introspection API. The AT-SPI
        # Window gate remains mandatory even when this diagnostic is unavailable.
        return {"query_available": False}


def verify_private_settings_bus(bus, output):
    import dbus

    evidence = {"passed": False, "settings_writes_before_verification": False}
    expected = {
        key: str(output / directory)
        for key, directory in (
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        )
    }
    runtime = Path(os.environ["XDG_RUNTIME_DIR"]).resolve()
    expected.update(
        XDG_RUNTIME_DIR=str(runtime),
        DCONF_PROFILE=str(output / "dconf-profile"),
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=" + str(runtime / "system-private-unavailable.sock"),
    )

    def daemon_environment(pid):
        entries = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
        actual = dict(entry.split(b"=", 1) for entry in entries if b"=" in entry)
        return {key: actual.get(key.encode()) == value.encode() for key, value in expected.items()}

    try:
        require_private_runtime(runtime, output)
        require(
            all(os.environ.get(key) == value for key, value in expected.items()),
            "private_settings_environment_missing",
        )
        profile = Path(expected["DCONF_PROFILE"])
        require(
            profile.is_absolute() and profile.read_text() == "user-db:user\n",
            "private_dconf_profile_missing",
        )
        evidence["private_profile_verified"] = True
        manager = dbus.Interface(
            bus.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus"), "org.freedesktop.DBus"
        )
        bus_pid = int(manager.GetConnectionUnixProcessID("org.freedesktop.DBus", timeout=5))
        evidence["bus_pid"] = bus_pid
        evidence["bus_environment"] = daemon_environment(bus_pid)
        require(all(evidence["bus_environment"].values()), "session_bus_environment_not_private")
        evidence["exported_before_bus_verified"] = True
        manager.StartServiceByName("ca.desrt.dconf", dbus.UInt32(0), timeout=5)
        dconf_pid = int(manager.GetConnectionUnixProcessID("ca.desrt.dconf", timeout=5))
        evidence["dconf_pid"] = dconf_pid
        evidence["dconf_environment"] = daemon_environment(dconf_pid)
        require(all(evidence["dconf_environment"].values()), "dconf_environment_not_private")
        evidence["passed"] = True
        return evidence
    finally:
        (output / "isolation-verification.json").write_text(json.dumps(evidence, indent=2) + "\n")


def redirect_private_system_bus(bus, output):
    import dbus

    proof = {"passed": False, "activation_environment_updated": False}
    try:
        runtime = Path(os.environ["XDG_RUNTIME_DIR"])
        blocked = "unix:path=" + str(runtime / "system-private-unavailable.sock")
        require(
            os.environ.get("DBUS_SYSTEM_BUS_ADDRESS") == blocked,
            "system_bus_not_blocked_before_session",
        )
        address = os.environ["DBUS_SESSION_BUS_ADDRESS"]
        require(address.startswith("unix:"), "private_session_bus_address_invalid")
        manager = dbus.Interface(
            bus.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus"), "org.freedesktop.DBus"
        )
        # No child ever inherits the live system-bus default. Earlier bus/dconf
        # processes retain the blocked private socket; new activations and direct
        # children use this already-verified private session bus for SYSTEM too.
        os.environ["DBUS_SYSTEM_BUS_ADDRESS"] = address
        manager.UpdateActivationEnvironment(
            dbus.Dictionary({"DBUS_SYSTEM_BUS_ADDRESS": address}, signature="ss"), timeout=5
        )
        proof["activation_environment_updated"] = True
        # libdbus can cache initial bus addresses. Verify the real default
        # SYSTEM connector in a fresh child that inherits the redirected address
        # BEFORE importing dbus, instead of observing a stale blocked cache.
        observer = subprocess.run(
            [
                "/usr/bin/python3",
                "-I",
                "-c",
                "import dbus,json; b=dbus.SystemBus(private=True); "
                "m=dbus.Interface(b.get_object('org.freedesktop.DBus','/org/freedesktop/DBus'),"
                "'org.freedesktop.DBus'); "
                "print(json.dumps({'id':str(m.GetId(timeout=3)),"
                "'pid':int(m.GetConnectionUnixProcessID('org.freedesktop.DBus',timeout=3))})); b.close()",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
        )
        require(observer.returncode == 0, "private_system_bus_observation_failed")
        observed = json.loads(observer.stdout)
        proof["fresh_default_system_connector"] = True
        proof["system_and_session_bus_ids_equal"] = observed["id"] == manager.GetId(timeout=5)
        proof["system_and_session_bus_processes_equal"] = observed[
            "pid"
        ] == manager.GetConnectionUnixProcessID("org.freedesktop.DBus", timeout=5)
        proof["both_addresses_private_and_equal"] = os.environ["DBUS_SYSTEM_BUS_ADDRESS"] == address
        require(
            all(
                proof[key]
                for key in (
                    "system_and_session_bus_ids_equal",
                    "system_and_session_bus_processes_equal",
                    "both_addresses_private_and_equal",
                )
            ),
            "system_bus_not_redirected_private",
        )
        proof["passed"] = True
        return proof
    finally:
        (output / "system-bus-verification.json").write_text(json.dumps(proof, indent=2) + "\n")


def runtime_child_environment(child):
    require(child.poll() is None, "private_child_process_exited")
    entries = Path(f"/proc/{child.pid}/environ").read_bytes().split(bytes([0]))
    actual = dict(entry.split(b"=", 1) for entry in entries if b"=" in entry)
    keys = (
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "DCONF_PROFILE",
        "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_SYSTEM_BUS_ADDRESS",
    )
    checks = {key: actual.get(key.encode()) == os.environ[key].encode() for key in keys}
    return checks if all(checks.values()) else None


def isolation_only_session(args):
    import dbus

    report = {
        "backend": args.private_session,
        "passed": False,
        "scope": "private_bus_dconf_only",
        "settings_writes_performed": False,
    }
    try:
        bus = dbus.SessionBus()
        report["isolation"] = verify_private_settings_bus(bus, args.output_dir)
        report["system_bus"] = redirect_private_system_bus(bus, args.output_dir)
        report["passed"] = True
    except Exception as error:
        report["failure"] = str(error) if isinstance(error, SmokeFailure) else type(error).__name__
    finally:
        (args.output_dir / "measurement.json").write_text(json.dumps(report, indent=2) + "\n")
    return int(not report["passed"])


def private_session(args):
    import dbus
    import dbus.service

    if args.isolation_only:
        return isolation_only_session(args)

    output = args.output_dir
    processes = Processes(output / "owned-processes.json")
    input_driver = None
    orca_stream = None
    orca_process = None
    orca_startup = {}
    frame_output = None
    application = None
    report = {"backend": args.private_session, "passed": False, "scope": "full_accessibility_smoke"}

    def interrupted(_signal, _frame):
        if processes.spawning:
            processes.interrupted = True
            return
        raise SmokeFailure("session_interrupted")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        bus = dbus.SessionBus()
        report["isolation"] = verify_private_settings_bus(bus, output)
        report["system_bus"] = redirect_private_system_bus(bus, output)
        report["source_binary"] = json.loads((output.parent / "binary-proof.json").read_text())
        require(report["source_binary"]["passed"], "source_executable_identity_not_verified")
        processes.start(["/usr/libexec/at-spi-bus-launcher", "--launch-immediately"])
        probe = Probe()
        probe.wait(lambda: bus.name_has_owner("org.a11y.Bus"), "accessibility_bus_missing")
        properties = dbus.Interface(
            bus.get_object("org.a11y.Bus", "/org/a11y/bus"), "org.freedesktop.DBus.Properties"
        )
        properties.Set("org.a11y.Status", "IsEnabled", dbus.Boolean(True))
        # Keep GNOME's automatic reader autostart disabled on the verified bus.
        # Manual genuine Orca uses IsEnabled and does not require this setting.
        run(
            [
                "gsettings",
                "set",
                "org.gnome.desktop.a11y.applications",
                "screen-reader-enabled",
                "false",
            ]
        )
        automatic_reader = subprocess.run(
            ["gsettings", "get", "org.gnome.desktop.a11y.applications", "screen-reader-enabled"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
        )
        require(
            automatic_reader.returncode == 0 and automatic_reader.stdout.strip() == b"false",
            "automatic_reader_not_disabled",
        )
        report["automatic_screen_reader_disabled"] = True
        if args.private_session == "x11":
            require(not Path(f"/tmp/.X{DISPLAY_NUMBER}-lock").exists(), "private_display_in_use")
            display = processes.start(
                ["Xvfb", f":{DISPLAY_NUMBER}", "-screen", "0", "1920x1080x24", "-nolisten", "tcp"]
            )
            probe.wait(
                lambda: (
                    subprocess.run(
                        ["xdpyinfo"],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        timeout=2,
                    ).returncode
                    == 0
                ),
                "x11_display_timeout",
            )
        else:
            # Fresh preferences must not leave GNOME's welcome dialog over the fixture.
            run(
                [
                    "gsettings",
                    "set",
                    "org.gnome.shell",
                    "welcome-dialog-last-shown-version",
                    "'999'",
                ]
            )
            display = processes.start(
                [
                    "gnome-shell",
                    "--headless",
                    "--wayland",
                    "--no-x11",
                    "--virtual-monitor",
                    "1920x1080",
                    "--wayland-display",
                    WAYLAND_SOCKET,
                ]
            )
            probe.wait(
                lambda: (
                    (Path(os.environ["XDG_RUNTIME_DIR"]) / WAYLAND_SOCKET).exists()
                    and bus.name_has_owner("org.gnome.Mutter.RemoteDesktop")
                ),
                "wayland_display_timeout",
                timeout=60,
            )
        require(display.poll() is None, "display_process_exited")
        readiness = Readiness(probe.drain)
        report["readiness"] = {
            "overall_timeout_seconds": round(readiness.deadline - readiness.started),
            "observed_ms": readiness.observed,
        }
        report["display_environment"] = readiness.wait(
            ReadinessStep.DISPLAY_ENVIRONMENT, lambda: runtime_child_environment(display)
        )
        orca_process, orca_stream, report["orca_launch"] = start_orca(
            processes, probe, output, orca_startup, readiness
        )
        # Desktop and reader startup can update these properties themselves.
        properties.Set("org.a11y.Status", "IsEnabled", dbus.Boolean(True))
        frame_output = FrameOutput(output, probe)
        os.environ["ZED_MEASUREMENTS"] = "1"
        drivers = sorted(Path("/usr/share/vulkan/icd.d").glob("lvp_icd*.json"))
        require(drivers, "lavapipe_driver_missing")
        os.environ["VK_DRIVER_FILES"] = str(drivers[0])
        expected_backend = {"headless_override_absent": "ZED_HEADLESS" not in os.environ}
        if args.private_session == "wayland":
            expected_backend.update(
                DISPLAY_absent="DISPLAY" not in os.environ,
                WAYLAND_DISPLAY_reserved=os.environ.get("WAYLAND_DISPLAY") == WAYLAND_SOCKET,
            )
        else:
            expected_backend.update(
                DISPLAY_reserved=os.environ.get("DISPLAY") == f":{DISPLAY_NUMBER}",
                WAYLAND_DISPLAY_absent="WAYLAND_DISPLAY" not in os.environ,
            )
        require(all(expected_backend.values()), "application_backend_environment_invalid")
        require(os.environ.get("SPACETERM_DEVELOPMENT_FOREGROUND") == "1", "source_owner_missing")
        require(os.environ.get("SPACETERM_DEVELOPMENT_OWNER_SOCKET"), "source_owner_socket_missing")
        require(
            os.environ.get("SPACETERM_DEVELOPMENT_CLIENT")
            == f"a11y-{args.private_session}-{output.parent.name}".replace("_", "-"),
            "server_fixture_client_identity_missing",
        )
        report["server_backed"] = {"owned_foreground_server": True}
        application = processes.start(
            [str(args.binary), "--new-instance"], stderr=frame_output.writer
        )
        probe.application_pid = application.pid
        report["application_private_environment"] = readiness.wait(
            ReadinessStep.APPLICATION_ENVIRONMENT, lambda: runtime_child_environment(application)
        )
        app_entries = Path(f"/proc/{application.pid}/environ").read_bytes().split(bytes([0]))
        app_environment = dict(entry.split(b"=", 1) for entry in app_entries if b"=" in entry)
        actual_backend = {
            "headless_override_absent": b"ZED_HEADLESS" not in app_environment,
            "named_server_client": app_environment.get(b"SPACETERM_DEVELOPMENT_CLIENT")
            == os.environ["SPACETERM_DEVELOPMENT_CLIENT"].encode(),
        }
        if args.private_session == "wayland":
            actual_backend.update(
                DISPLAY_absent=b"DISPLAY" not in app_environment,
                WAYLAND_DISPLAY_reserved=app_environment.get(b"WAYLAND_DISPLAY")
                == WAYLAND_SOCKET.encode(),
            )
        else:
            actual_backend.update(
                DISPLAY_reserved=app_environment.get(b"DISPLAY") == f":{DISPLAY_NUMBER}".encode(),
                WAYLAND_DISPLAY_absent=b"WAYLAND_DISPLAY" not in app_environment,
            )
        require(all(actual_backend.values()), "application_actual_backend_environment_invalid")
        report["application_backend_environment"] = actual_backend
        launched = Path(f"/proc/{application.pid}/exe").stat()
        staged = args.binary.stat()
        report["pinned_executable_running"] = (launched.st_dev, launched.st_ino) == (
            staged.st_dev,
            staged.st_ino,
        )
        require(report["pinned_executable_running"], "application_executable_not_pinned")
        frame_output.close_writer()

        def application_window():
            require(application.poll() is None, "application_process_exited")
            return probe.find_window(application.pid)

        window = readiness.wait(ReadinessStep.APPLICATION_WINDOW, application_window, timeout=60)
        report["window_accessibility_published"] = True
        if args.private_session == "x11":
            run(["xdotool", "search", "--onlyvisible", "--class", "spaceterm", "windowfocus"])
        input_driver = Input(args.private_session, probe)
        probe.open_server_workspace(input_driver)
        readiness.wait(ReadinessStep.TERMINAL_OBJECT, probe.find_terminal, timeout=60)
        report["terminal_accessibility_published"] = True
        window = probe.terminal
        while window is not None and window.getRole() != probe.atspi.ROLE_FRAME:
            window = window.parent
        require(window is not None, "server_terminal_window_missing")
        report["server_backed"]["workspace_created_through_native_controls"] = True

        def orca_terminal_script():
            require(orca_process.poll() is None, "orca_process_exited")
            require(
                orca_stream.speech_backend["not_available"] == 0, "orca_speech_backend_unavailable"
            )
            return orca_stream.active_terminal_script

        def orca_fixture_speech():
            return (
                orca_terminal_script()
                and orca_stream.marker_spoken
                and orca_stream.speech_backend["factory_success"] > 0
            )

        def focused_terminal():
            require(application.poll() is None, "application_process_exited")
            probe.terminal.clear_cache()
            return probe.terminal.getState().contains(probe.atspi.STATE_FOCUSED)

        def focused_window():
            require(application.poll() is None, "application_process_exited")
            window.clear_cache()
            return window.getState().contains(probe.atspi.STATE_ACTIVE)

        readiness.wait(ReadinessStep.APPLICATION_WINDOW_FOCUS, focused_window)
        readiness.wait(ReadinessStep.TERMINAL_FOCUS, focused_terminal)
        readiness.wait(ReadinessStep.ORCA_TERMINAL_SCRIPT, orca_terminal_script)
        readiness.wait(
            ReadinessStep.ORCA_TERMINAL_FOCUS_PROCESSED,
            lambda: orca_terminal_script() and orca_stream.terminal_focus_processed,
        )
        report["tree"] = validate_tree(probe, input_driver, output, orca_fixture_speech, readiness)
        # A shell line editor can erase and redraw a character after its first publication.
        # Kernel canonical echo stays visible until the real key-release speech check finishes.
        keyboard = output / "keyboard.py"
        keyboard.write_text(KEYBOARD_WORKLOAD)
        input_driver.command(
            f"python3 {shlex.quote(str(keyboard))} {shlex.quote(str(output))}",
            wait_for_echo=True,
        )
        probe.wait(
            lambda: (output / "keyboard.ready").exists() and probe.contains("KEYBOARD-READY"),
            "keyboard_reader_not_ready",
        )
        # Native publication and Orca's keyboard pipeline run independently. Fence the earlier
        # command keys with a harmless modifier release processed by the real reader.
        before = orca_stream.keyboard_barriers
        input_driver.keyboard_barrier()
        probe.wait(
            lambda: orca_stream.keyboard_barriers > before and orca_stream.active_terminal_script,
            "orca_keyboard_pipeline_not_ready",
        )
        report["keyboard_pipeline_barrier"] = True
        probe.begin("keyboard_echo")
        typed_text = probe.terminal.queryText()
        typed_caret = typed_text.caretOffset
        typed_count = typed_text.characterCount
        input_driver.character(True)
        try:
            # Orca echoes Terminal keys after release or a matching text insertion.
            # Hold the genuine key until the paced AT-SPI insertion and caret
            # converge, so a 2ms synthetic key cannot outrun native publication.
            probe.wait(
                lambda: (
                    probe.counts["insert_events"] > 0
                    and probe.counts["caret_events"] > 0
                    and typed_text.characterCount == typed_count + 1
                    and typed_text.caretOffset == typed_caret + 1
                    and typed_text.getText(typed_caret, typed_caret + 1) == "q"
                ),
                "typed_character_not_published",
                timeout=5,
            )
        finally:
            input_driver.character(False)
        report["keyboard_release_after_atspi_publication"] = True

        def orca_character_speech():
            require(orca_process.poll() is None, "orca_process_exited")
            return (
                orca_stream.typed_character_spoken
                and orca_stream.terminal_keyboard_event
                and orca_stream.keyboard_counts["typed_key_pressed_events"] > 0
                and orca_stream.keyboard_counts["typed_key_released_events"] > 0
            )

        probe.wait(orca_character_speech, "orca_typed_character_speech_missing")
        report["keyboard_input"] = dict(probe.counts)
        input_driver.backspace()
        probe.wait(
            lambda: (
                typed_text.caretOffset == typed_caret
                and not typed_text.getText(
                    typed_caret, min(typed_caret + 1, typed_text.characterCount)
                ).strip()
            ),
            "keyboard_erase_not_published",
        )
        report["keyboard_echo"] = probe.end()
        input_driver.command("")
        probe.wait(lambda: (output / "keyboard.done").exists(), "keyboard_reader_not_finished")
        report["churn"] = {}
        measure_churn(probe, input_driver, output, report["churn"])
        require(orca_process.poll() is None, "orca_process_exited")
        require(
            all(phase["text_notifications_present"] for phase in report["churn"].values()),
            "churn_text_events_missing",
        )
        require(
            all(not phase["excessive_scalar_text_churn"] for phase in report["churn"].values()),
            "excessive_scalar_text_churn",
        )
        require(
            all(
                not phase["near_document_delete"] and not phase["near_document_insert"]
                for phase in report["churn"].values()
            ),
            "near_document_text_churn",
        )
        report["orca"] = orca_stream.summary()
        require(report["orca"]["speech_backend_initialized"], "orca_speech_backend_unavailable")
        require(
            not report["orca"]["process_output"].get("invalid_cache_add_signature", 0)
            and not report["orca"]["process_output"].get("invalid_cache_remove_signature", 0),
            "orca_invalid_cache_signal_signature",
        )
        processes.stop(orca_process)
        orca_stream.close()
        orca_stream = None
        primary_last = report["churn"]["trim"]["document"]["last_row"]
        probe.wait(
            lambda: probe.contains(f"a11y-{primary_last:06d}"), "primary_screen_restore_timeout"
        )
        report["performance"] = {}
        measure_performance(
            probe,
            input_driver,
            output,
            application,
            properties,
            frame_output,
            report["performance"],
        )
        require(application.poll() is None, "application_process_exited")
        frame_output.stream.drain()
        require(
            not frame_output.startup_errors
            and not frame_output.window_creation_errors
            and not frame_output.panic_lines,
            "application_health_errors",
        )
        report["passed"] = True
    except Exception as error:
        report["failure"] = str(error) if isinstance(error, SmokeFailure) else type(error).__name__
        if not isinstance(error, SmokeFailure):
            report["error_sites"] = [
                {"function": frame.name, "line": frame.lineno}
                for frame in traceback.extract_tb(error.__traceback__)[-4:]
            ]
        if input_driver is not None:
            try:
                input_driver.screenshot(output / "initial-failure.png")
                report["failure_screenshot_captured"] = True
            except Exception:
                report["failure_screenshot_captured"] = False
    finally:
        # Retain caller ownership even when a startup gate raises before return.
        # Cleanup then drains bounded diagnostics and closes both private FIFOs.
        if orca_startup and not orca_startup.get("complete"):
            orca_stream = orca_startup.get("stream")
            orca_process = orca_startup.get("child")
            report["orca_launch"] = orca_startup.get("proof", {})
        if "probe" in locals() and probe.terminal is not None:
            try:
                probe.terminal.clear_cache()
                interfaces = probe.terminal.get_interfaces()
                report["terminal_diagnostics"] = {
                    "children": probe.terminal.get_child_count(),
                    "child_roles": dict(Counter(child.getRoleName() for child in probe.terminal)),
                    "text_interface": "Text" in interfaces,
                    "component_interface": "Component" in interfaces,
                }
                if "Text" in interfaces:
                    report["terminal_diagnostics"].update(
                        scalar_characters=probe.terminal.queryText().characterCount,
                        fixture_output_queried=probe.contains(MARKER),
                        fixture_insert_event_observed=probe.insert_marker,
                    )
                if probe.phase is not None:
                    report["pending_phase"] = probe.phase
                    report["pending_events"] = dict(probe.counts)
                report["text_event_source_diagnostics"] = dict(probe.source_counts)
                if hasattr(probe, "viewport_geometry"):
                    report["initial_viewport_geometry"] = probe.viewport_geometry
            except Exception:
                report["terminal_diagnostics"] = {"query_available": False}
        if orca_process is not None:
            report["orca_process_running_before_cleanup"] = orca_process.poll() is None
            report["orca_process_exit_code_before_cleanup"] = orca_process.poll()
        if application is not None:
            report["application_running_at_cleanup"] = application.poll() is None
            report["native_windows"] = native_windows(args.private_session)
        cleanup = {"passed": True, "creation_attempted": False}
        report["server_workspace_cleanup"] = cleanup
        if "probe" in locals() and probe.server_workspace_before is not None:
            cleanup.update(passed=False, creation_attempted=True, name=probe.server_workspace_name)
            try:
                require(
                    application is not None and application.poll() is None,
                    "server_workspace_cleanup_application_unavailable",
                )
                probe.retire_server_workspace(input_driver)
                cleanup["passed"] = True
            except Exception as error:
                cleanup["failure"] = (
                    str(error) if isinstance(error, SmokeFailure) else type(error).__name__
                )
                report.update(passed=False)
                report.setdefault("failure", "server_workspace_cleanup_incomplete")
        if input_driver is not None:
            input_driver.close()
        try:
            # Keep the desktop's owner connection alive while retiring reader/display children.
            # Its exit lets the source supervisor finish this private-session process.
            processes.close(last=application)
        except SmokeFailure:
            report.update(passed=False, failure="direct_child_cleanup_incomplete")
        if orca_stream is not None:
            report["orca"] = orca_stream.summary()
            orca_stream.close()
        if frame_output is not None:
            frame_output.stream.drain()
            report["observed_draw_present_frames"] = frame_output.total_frames
            report["application_startup_errors"] = frame_output.startup_errors
            report["window_creation_errors"] = frame_output.window_creation_errors
            report["app_panic_lines"] = frame_output.panic_lines
            frame_output.close()
        if report.get("passed") and (
            not report.get("application_running_at_cleanup")
            or report.get("application_startup_errors")
            or report.get("window_creation_errors")
            or report.get("app_panic_lines")
        ):
            report.update(passed=False, failure="application_health_errors")
        (output / "measurement.json").write_text(json.dumps(report, indent=2) + "\n")
    return int(not report["passed"])


def prepare_binary(args, output):
    require(
        args.binary is None or args.expected_binary_sha256 is not None,
        "existing_binary_sha256_required",
    )
    if args.expected_binary_sha256 is not None:
        require(
            re.fullmatch(r"[0-9a-fA-F]{64}", args.expected_binary_sha256),
            "source_executable_sha256_invalid",
        )
    binary = args.binary
    require_prerequisites(args.backend)
    environment = source_environment({**os.environ, "CARGO_BUILD_JOBS": "2"})
    if binary is None:
        try:
            binary = build_executable("--locked", env=environment)
        except TaskError:
            raise SmokeFailure("source_build_failed") from None
    require(binary.is_file() and os.access(binary, os.X_OK), "source_executable_missing")
    destination = output / "source" / "bin" / "spaceterm"
    destination.parent.mkdir(parents=True)
    try:
        # A source build can contain gigabytes of debug symbols. The prefix only reads it.
        os.link(binary, destination)
    except OSError as error:
        if error.errno not in (errno.EXDEV, errno.EPERM, errno.EOPNOTSUPP):
            raise
        shutil.copy2(binary, destination)
    try:
        server = build_executable(
            "--package",
            "spaceterm-cli",
            "--no-default-features",
            "--locked",
            binary="spaceterm",
            env=environment,
        )
        # Reuse Development's resource layout, confined to this run's evidence directory.
        prefix = stage_prefix(destination, server)
    except TaskError:
        raise SmokeFailure("server_source_build_failed") from None
    destination.unlink()
    destination = prefix / "bin" / "spaceterm"
    with destination.open("rb") as executable:
        digest = hashlib.file_digest(executable, "sha256").hexdigest()
    expected = (
        args.expected_binary_sha256.lower() if args.expected_binary_sha256 is not None else None
    )
    proof = {
        "sha256": digest,
        "size_bytes": destination.stat().st_size,
        "expected_sha256": expected,
        "expected_hash_supplied": expected is not None,
        "existing_source_executable": args.binary is not None,
        "matched_expected_sha256": digest == expected if expected is not None else None,
        "passed": expected is None or digest == expected,
    }
    (output / "binary-proof.json").write_text(json.dumps(proof, indent=2) + "\n")
    require(proof["passed"], "source_executable_hash_mismatch")
    server = prefix / "bin" / "spaceterm-server"
    with server.open("rb") as executable:
        server_digest = hashlib.file_digest(executable, "sha256").hexdigest()
    (output / "server-binary-proof.json").write_text(
        json.dumps({"sha256": server_digest, "size_bytes": server.stat().st_size}) + "\n"
    )
    return destination


def supervised_session(args):
    require(
        args.binary is not None and args.server_binary is not None, "source_executables_missing"
    )
    # supervise removes mise's PYTHONPATH before launching its client. Bootstrap this
    # task explicitly so its library path never reaches the desktop or terminal programs.
    command = [
        "dbus-run-session",
        "--",
        "/usr/bin/python3",
        "-c",
        "import runpy, sys; sys.path.insert(0, sys.argv.pop(1)); sys.argv.pop(0); "
        "runpy.run_path(sys.argv[0], run_name='__main__')",
        str(TASK_LIBRARY),
        str(Path(__file__).resolve()),
        "--private-session",
        args.source_session,
        "--binary",
        str(args.binary),
        "--output-dir",
        str(args.output_dir),
    ]
    measurement = args.output_dir / "measurement.json"
    try:
        status = source_session(args.server_binary, command, os.environ)
    except TaskError:
        report = json.loads(measurement.read_text()) if measurement.exists() else {}
        report.update(passed=False, failure="source_session_failed")
        report["source_run_cleanup"] = {"passed": False}
        measurement.write_text(json.dumps(report, indent=2) + "\n")
        return 1
    # supervise returns only after its exact foreground Server completes cleanup.
    report = json.loads(measurement.read_text()) if measurement.exists() else {}
    report["source_run_cleanup"] = {"passed": True}
    if status:
        report.update(passed=False)
    measurement.write_text(json.dumps(report, indent=2) + "\n")
    return int(status != 0 or not report.get("passed"))


def main():
    args = parse_args()
    require(sys.platform == "linux", "linux_required")
    require(sys.version_info >= (3, 11), "system_python_3_11_required")
    require(1 <= args.session_timeout <= 1800, "session_timeout_out_of_range")
    if args.source_session or args.private_session:
        backend = args.source_session or args.private_session
        require(
            os.environ.get("SPACETERM_ACCESSIBILITY_SMOKE_SESSION") == backend,
            "private_session_wrapper_required",
        )
        require(args.output_dir is not None, "private_session_output_required")
        output = args.output_dir.resolve()
        require_private_runtime(Path(os.environ.get("XDG_RUNTIME_DIR", "/")), output)
        require(
            Path(os.environ.get("XDG_CONFIG_HOME", "/")).resolve().is_relative_to(output)
            and Path(os.environ.get("HOME", "/")).resolve().is_relative_to(output),
            "private_session_directories_required",
        )
        return supervised_session(args) if args.source_session else private_session(args)
    require(
        hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"), "pidfd_cleanup_required"
    )
    interruption = {"spawning": False, "received": False}

    def interrupted(_signal, _frame):
        # Defer during Popen until the finally block can retire the exact child.
        if interruption["received"]:
            return
        interruption["received"] = True
        if not interruption["spawning"]:
            raise SmokeFailure("session_interrupted")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    # Verify kernel support before starting any private bus/display/reader.
    try:
        capability_descriptor = os.pidfd_open(os.getpid())
    except OSError:
        raise SmokeFailure("pidfd_cleanup_required") from None
    os.close(capability_descriptor)
    output = (
        args.output_dir.resolve()
        if args.output_dir
        else Path(tempfile.mkdtemp(prefix="spaceterm-accessibility-smoke-"))
    )
    output.mkdir(parents=True, exist_ok=True)
    # A fresh directory keeps stale workload signals from satisfying a later run.
    run_output = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    binary = None if args.isolation_only else prepare_binary(args, run_output)
    server_binary = binary.parent / "spaceterm-server" if binary is not None else None
    backends = ("x11", "wayland") if args.backend == "both" else (args.backend,)
    status = 0
    for backend in backends:
        backend_output = run_output / backend
        backend_output.mkdir()
        runtime = create_private_runtime(backend_output)
        environment = dict(os.environ)
        for key in (
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "DBUS_SYSTEM_BUS_ADDRESS",
            "DBUS_STARTER_ADDRESS",
            "DBUS_STARTER_BUS_TYPE",
            "AT_SPI_BUS_ADDRESS",
            "XAUTHORITY",
            "SESSION_MANAGER",
            "ZDOTDIR",
            "BASH_ENV",
            "ENV",
            "ZED_HEADLESS",
            "PYTHONPATH",
            "PYTHONHOME",
            "ATSPI_USE_LEGACY_DEVICE",
            "ATSPI_USE_A11Y_MANAGER_DEVICE",
        ):
            environment.pop(key, None)
        # The private session imports only the task library.
        environment["PYTHONPATH"] = str(TASK_LIBRARY)
        for key in list(environment):
            if (
                key.startswith("SPACETERM_")
                or key.startswith("DCONF_")
                or key.startswith("SPEECHD_")
                or key
                in (
                    "PULSE_SERVER",
                    "PIPEWIRE_REMOTE",
                    "ALSA_CONFIG_PATH",
                    "ZED_MEASUREMENTS",
                    "VK_DRIVER_FILES",
                    "VK_ICD_FILENAMES",
                )
            ):
                environment.pop(key)
        for key, name in (
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ):
            directory = backend_output / name
            directory.mkdir(mode=0o700)
            environment[key] = str(directory)
        environment["ZDOTDIR"] = environment["HOME"]
        (Path(environment["HOME"]) / ".zshrc").write_text("")
        dconf_profile = backend_output / "dconf-profile"
        dconf_profile.write_text("user-db:user\n")
        null_audio = backend_output / "alsa-null.conf"
        null_audio.write_text("pcm.null { type null }\npcm.!default { type null }\n")
        # Block SYSTEM even before dbus-run-session knows its SESSION address.
        environment["DBUS_SYSTEM_BUS_ADDRESS"] = "unix:path=" + str(
            runtime / "system-private-unavailable.sock"
        )
        environment.update(
            XDG_RUNTIME_DIR=str(runtime),
            TMPDIR=str(runtime),
            XDG_SESSION_TYPE=backend,
            XDG_CURRENT_DESKTOP="GNOME",
            GSETTINGS_BACKEND="dconf",
            DCONF_PROFILE=str(dconf_profile),
            GDK_BACKEND=backend,
            NO_AT_BRIDGE="0",
            CARGO_BUILD_JOBS="2",
            SPACETERM_ACCESSIBILITY_SMOKE_SESSION=backend,
        )
        if not args.isolation_only:
            client_name = f"a11y-{backend}-{run_output.name}".replace("_", "-")
            environment["SPACETERM_DEVELOPMENT_CLIENT"] = client_name
        environment.update(
            ALSA_CONFIG_PATH=str(null_audio),
            PULSE_SERVER="unix:" + str(runtime / "private-null-pulse.sock"),
            PIPEWIRE_REMOTE="spaceterm-accessibility-null",
        )
        environment["DISPLAY" if backend == "x11" else "WAYLAND_DISPLAY"] = (
            f":{DISPLAY_NUMBER}" if backend == "x11" else WAYLAND_SOCKET
        )
        command = [
            "/usr/bin/python3",
            str(Path(__file__).resolve()),
            "--private-session" if args.isolation_only else "--source-session",
            backend,
            "--output-dir",
            str(backend_output),
        ]
        if binary is not None:
            command.extend(("--binary", str(binary)))
        if server_binary is not None:
            command.extend(("--server-binary", str(server_binary)))
        if args.isolation_only:
            command = ["dbus-run-session", "--", *command]
            command.append("--isolation-only")
        process = None
        timed_out = False
        try:
            interruption["spawning"] = True
            try:
                process = subprocess.Popen(
                    command,
                    cwd=ROOT,
                    env=environment,
                    start_new_session=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
            finally:
                interruption["spawning"] = False
            require(not interruption["received"], "session_interrupted")
            status |= process.wait(timeout=args.session_timeout)
        except subprocess.TimeoutExpired:
            status = 1
            timed_out = True
        finally:
            cleanup_failed = False
            try:
                if process is not None:
                    if args.isolation_only:
                        retire_direct_child(process)
                    else:
                        # This exact child owns the foreground Server's owner pipe.
                        # Let admitted Server cleanup finish before sweeping private daemons.
                        if process.poll() is None:
                            process.terminate()
                        process.wait()
            except (OSError, subprocess.TimeoutExpired):
                cleanup_failed = True
            try:
                cleanup_private_processes(runtime, Path(environment["HOME"]), backend_output)
                # Reap the exact direct wrapper if pidfd cleanup finished it.
                if process is not None:
                    process.wait(timeout=3)
            except Exception:
                cleanup_failed = True
            if cleanup_failed:
                status = 1
                measurement = backend_output / "measurement.json"
                report = (
                    json.loads(measurement.read_text())
                    if measurement.exists()
                    else {"backend": backend}
                )
                report.update(passed=False, failure="private_process_cleanup_incomplete")
                measurement.write_text(json.dumps(report, indent=2) + "\n")
            else:
                shutil.rmtree(runtime)
        if timed_out:
            measurement = backend_output / "measurement.json"
            report = (
                json.loads(measurement.read_text())
                if measurement.exists()
                else {"backend": backend}
            )
            report.update(passed=False, outer_session_timeout=True)
            measurement.write_text(json.dumps(report, indent=2) + "\n")
        measurement = backend_output / "measurement.json"
        report = json.loads(measurement.read_text()) if measurement.exists() else {}
        if not args.isolation_only and not report.get("source_run_cleanup", {}).get("passed"):
            report.update(passed=False)
            report.setdefault("failure", "source_run_cleanup_unconfirmed")
            measurement.write_text(json.dumps(report, indent=2) + "\n")
            status = 1
        if not args.isolation_only and not report.get("server_workspace_cleanup", {}).get("passed"):
            # A killed or timed-out wrapper cannot confirm cleanup through its retired GUI.
            report.update(passed=False)
            report.setdefault("failure", "server_workspace_cleanup_incomplete")
            report.setdefault(
                "server_workspace_cleanup",
                {
                    "passed": False,
                    "name": environment["SPACETERM_DEVELOPMENT_CLIENT"],
                    "failure": "server_workspace_cleanup_unconfirmed",
                },
            )
            measurement.write_text(json.dumps(report, indent=2) + "\n")
            status = 1
        if not report.get("passed"):
            status = 1
        outcome = "passed" if report.get("passed") else "failed"
        failure = (
            "session_timeout" if report.get("outer_session_timeout") else report.get("failure")
        )
        scope = " (private bus/dconf isolation only)" if args.isolation_only else ""
        print(f"{backend}: {outcome}{scope}" + (f" ({failure})" if failure else ""))
        if not report.get("passed"):
            # These observers retain classifications, counts and timings only.
            diagnostics = {
                key: report[key]
                for key in ("readiness", "orca", "server_workspace_cleanup", "source_run_cleanup")
                if key in report
            }
            print(f"{backend}: diagnostics " + json.dumps(diagnostics, sort_keys=True))
    return int(status != 0)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SmokeFailure as failure:
        print(f"accessibility smoke: {failure}", file=sys.stderr)
        raise SystemExit(1) from None
    except Exception as error:
        print(f"accessibility smoke: {type(error).__name__}", file=sys.stderr)
        raise SystemExit(1) from None
