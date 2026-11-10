"""A release desktop that packaging compiles embeds its release notes.

A desktop compiled before packaging already embeds them: its build refuses a release without
notes (tests/build_release_notes.rs), so packaging it needs no notes file.
"""

import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.packaging import release_notes_file


class ReleaseNotesFileTests(unittest.TestCase):
    def test_a_release_names_its_notes_file(self):
        with tempfile.TemporaryDirectory() as directory:
            notes = Path(directory) / "notes.md"
            notes.write_text("### Fixes\n")
            self.assertEqual(release_notes_file("v1.0.0", notes), notes.resolve())

    def test_a_release_refuses_missing_notes_before_building(self):
        with tempfile.TemporaryDirectory() as directory:
            for notes in (None, Path(directory) / "missing.md", Path(directory)):
                with self.subTest(notes=notes), self.assertRaises(TaskError) as refused:
                    release_notes_file("v1.0.0", notes)
                self.assertIn("embeds its release notes", str(refused.exception))

    def test_a_build_that_is_not_a_release_embeds_no_notes(self):
        self.assertIsNone(release_notes_file(None, None))


if __name__ == "__main__":
    unittest.main()
