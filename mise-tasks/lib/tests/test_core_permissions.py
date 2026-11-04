"""The owner-run permission check's pure parts. Nothing here runs ssh or osascript."""

import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.core_permissions import (
    APPLE_EVENT,
    apple_event_verdict,
    bundle_of,
    ssh_command,
    ssh_verdict,
)


class CorePermissionCheckTests(unittest.TestCase):
    def test_ssh_command_takes_only_a_name_or_address(self):
        self.assertEqual(
            ssh_command("penguin.local"),
            "/usr/bin/ssh -F /dev/null -o BatchMode=yes -o ConnectTimeout=3 penguin.local true",
        )
        for host in ("", "-oProxyCommand=x", "a b", "a;b", "$(id)", "user@host"):
            with self.subTest(host=host), self.assertRaises(TaskError):
                ssh_command(host)

    def test_ssh_passes_only_once_it_reached_the_host(self):
        self.assertTrue(ssh_verdict(0, ""))
        self.assertTrue(ssh_verdict(255, "sdk@penguin: Permission denied (publickey)."))
        self.assertTrue(ssh_verdict(255, "Host key verification failed."))
        for output in (
            "ssh: connect to host penguin port 22: No route to host",
            "ssh: connect to host penguin port 22: Operation timed out",
            "ssh: Could not resolve hostname penguin",
            "",
        ):
            with self.subTest(output=output):
                self.assertFalse(ssh_verdict(255, output))

    def test_apple_event_targets_another_application_and_needs_its_answer(self):
        self.assertIn('tell application "System Events"', APPLE_EVENT)
        self.assertTrue(apple_event_verdict(0, "loginwindow"))
        self.assertFalse(apple_event_verdict(0, ""))
        self.assertFalse(
            apple_event_verdict(1, "execution error: Not authorized to send Apple events (-1743)")
        )

    def test_bundle_is_the_app_that_holds_the_launchers_executable(self):
        with self.assertRaises(TaskError):
            bundle_of(Path("/usr/bin/true"))


if __name__ == "__main__":
    unittest.main()
