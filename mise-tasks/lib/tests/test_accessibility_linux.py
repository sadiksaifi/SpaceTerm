"""Verify bounded prerequisite failures and observable accessibility readiness."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import ROOT
from spaceterm_tasks.accessibility_linux import (
    OrcaOutput,
    Readiness,
    ReadinessStep,
    SmokeFailure,
    orca_registered,
    require_prerequisites,
)


class AccessibilityPrerequisiteTests(unittest.TestCase):
    def setUp(self):
        self.tools = patch("spaceterm_tasks.accessibility_linux.shutil.which", return_value="tool")
        self.which = self.tools.start()
        self.addCleanup(self.tools.stop)
        self.probe = patch(
            "spaceterm_tasks.accessibility_linux.subprocess.run",
            return_value=subprocess.CompletedProcess([], 0),
        )
        self.probe_command = self.probe.start()
        self.addCleanup(self.probe.stop)
        self.drivers = patch(
            "spaceterm_tasks.accessibility_linux.Path.glob", return_value=iter(["driver"])
        )
        self.drivers.start()
        self.addCleanup(self.drivers.stop)

    def test_missing_display_tool_fails_before_python_or_session_startup(self):
        self.which.side_effect = lambda tool: None if tool == "Xvfb" else "tool"
        with self.assertRaisesRegex(SmokeFailure, "^prerequisite_tool_missing:Xvfb$"):
            require_prerequisites("both")
        self.probe_command.assert_not_called()

    def test_x11_needs_no_gnome_shell_but_both_backends_require_it(self):
        self.which.side_effect = lambda tool: None if tool == "gnome-shell" else "tool"
        require_prerequisites("x11")
        with self.assertRaisesRegex(SmokeFailure, "^prerequisite_tool_missing:gnome-shell$"):
            require_prerequisites("both")

    def test_missing_system_bindings_or_schemas_has_a_bounded_reason(self):
        self.probe_command.return_value = subprocess.CompletedProcess([], 1)
        with self.assertRaisesRegex(
            SmokeFailure, "^prerequisite_system_python_bindings_or_schemas_missing$"
        ):
            require_prerequisites("both")

    def test_missing_software_vulkan_driver_has_a_bounded_reason(self):
        with patch("spaceterm_tasks.accessibility_linux.Path.glob", return_value=iter([])):
            with self.assertRaisesRegex(SmokeFailure, "^lavapipe_driver_missing$"):
                require_prerequisites("both")


class AccessibilityReadinessTests(unittest.TestCase):
    def setUp(self):
        self.now = 0.0
        clock = patch("spaceterm_tasks.accessibility_linux.time.monotonic", lambda: self.now)
        clock.start()
        self.addCleanup(clock.stop)
        sleeper = patch("spaceterm_tasks.accessibility_linux.time.sleep", self.advance)
        sleeper.start()
        self.addCleanup(sleeper.stop)
        self.drains = 0
        self.readiness = Readiness(self.drain, timeout=1)

    def advance(self, seconds):
        self.now += seconds

    def drain(self):
        self.drains += 1

    def test_waits_for_observation_instead_of_elapsed_startup_sleep(self):
        result = self.readiness.wait(
            ReadinessStep.ORCA_EVENT_LOOP, lambda: "ready" if self.drains >= 4 else None
        )
        self.assertEqual(result, "ready")
        self.assertEqual(self.drains, 4)
        self.assertLess(self.now, 1)
        self.assertEqual(self.readiness.observed, {"orca_event_loop": round(self.now * 1000)})

    def test_steps_share_an_overall_deadline(self):
        self.readiness.wait(ReadinessStep.ORCA_EVENT_LOOP, lambda: self.now >= 0.6)
        with self.assertRaisesRegex(SmokeFailure, "^readiness_timeout:terminal_focus$"):
            self.readiness.wait(ReadinessStep.TERMINAL_FOCUS, lambda: False)
        self.assertAlmostEqual(self.now, 1)
        self.assertNotIn("terminal_focus", self.readiness.observed)

    def test_transient_bus_error_does_not_replace_the_missing_step(self):
        def transient():
            raise OSError("raw native error with private contents")

        with self.assertRaisesRegex(SmokeFailure, "^readiness_timeout:orca_atspi_registration$"):
            self.readiness.wait(ReadinessStep.ORCA_ATSPI_REGISTRATION, transient)
        self.assertAlmostEqual(self.now, 1)

    def test_fatal_typed_failure_is_immediate(self):
        def exited():
            raise SmokeFailure("orca_process_exited")

        with self.assertRaisesRegex(SmokeFailure, "^orca_process_exited$"):
            self.readiness.wait(ReadinessStep.ORCA_EVENT_LOOP, exited)
        self.assertEqual(self.now, 0)

    def test_signal_delivered_after_deadline_cannot_satisfy_readiness(self):
        def delayed():
            self.advance(1.1)
            return True

        with self.assertRaisesRegex(SmokeFailure, "^readiness_timeout:fixture_speech$"):
            self.readiness.wait(ReadinessStep.FIXTURE_SPEECH, delayed)

    def test_each_step_keeps_its_shorter_timeout(self):
        with self.assertRaisesRegex(SmokeFailure, "^readiness_timeout:orca_speech_backend$"):
            self.readiness.wait(ReadinessStep.ORCA_SPEECH_BACKEND, lambda: False, timeout=0.2)
        self.assertAlmostEqual(self.now, 0.2)

    def test_only_orca_owned_listener_registrations_satisfy_readiness(self):
        events = [(":1.4", "Object:StateChanged:Focused"), (":1.4", "Object:TextChanged:Insert")]
        self.assertFalse(orca_registered(events, 40, lambda name: 41))
        self.assertTrue(orca_registered(events, 40, lambda name: 40))
        self.assertFalse(orca_registered(events[:1], 40, lambda name: 40))

    def test_focus_and_text_listeners_from_different_processes_do_not_combine(self):
        events = [(":1.4", "object:state-changed:focused"), (":1.5", "object:text-changed:insert")]
        self.assertFalse(orca_registered(events, 40, {":1.4": 40, ":1.5": 41}.__getitem__))

    def test_unrelated_bus_owner_is_never_queried(self):
        events = [(":1.5", "Window:Activate:")]

        def unexpected_query(name):
            self.fail("unrelated registration must not be queried")

        self.assertFalse(orca_registered(events, 40, unexpected_query))


class OrcaFocusReadinessTests(unittest.TestCase):
    TERMINAL = "SCRIPT MANAGER: Setting active script to [module=orca.scripts.terminal.script]"
    FINISHED = "^^^^^ FINISHED PRIORITY-0 OBJECT EVENT OBJECT:STATE-CHANGED:FOCUSED ^^^^^"

    def observed(self, records):
        with tempfile.TemporaryDirectory() as directory:
            probe = SimpleNamespace(phase=None, observers=[])
            output = OrcaOutput(Path(directory), probe, "spaceterm-a11y")
            descriptor = os.open(output.fifo, os.O_WRONLY)
            try:
                os.write(descriptor, ("\n".join(records) + "\n").encode())
                output.summary()
                return output.terminal_focus_processed
            finally:
                os.close(descriptor)
                output.close()

    def record(self, body):
        return "12:34:56.000000 - " + body

    def test_blank_terminal_becomes_ready_when_orca_finishes_processing_focus(self):
        self.assertTrue(
            self.observed(
                [
                    self.record(self.TERMINAL),
                    self.record("TOTAL PROCESSING TIME: 0.0010"),
                    self.FINISHED,
                ]
            )
        )

    def test_selection_and_initial_speech_do_not_prove_focus_processing_finished(self):
        self.assertFalse(
            self.observed(
                [
                    self.record(self.TERMINAL),
                    self.record("SPEECH OUTPUT: 'terminal'"),
                ]
            )
        )

    def test_focus_processed_before_terminal_selection_is_insufficient(self):
        self.assertFalse(self.observed([self.record(self.FINISHED), self.record(self.TERMINAL)]))

    def test_deactivation_invalidates_previous_focus_completion(self):
        self.assertFalse(
            self.observed(
                [
                    self.record(self.TERMINAL),
                    self.record(self.FINISHED),
                    self.record("SCRIPT MANAGER: Deactivating terminal"),
                    self.record(self.TERMINAL),
                ]
            )
        )

    def test_other_event_completion_is_insufficient(self):
        self.assertFalse(
            self.observed(
                [
                    self.record(self.TERMINAL),
                    self.record(self.FINISHED.replace("FOCUSED", "ACTIVE")),
                ]
            )
        )

    def test_quoted_completion_marker_is_insufficient(self):
        self.assertFalse(
            self.observed(
                [
                    self.record(self.TERMINAL),
                    self.record("SPEECH OUTPUT: '" + self.FINISHED + "'"),
                ]
            )
        )


class AccessibilityTerminalDiscoveryTests(unittest.TestCase):
    def setUp(self):
        specification = importlib.util.spec_from_file_location(
            "accessibility_smoke", ROOT / "mise-tasks/test/accessibility/linux.py"
        )
        assert specification is not None and specification.loader is not None
        smoke = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(smoke)
        self.smoke = smoke
        # Accessible objects are the external AT-SPI boundary. No native bus or
        # event loop is needed to verify process ownership of discovery.
        self.probe = smoke.Probe.__new__(smoke.Probe)
        self.probe.application_pid = 42
        self.probe.atspi = SimpleNamespace(ROLE_FRAME="frame", ROLE_TERMINAL="terminal")

    def desktop(self, applications):
        desktop = Accessible("desktop", children=applications)
        self.probe.atspi.Registry = SimpleNamespace(getDesktop=lambda index: desktop)

    def test_terminal_must_belong_to_the_launched_application_window(self):
        expected = Accessible("terminal", name="Terminal Pane")
        unrelated = Accessible("terminal", name="Terminal Pane")
        self.desktop(
            [
                Accessible(
                    "application", pid=42, children=[Accessible("frame", children=[expected])]
                ),
                Accessible(
                    "application", pid=43, children=[Accessible("frame", children=[unrelated])]
                ),
            ]
        )
        self.assertIs(self.probe.find_terminal(), expected)

    def test_foreign_terminal_cannot_satisfy_missing_application_readiness(self):
        self.desktop(
            [
                Accessible(
                    "application", pid=43, children=[Accessible("terminal", name="Terminal Pane")]
                )
            ]
        )
        self.assertIsNone(self.probe.find_terminal())

    def test_transient_empty_process_environment_waits_for_verified_private_values(self):
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
        environment = dict.fromkeys(keys, "private fixture value")
        published = b"\0".join(f"{key}={value}".encode() for key, value in environment.items())
        child = SimpleNamespace(pid=42, poll=lambda: None)
        with (
            patch.dict(os.environ, environment, clear=True),
            patch.object(Path, "read_bytes", side_effect=[b"", published]),
        ):
            readiness = Readiness(lambda: None)
            proof = readiness.wait(
                ReadinessStep.APPLICATION_ENVIRONMENT,
                lambda: self.smoke.runtime_child_environment(child),
            )
        self.assertEqual(proof, dict.fromkeys(keys, True))


class AccessibilityCommandInputTests(unittest.TestCase):
    def setUp(self):
        specification = importlib.util.spec_from_file_location(
            "accessibility_smoke", ROOT / "mise-tasks/test/accessibility/linux.py"
        )
        smoke = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(smoke)
        self.smoke = smoke

    def test_return_waits_for_command_echo_in_the_terminal_text(self):
        command = "printf '\\163'"
        text = SimpleNamespace(caretOffset=2, getText=lambda start, end: "$ "[start:end])
        terminal = SimpleNamespace(clear_cache=lambda: None, queryText=lambda: text)
        waited = False

        def wait(predicate, classification):
            nonlocal waited
            self.assertFalse(predicate())
            document = "$ " + command
            text.caretOffset = len(document)
            text.getText = lambda start, end: document[start:end]
            self.assertTrue(predicate())
            waited = True

        probe = SimpleNamespace(terminal=terminal, wait=wait)
        input_driver = self.smoke.Input("x11", probe)

        def send(arguments):
            if arguments[-1] == "Return":
                self.assertTrue(waited, "Return executed before the command echo was published")

        with patch.object(self.smoke, "run", side_effect=send):
            input_driver.command(command, wait_for_echo=True)

    def test_inactive_performance_input_does_not_require_accessibility_publication(self):
        input_driver = self.smoke.Input("x11", SimpleNamespace())
        with patch.object(self.smoke, "run") as send:
            input_driver.command("python3 workload.py")
        self.assertEqual(send.call_args.args[0][-1], "Return")


class Accessible(list):
    def __init__(self, role, name="", pid=None, children=()):
        super().__init__(children)
        self.role = role
        self.name = name
        self.pid = pid

    def getRole(self):
        return self.role

    def get_process_id(self):
        return self.pid


if __name__ == "__main__":
    unittest.main()
