"""Exercise the no-display check against fake headless executables."""

import io
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks import cli_companion_checks as checks

# What the production CLI answers without native bindings or credentials.
ANSWERS = {
    "--help": "printf 'Usage: spaceterm-cli [--json] [--non-interactive] <command>\\n'",
    "--version": "printf 'spaceterm-cli 1.2.3\\n'",
    "--json": """printf '{"schema":1,"usage":"Usage: spaceterm-cli"}\\n'""",
    "--json --help": """printf '{"schema":1,"usage":"Usage: spaceterm-cli"}\\n'""",
    "--json --version": """printf '{"schema":1,"version":"spaceterm-cli 1.2.3"}\\n'""",
    "--json status": (
        """printf '{"result":{"authenticated":null,"authentication_failure":null,"""
        """"kind":"status","native":null,"native_failure":"native_unavailable","""
        """"negotiated_protocol_major":null},"schema":1}\\n'"""
    ),
    "--json invalid-command": """printf '{"error":"usage","schema":1}\n' >&2; exit 2""",
}
SESSION_VARIABLES = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "SSH_AUTH_SOCK",
)


class NoDisplayTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def cli(self, **overrides):
        """A fake spaceterm-cli that fails with 98 when it inherits a session variable."""
        answers = {**ANSWERS, **{key.replace("_", " "): value for key, value in overrides.items()}}
        inherited = "".join(f"${{{name}+x}}" for name in SESSION_VARIABLES)
        lines = [
            "#!/bin/sh",
            f'[ -n "{inherited}" ] && exit 98',
            "[ -t 0 ] && exit 97",
            'case "$*" in',
            *(f'  "{arguments}") {answer} ;;' for arguments, answer in answers.items()),
            "  *) exit 64 ;;",
            "esac",
        ]
        path = self.root / "spaceterm-cli"
        path.write_text("\n".join(lines) + "\n")
        path.chmod(0o755)
        return path

    def check(self, executable):
        with redirect_stdout(io.StringIO()) as output:
            count = checks.no_display(executable)
        return count, output.getvalue()

    def rejected(self, executable):
        with redirect_stdout(io.StringIO()), self.assertRaises(TaskError) as raised:
            checks.no_display(executable)
        message = str(raised.exception)
        self.assertNotIn(str(self.root), message)
        return message

    def test_normal_validation_never_submits_a_mutating_or_serve_command(self):
        for case in checks.CASES:
            self.assertNotIn("start", case.arguments)
            self.assertNotIn("serve", case.arguments)
            self.assertNotIn("end-all-sessions", case.arguments)

    def test_read_only_cli_passes_every_check(self):
        count, output = self.check(self.cli())
        self.assertEqual(count, len(checks.CASES))
        self.assertEqual(count, len(ANSWERS))
        self.assertIn("no-display --json invalid-command: exit 2\n", output)
        self.assertTrue(
            output.endswith(
                f"{count} no-display checks passed without a display, session bus, or desktop\n"
            )
        )

    def test_graphical_and_session_variables_never_reach_the_cli(self):
        # macOS sh sets TERM itself, so the environment assertion below covers TERM.
        session = {name: "inherited" for name in (*SESSION_VARIABLES, "TERM")}
        with patch.dict(os.environ, session):
            count, _ = self.check(self.cli())
        self.assertEqual(count, len(checks.CASES))
        environment = checks.no_display_environment(Path("/home"), Path("/tmp"))
        self.assertEqual(environment, {"PATH": "/usr/bin:/bin", "HOME": "/home", "TMPDIR": "/tmp"})

    def test_a_successful_unknown_command_is_rejected(self):
        message = self.rejected(
            self.cli(**{"--json_invalid-command": "printf 'started private-detail\\n'"})
        )
        self.assertIn("no-display --json invalid-command: exited 0, expected 2", message)
        self.assertNotIn("private-detail", message)

    def test_unexpected_results_are_rejected_without_their_content(self):
        for arguments, answer in (
            (
                "--json status",
                """printf '{"result":{"kind":"status","native_failure":null},"schema":1}\\n'""",
            ),
            (
                "--json invalid-command",
                """printf '{"error":"private-detail","schema":1}\\n' >&2; exit 2""",
            ),
            (
                "--json invalid-command",
                """printf '{"error":"usage","schema":2}\\n' >&2; exit 2""",
            ),
            (
                "--json invalid-command",
                """printf 'private-detail\\n'; printf '{"error":"usage","schema":1}\\n' >&2; exit 2""",
            ),
            ("--json invalid-command", "printf 'private-detail\\n' >&2; exit 2"),
            ("--version", "printf 'spaceterm-cli 1.2.3\\n'; printf 'private-detail\\n' >&2"),
        ):
            with self.subTest(arguments=arguments, answer=answer):
                message = self.rejected(self.cli(**{arguments.replace(" ", "_"): answer}))
                self.assertIn(f"no-display {arguments}: reported an unexpected result", message)
                self.assertNotIn("private-detail", message)

    def test_version_must_include_a_version_in_both_output_formats(self):
        for arguments, answer in (
            ("--version", "printf 'spaceterm-cli\\n'"),
            ("--json --version", """printf '{"schema":1,"version":"spaceterm-cli"}\\n'"""),
        ):
            with self.subTest(arguments=arguments):
                message = self.rejected(self.cli(**{arguments.replace(" ", "_"): answer}))
                self.assertIn(f"no-display {arguments}: reported an unexpected result", message)

    def test_state_written_by_the_cli_is_rejected(self):
        for answer in (
            'mkdir "$HOME/.local"',
            ': > "$TMPDIR/spaceterm.sock"',
            ": > listener",
        ):
            with self.subTest(answer=answer):
                message = self.rejected(
                    self.cli(
                        **{
                            "--json_invalid-command": f"{answer}; {ANSWERS['--json invalid-command']}"
                        }
                    )
                )
                self.assertIn("no-display --json invalid-command: wrote to its private", message)

    def test_background_processes_are_rejected_and_stopped(self):
        marker = self.root / "background.pid"
        answer = f"sleep 30 & echo $! > '{marker}'; {ANSWERS['--json invalid-command']}"
        message = self.rejected(self.cli(**{"--json_invalid-command": answer}))
        self.assertIn(
            "no-display --json invalid-command: left a process running after it exited", message
        )
        pid = int(marker.read_text())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.05)
        else:
            self.fail("the background process survived the check")

    def test_a_descendant_that_escapes_the_group_and_keeps_the_output_open_is_bounded(self):
        # The descendant starts its own session, so stopping the case's group cannot reach
        # it. It keeps the inherited stdout and stderr open after the CLI exits.
        marker = self.root / "escaped.pid"
        escape = (
            f'"{sys.executable}" -c '
            f'"import os, time; os.setsid(); open(\\"{marker}\\", \\"w\\").write(str(os.getpid())); '
            f'time.sleep(60)" &'
        )
        wait = f"while [ ! -s '{marker}' ]; do sleep 0.05; done"
        executable = self.cli(
            **{"--json_invalid-command": f"{escape} {wait}; {ANSWERS['--json invalid-command']}"}
        )

        def cleanup():
            # The check cannot stop a process outside the case's group, so the fixture does.
            if marker.exists() and marker.read_text():
                try:
                    os.kill(int(marker.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass

        self.addCleanup(cleanup)
        started = time.monotonic()
        with patch.object(checks, "CASE_TIMEOUT", 2):
            message = self.rejected(executable)
        self.assertLess(time.monotonic() - started, 30)
        self.assertIn(
            "no-display --json invalid-command: left a process running after it exited", message
        )
        self.assertTrue(marker.read_text())

    def test_runtime_and_output_are_bounded(self):
        def warmed(**overrides):
            # The first launch of a new executable can wait for the OS to assess it.
            executable = self.cli(**overrides)
            subprocess.run(
                [executable, "--help"], env={"PATH": checks.PATH}, capture_output=True, timeout=30
            )
            return executable

        slow_cli = warmed(**{"--json_status": "sleep 30"})
        with patch.object(checks, "CASE_TIMEOUT", 1):
            slow = self.rejected(slow_cli)
        flood_cli = warmed(**{"--json_status": "yes private-detail"})
        with patch.object(checks, "CASE_TIMEOUT", 1):
            flood = self.rejected(flood_cli)
        self.assertIn("no-display --json status: did not finish within 1 seconds", slow)
        self.assertIn("no-display --json status: exceeded its 64 KiB output bound", flood)
        self.assertNotIn("private-detail", flood)

    def test_only_the_checked_executable_runs(self):
        executable = self.cli()
        with patch.object(checks.subprocess, "Popen", wraps=subprocess.Popen) as popen:
            self.check(executable)
        self.assertEqual(popen.call_count, len(checks.CASES))
        for call in popen.call_args_list:
            self.assertEqual(call.args[0][0], str(executable))
            self.assertIs(call.kwargs["stdin"], subprocess.DEVNULL)

    def test_task_builds_the_development_cli_unless_an_executable_is_given(self):
        executable = self.cli()
        with (
            patch.object(checks, "build_executable", return_value=executable) as build,
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(checks.run(["no-display"]), 0)
            build.assert_called_once_with(
                "--locked",
                "-p",
                "spaceterm-client-native",
                "--features",
                "cli",
                binary="spaceterm-cli",
            )
            build.reset_mock()
            self.assertEqual(checks.run(["no-display", "--executable", str(executable)]), 0)
            build.assert_not_called()
        for arguments in (
            [],
            ["no-display", "--executable"],
            ["no-display", "private-detail"],
            ["no-display", "--executable", str(self.root / "private-detail")],
            ["no-display", "--executable", str(self.root)],
        ):
            with self.subTest(arguments=arguments), self.assertRaises(TaskError) as raised:
                checks.run(arguments)
            self.assertNotIn("private-detail", str(raised.exception))
            self.assertNotIn(str(self.root), str(raised.exception))
