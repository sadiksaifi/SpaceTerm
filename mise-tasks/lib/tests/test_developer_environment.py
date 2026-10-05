"""Exercise the environment a task hands to SpaceTerm Development."""

import os
import unittest

from spaceterm_tasks import LIBRARY, developer_environment


class DeveloperEnvironmentTests(unittest.TestCase):
    def test_task_library_is_removed_and_other_entries_are_kept(self):
        cases = {
            str(LIBRARY): None,
            os.pathsep.join(["/developer", str(LIBRARY), "/other"]): os.pathsep.join(
                ["/developer", "/other"]
            ),
            # The task library reached through another spelling is still the task library.
            str(LIBRARY / ".." / "lib"): None,
            "": None,
        }
        for pythonpath, expected in cases.items():
            with self.subTest(pythonpath=pythonpath):
                result = developer_environment({"PYTHONPATH": pythonpath, "TERM": "xterm"})
                self.assertEqual(result.get("PYTHONPATH"), expected)
                self.assertEqual(result["TERM"], "xterm")

    def test_environment_without_pythonpath_is_unchanged(self):
        environment = {"TERM": "xterm"}
        self.assertEqual(developer_environment(environment), environment)


if __name__ == "__main__":
    unittest.main()
