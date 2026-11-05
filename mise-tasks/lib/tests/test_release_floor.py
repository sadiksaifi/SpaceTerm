"""Exercise the Update Floor each product's release computes from released Data Generations."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, release_floor

ROOTS = ("CORE_DATA", "CLI_STATE", "DESKTOP_CONFIG", "DESKTOP_STATE")


def source(**declared):
    """generations.rs with every root at generation 1 unless `declared` names (oldest, current)."""
    lines = []
    for name in ROOTS:
        oldest, current = declared.get(name, (1, 1))
        lines.append(
            f"pub const {name}: Generations = Generations {{ oldest: {oldest}, current: {current} }};"
        )
    return "use super::Generations;\n\n" + "\n".join(lines) + "\n"


class Repository:
    def __init__(self, root):
        self.root = root
        self.git("init", "-q")
        self.git("commit", "-qm", "Initial", "--allow-empty")

    def git(self, *args):
        return subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", *args],
            cwd=self.root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def commit(self, text):
        path = self.root / release_floor.SOURCE
        if text is None:
            if path.exists():
                self.git("rm", "-q", str(release_floor.SOURCE))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
            self.git("add", str(release_floor.SOURCE))
        self.git("commit", "-qm", "Change", "--allow-empty")

    def release(self, version, text):
        self.commit(text)
        self.git("tag", "-a", f"v{version}", "-m", "Release")

    def fixtures(self, root, *generations):
        for generation in generations:
            directory = self.root / release_floor.FIXTURES / root / f"gen-{generation}"
            directory.mkdir(parents=True, exist_ok=True)
            (directory / "fixture.json").write_text("{}\n")


class FloorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repository = Repository(Path(temporary.name))
        self.root = self.repository.root

    def test_the_first_release_with_data_generations_is_its_own_floor(self):
        self.repository.release("0.6.8", None)
        self.repository.release("0.7.0", None)
        self.repository.commit(source())
        self.assertEqual(release_floor.floor("core", "1.0.0", self.root), "1.0.0")
        self.assertEqual(release_floor.floor("desktop", "1.0.0", self.root), "1.0.0")

    def test_the_floor_is_the_oldest_release_at_or_above_every_oldest_of_the_product(self):
        self.repository.release("0.6.8", None)
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CLI_STATE=(1, 2)))
        self.repository.release("1.2.0", source(CLI_STATE=(1, 2), DESKTOP_STATE=(1, 2)))
        # Only a release older than the candidate counts, as when a patch follows a newer line.
        self.repository.release("1.3.0", source(CLI_STATE=(1, 3), DESKTOP_STATE=(1, 3)))
        self.repository.commit(source(CLI_STATE=(2, 2), DESKTOP_STATE=(2, 2)))
        self.assertEqual(release_floor.floor("core", "1.2.1", self.root), "1.1.0")
        self.assertEqual(release_floor.floor("desktop", "1.2.1", self.root), "1.2.0")
        self.assertEqual(
            release_floor.floors("1.2.1", self.root), {"core": "1.1.0", "desktop": "1.2.0"}
        )

    def test_a_release_with_no_published_release_to_step_from_is_refused(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CORE_DATA=(1, 2)))
        # A newer release line reaches generation 3, but a patch of 1.1 cannot step from it.
        self.repository.release("1.3.0", source(CORE_DATA=(3, 3)))
        self.repository.commit(source(CORE_DATA=(3, 3)))
        with self.assertRaisesRegex(TaskError, "no published release"):
            release_floor.floor("core", "1.2.0", self.root)
        self.assertEqual(release_floor.floor("desktop", "1.2.0", self.root), "1.0.0")

    def test_the_source_declares_each_root_once_with_oldest_at_most_current(self):
        valid = source()
        for text in (
            valid.replace("CLI_STATE", "OTHER"),
            valid + valid.splitlines()[2] + "\n",
            source(CORE_DATA=(2, 1)),
            source(CORE_DATA=(0, 1)),
            valid.replace(" };", "};", 1),
        ):
            with self.subTest(text=text):
                with self.assertRaises(TaskError):
                    release_floor.generations(text)
        self.assertEqual(
            release_floor.generations(source(DESKTOP_CONFIG=(2, 4)))["DESKTOP_CONFIG"], (2, 4)
        )

    def test_a_malformed_released_source_is_refused_rather_than_skipped(self):
        self.repository.release("1.0.0", "pub const CORE_DATA: Generations = broken;\n")
        self.repository.commit(source())
        with self.assertRaises(TaskError):
            release_floor.floor("core", "1.1.0", self.root)


class ReleaseCheckTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repository = Repository(Path(temporary.name))
        self.root = self.repository.root
        self.asked = []
        self.releases = {"1.0.0", "1.1.0"}

    def published(self, version):
        self.asked.append(version)
        return version in self.releases

    def complete_fixtures(self, **ranges):
        for name, root in release_floor.ROOTS.items():
            oldest, current = ranges.get(name, (1, 1))
            self.repository.fixtures(root, *range(oldest, current + 1))

    def test_a_consistent_release_passes_and_reports_both_floors(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CORE_DATA=(1, 2)))
        self.repository.commit(source(CORE_DATA=(2, 3)))
        self.complete_fixtures(CORE_DATA=(2, 3))
        floors = release_floor.check("1.2.0", self.root, self.published)
        self.assertEqual(floors, {"core": "1.1.0", "desktop": "1.0.0"})
        self.releases.remove("1.0.0")
        with self.assertRaisesRegex(TaskError, "1.0.0 is not a published release"):
            release_floor.check("1.2.0", self.root, self.published)

    def test_a_first_release_is_its_own_floor_without_asking_github(self):
        self.repository.commit(source())
        self.complete_fixtures()
        self.assertEqual(
            release_floor.check("1.0.0", self.root, self.published),
            {"core": "1.0.0", "desktop": "1.0.0"},
        )
        self.assertEqual(self.asked, [])

    def test_generations_never_decrease_against_the_previous_release(self):
        self.repository.release("1.0.0", source(DESKTOP_CONFIG=(1, 2)))
        self.repository.release("1.1.0", source(DESKTOP_CONFIG=(2, 3)))
        for declared in ((2, 2), (1, 3)):
            with self.subTest(declared=declared):
                self.repository.commit(source(DESKTOP_CONFIG=declared))
                self.complete_fixtures(DESKTOP_CONFIG=declared)
                with self.assertRaisesRegex(TaskError, "DESKTOP_CONFIG decreased"):
                    release_floor.check("1.2.0", self.root, self.published)

    def test_every_generation_from_oldest_to_current_has_a_fixture(self):
        self.repository.commit(source(CLI_STATE=(1, 3)))
        self.complete_fixtures()
        self.repository.fixtures("cli-state", 3)
        with self.assertRaisesRegex(TaskError, "cli-state/gen-2"):
            release_floor.check("1.0.0", self.root, self.published)
        (self.root / release_floor.FIXTURES / "cli-state/gen-2").mkdir()
        with self.assertRaisesRegex(TaskError, "cli-state/gen-2"):
            release_floor.check("1.0.0", self.root, self.published)
        self.repository.fixtures("cli-state", 2)
        release_floor.check("1.0.0", self.root, self.published)


if __name__ == "__main__":
    unittest.main()
