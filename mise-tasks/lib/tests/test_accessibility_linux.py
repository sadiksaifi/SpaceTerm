"""Verify bounded prerequisite failures and observable accessibility readiness."""

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from spaceterm_tasks import ROOT
from spaceterm_tasks.accessibility_linux import (
    OrcaOutput,
    Readiness,
    ReadinessStep,
    SmokeFailure,
    orca_registered,
    require_account_server,
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

    def test_account_server_prerequisite_distinguishes_service_state_without_disclosing_output(
        self,
    ):
        for installed, running, stopped, failure in (
            (False, False, False, "account_server_not_installed"),
            (True, False, True, "account_server_stopped"),
            (True, False, False, "account_server_not_running"),
        ):
            with self.subTest(failure=failure):
                self.probe_command.return_value = subprocess.CompletedProcess(
                    [],
                    0,
                    stdout=(
                        "private service details\nInstalled: "
                        + str(installed).lower()
                        + "; enabled: true; running: "
                        + str(running).lower()
                        + "; stopped: "
                        + str(stopped).lower()
                        + ".\n"
                    ).encode(),
                )
                with self.assertRaisesRegex(SmokeFailure, "^" + failure + "$"):
                    require_account_server(Path("server"))
        self.probe_command.return_value = subprocess.CompletedProcess(
            [], 1, stdout=b"raw error with private values"
        )
        with self.assertRaisesRegex(SmokeFailure, "^account_server_service_unavailable$"):
            require_account_server(Path("server"))

    def test_running_service_requires_authenticated_account_connection(self):
        service = subprocess.CompletedProcess(
            [],
            0,
            stdout=b"Installed: true; enabled: true; running: true; stopped: false.\n",
        )
        self.probe_command.side_effect = [service, subprocess.CompletedProcess([], 1)]
        with self.assertRaisesRegex(SmokeFailure, "^account_server_connection_unavailable$"):
            require_account_server(Path("server"))
        self.probe_command.side_effect = [service, subprocess.CompletedProcess([], 0)]
        require_account_server(Path("server"))


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
        self.probe.server_workspace_named = False
        self.probe.server_workspace_name = "a11y-x11-run-owned"
        self.probe.atspi = SimpleNamespace(
            ROLE_FRAME="frame",
            ROLE_TERMINAL="terminal",
            ROLE_TREE_ITEM="tree item",
            STATE_SELECTED="selected",
        )

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
        self.probe.server_workspace_named = True
        self.probe.workspace_rows = lambda: [
            Accessible("tree item", name=self.probe.server_workspace_name)
        ]
        self.assertIs(self.probe.find_terminal(), expected)

    def test_retained_terminal_is_not_a_workload_target_before_fixture_creation(self):
        retained = Accessible("terminal", name="Terminal Pane")
        self.desktop(
            [Accessible("application", pid=42, children=[Accessible("frame", children=[retained])])]
        )
        self.assertIsNone(self.probe.find_terminal())

    def test_another_selected_workspace_cannot_supply_the_fixture_terminal(self):
        retained = Accessible("terminal", name="Terminal Pane")
        self.desktop(
            [Accessible("application", pid=42, children=[Accessible("frame", children=[retained])])]
        )
        self.probe.server_workspace_named = True
        self.probe.workspace_rows = lambda: [Accessible("tree item", name="Retained work")]
        self.assertIsNone(self.probe.find_terminal())

    def test_terminal_validation_refuses_retained_work_before_reading_or_typing(self):
        retained = Accessible("terminal", name="Terminal Pane")
        retained.queryText = Mock()
        self.probe.terminal = retained
        driver = Mock()
        with self.assertRaisesRegex(SmokeFailure, "^fixture_terminal_not_selected$"):
            self.smoke.validate_tree(self.probe, driver, None, None, None)
        retained.queryText.assert_not_called()
        driver.command.assert_not_called()

    def test_missing_control_failure_identifies_the_known_operation(self):
        self.desktop([Accessible("application", pid=42)])
        self.probe.wait = lambda predicate, classification: (
            predicate() or self.smoke.require(False, classification)
        )
        with self.assertRaisesRegex(
            SmokeFailure, "^server_workspace_control_missing:rename_workspace$"
        ):
            self.probe.activate_control("Rename Workspace")

    def test_fixture_is_named_at_creation_without_targeting_replaced_retained_proxies(self):
        retained = Accessible("terminal", name="Terminal Pane")
        retained.queryText = Mock()
        existing = Accessible("tree item", name="Retained work", states=())
        replacement = Accessible("tree item", name="Retained work", states=())
        owned = Accessible("tree item", name=self.probe.server_workspace_name)
        terminal = Accessible("terminal", name="Terminal Pane")
        start = Accessible("button", name="Start New Shell")
        tree = Accessible("tree", name="Workspaces", children=[existing])
        frame = Accessible("frame", children=[tree, retained])
        self.desktop([Accessible("application", pid=42, children=[frame])])
        self.probe.server_workspace_before = None
        self.probe.server_workspace_row = None
        self.probe.terminal = None
        self.probe.atspi.STATE_FOCUSED = "focused"
        self.probe.wait = lambda predicate, classification: (
            predicate() or self.smoke.require(False, classification)
        )
        switch = Accessible("button", name="Switch Workspace")
        local = Accessible("menu item", name="Local Workspace")
        field = Accessible("entry", name="Workspace name")
        query = ""
        field.queryText.side_effect = lambda: SimpleNamespace(getText=lambda start, end: query)
        frame.append(switch)
        switch.action = lambda: frame.extend([local, field])

        def create():
            self.assertEqual(query, owned.name, "the name must precede native creation")
            frame.remove(local)
            frame.remove(field)
            frame.remove(retained)
            tree[:] = [replacement, owned]
            frame.append(start)

        local.action = create

        def start_fixture():
            self.assertTrue(self.probe.server_workspace_named)
            frame.remove(start)
            frame.append(terminal)

        start.action = start_fixture

        def name_fixture(name, field):
            nonlocal query
            self.assertIsNone(self.probe.find_terminal(), "unnamed Workspace is not a target")
            query = name

        driver = SimpleNamespace(workspace_query=name_fixture)
        with patch.dict(
            os.environ, {"SPACETERM_DEVELOPMENT_CLIENT": self.probe.server_workspace_name}
        ):
            self.probe.open_server_workspace(driver)
        self.assertIs(self.probe.terminal, terminal)
        self.assertIs(self.probe.server_workspace_row, owned)
        self.assertEqual(replacement.name, "Retained work")
        retained.queryText.assert_not_called()

    def test_replaced_retained_proxy_is_not_the_created_workspace(self):
        self.probe.wait = lambda predicate, classification: (
            predicate() or self.smoke.require(False, classification)
        )
        retained = Accessible("tree item", name="Retained work")
        replacement = Accessible("tree item", name="Retained work")
        owned = Accessible("tree item", name=self.probe.server_workspace_name, states=())
        tree = Accessible("tree", name="Workspaces", children=[replacement, owned])
        self.desktop(
            [Accessible("application", pid=42, children=[Accessible("frame", children=[tree])])]
        )
        self.probe.server_workspace_before = [retained]
        self.assertIsNone(self.probe.created_workspace_row())
        replacement.states = ()
        owned.states = ("selected",)
        self.assertIs(self.probe.created_workspace_row(), owned)

    def test_cleanup_refuses_a_retained_proxy_when_the_fixture_name_is_missing(self):
        retained = Accessible("tree item", name="Retained work")
        tree = Accessible("tree", name="Workspaces", children=[retained])
        self.desktop(
            [Accessible("application", pid=42, children=[Accessible("frame", children=[tree])])]
        )
        self.probe.server_workspace_before = []
        self.probe.server_workspace_row = retained
        self.probe.wait = lambda predicate, classification: (
            predicate() or self.smoke.require(False, classification)
        )
        driver = Mock()
        with self.assertRaisesRegex(SmokeFailure, "^server_workspace_cleanup_identity_missing$"):
            self.probe.retire_server_workspace(driver)
        driver.context_menu.assert_not_called()
        self.assertEqual(list(tree), [retained])

    def test_foreign_terminal_cannot_satisfy_missing_application_readiness(self):
        self.desktop(
            [
                Accessible(
                    "application", pid=43, children=[Accessible("terminal", name="Terminal Pane")]
                )
            ]
        )
        self.assertIsNone(self.probe.find_terminal())

    def test_cleanup_reacquires_only_the_owned_workspace_after_native_objects_change(self):
        owned_name = "a11y-x11-run-owned"
        old_owned = Accessible("tree item", name=owned_name)
        other = Accessible("tree item", name="Server")
        survivor = Accessible("tree item", name="Server")
        owned = Accessible("tree item", name=owned_name)
        tree = Accessible("tree", name="Workspaces", children=[other, owned, survivor])
        frame = Accessible("frame", children=[tree])
        self.desktop([Accessible("application", pid=42, children=[frame])])
        self.probe.atspi.ROLE_TREE_ITEM = "tree item"
        self.probe.atspi.STATE_SELECTED = "selected"
        self.probe.atspi.STATE_FOCUSED = "focused"
        self.probe.server_workspace_before = [other, survivor]
        self.probe.server_workspace_row = old_owned
        self.probe.server_workspace_name = owned_name
        self.probe.server_workspace_named = True

        def wait(predicate, classification):
            value = predicate()
            self.smoke.require(value, classification)
            return value

        self.probe.wait = wait
        pressed = []
        owned.action = lambda: pressed.append("owned row")
        confirmation = Accessible("button", name="Kill Workspace")
        dialog = Accessible(
            "dialog", name=f"Kill Workspace \u201c{owned_name}\u201d?", children=[confirmation]
        )
        menu_item = Accessible("menu item", name="Kill Workspace")

        def show_confirmation():
            frame.remove(menu_item)
            frame.append(dialog)

        def retire():
            tree.remove(owned)
            frame.remove(dialog)
            pressed.append("confirmed")

        menu_item.action = show_confirmation
        confirmation.action = retire
        driver = SimpleNamespace(escape=lambda: None, context_menu=lambda: frame.append(menu_item))
        self.probe.retire_server_workspace(driver)
        self.assertEqual(pressed, ["confirmed"])
        self.assertEqual(list(tree), [other, survivor])

    def test_cleanup_refuses_ambiguous_owned_identity_without_deleting_a_workspace(self):
        rows = [Accessible("tree item", name="a11y-owned") for _ in range(2)]
        tree = Accessible("tree", name="Workspaces", children=rows)
        self.desktop(
            [Accessible("application", pid=42, children=[Accessible("frame", children=[tree])])]
        )
        self.probe.atspi.ROLE_TREE_ITEM = "tree item"
        self.probe.server_workspace_before = []
        self.probe.server_workspace_row = None
        self.probe.server_workspace_name = "a11y-owned"
        self.probe.wait = lambda predicate, classification: (
            predicate() or self.smoke.require(False, classification)
        )
        with self.assertRaisesRegex(SmokeFailure, "server_workspace_cleanup_ambiguous"):
            self.probe.retire_server_workspace(SimpleNamespace(escape=lambda: None))
        self.assertEqual(len(tree), 2)

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
        assert specification is not None and specification.loader is not None
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

        probe = SimpleNamespace(terminal=terminal, wait=wait, server_workspace_named=True)
        input_driver = self.smoke.Input("x11", probe)

        def send(arguments):
            if arguments[-1] == "Return":
                self.assertTrue(waited, "Return executed before the command echo was published")

        with patch.object(self.smoke, "run", side_effect=send):
            input_driver.command(command, wait_for_echo=True)

    def test_inactive_performance_input_does_not_require_accessibility_publication(self):
        input_driver = self.smoke.Input(
            "x11", SimpleNamespace(terminal=object(), server_workspace_named=True)
        )
        with patch.object(self.smoke, "run") as send:
            input_driver.command("python3 workload.py")
        self.assertEqual(send.call_args.args[0][-1], "Return")

    def test_workload_input_is_refused_before_fixture_ownership_without_sending_keys(self):
        input_driver = self.smoke.Input(
            "x11", SimpleNamespace(terminal=None, server_workspace_named=False)
        )
        with patch.object(self.smoke, "run") as send:
            for action in (
                lambda: input_driver.command("python3 workload.py"),
                lambda: input_driver.character(True),
                input_driver.backspace,
                input_driver.keyboard_barrier,
            ):
                with self.assertRaisesRegex(SmokeFailure, "^fixture_terminal_not_selected$"):
                    action()
            send.assert_not_called()


class AccessibilityApplicationIsolationTests(unittest.TestCase):
    def run_fixture(self, *, cleanup=True, isolation_only=False):
        specification = importlib.util.spec_from_file_location(
            "accessibility_smoke", ROOT / "mise-tasks/test/accessibility/linux.py"
        )
        assert specification is not None and specification.loader is not None
        smoke = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(smoke)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "prefix/bin/spaceterm"
            binary.parent.mkdir(parents=True)
            binary.touch()
            server = root / "source-server"
            server.write_bytes(b"fixture server executable")
            sessions = []
            reports = []

            def session(command, **options):
                sessions.append((command, options["env"]))
                output = Path(command[command.index("--output-dir") + 1])
                report = {"passed": True}
                if cleanup:
                    report["server_workspace_cleanup"] = {"passed": True}
                measurement = output / "measurement.json"
                measurement.write_text(json.dumps(report))
                reports.append(measurement)
                return SimpleNamespace(wait=lambda timeout: 0)

            def runtime(output):
                path = output / "runtime"
                path.mkdir()
                return path

            args = ["accessibility", "--output-dir", str(root / "output")]
            if isolation_only:
                args.append("--isolation-only")
            close = os.close
            with (
                patch.dict(os.environ, {"SPACETERM_DEVELOPMENT_CLIENT": "other-client"}),
                patch.object(smoke.sys, "argv", args),
                patch.object(smoke.sys, "platform", "linux"),
                patch.object(smoke.os, "pidfd_open", return_value=98765, create=True),
                patch.object(
                    smoke.os, "close", side_effect=lambda fd: None if fd == 98765 else close(fd)
                ),
                patch.object(smoke.signal, "pidfd_send_signal", create=True),
                patch.object(smoke.signal, "signal"),
                patch.object(smoke, "prepare_binary", return_value=binary),
                patch.object(smoke, "build_executable", return_value=server) as build,
                patch.object(smoke, "create_private_runtime", side_effect=runtime),
                patch.object(smoke.subprocess, "Popen", side_effect=session),
                patch.object(smoke, "retire_direct_child"),
                patch.object(smoke, "cleanup_private_processes"),
                patch("builtins.print"),
            ):
                status = smoke.main()
                self.assertEqual(os.environ["SPACETERM_DEVELOPMENT_CLIENT"], "other-client")
            return (
                status,
                sessions,
                build.call_count,
                [json.loads(path.read_text()) for path in reports],
            )

    def test_default_application_runs_always_stage_server_preflight_and_fresh_clients(self):
        status, sessions, builds, _ = self.run_fixture()
        self.assertEqual(status, 0)
        self.assertEqual(builds, 1)
        self.assertEqual(len(sessions), 2)
        clients = []
        for command, environment in sessions:
            self.assertIn("--server-binary", command)
            self.assertNotIn("--server-backed", command)
            clients.append(environment["SPACETERM_DEVELOPMENT_CLIENT"])
        self.assertNotEqual(*clients)
        self.assertTrue(all(client.startswith("a11y-") for client in clients))

    def test_default_application_run_cannot_pass_without_fixture_cleanup_evidence(self):
        status, _, _, reports = self.run_fixture(cleanup=False)
        self.assertEqual(status, 1)
        for report in reports:
            self.assertFalse(report["passed"])
            self.assertEqual(report["failure"], "server_workspace_cleanup_incomplete")

    def test_bus_only_isolation_run_never_builds_or_preflights_an_application_server(self):
        status, sessions, builds, _ = self.run_fixture(cleanup=False, isolation_only=True)
        self.assertEqual(status, 0)
        self.assertEqual(builds, 0)
        for command, _ in sessions:
            self.assertIn("--isolation-only", command)
            self.assertNotIn("--server-binary", command)
            self.assertNotIn("--binary", command)


class Accessible(list):
    def __init__(self, role, name="", pid=None, children=(), states=("selected", "focused")):
        super().__init__(children)
        self.role = role
        self.name = name
        self.pid = pid
        self.states = states
        self.queryText = Mock(side_effect=AssertionError("unexpected terminal read"))
        self.action = lambda: None

    def __bool__(self):
        return True

    def __eq__(self, other):
        return self is other

    def __ne__(self, other):
        return self is not other

    def clear_cache(self):
        pass

    def getState(self):
        return SimpleNamespace(contains=lambda flag: flag in self.states)

    def queryComponent(self):
        return SimpleNamespace(grabFocus=lambda: True)

    def queryAction(self):
        def activate(index):
            self.action()
            return True

        return SimpleNamespace(nActions=1, getName=lambda index: "click", doAction=activate)

    def getRole(self):
        return self.role

    def get_process_id(self):
        return self.pid


if __name__ == "__main__":
    unittest.main()
