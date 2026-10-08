"""Verify that missing native harness prerequisites produce bounded failures."""

import subprocess
import unittest
from unittest.mock import patch

from spaceterm_tasks.accessibility_linux import SmokeFailure, require_prerequisites


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


if __name__ == "__main__":
    unittest.main()
