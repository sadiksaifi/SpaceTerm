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
        self.released = []
        self.unpublished = set()
        self.git("init", "-q")
        self.git("commit", "-qm", "Initial", "--allow-empty")
        self.withdraw()

    def published(self):
        """The versions GitHub lists as published releases: every tagged one not left out."""
        return set(self.released) - self.unpublished

    def withdraw(self, *versions):
        path = self.root / release_floor.WITHDRAWN
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"releases = {list(versions)!r}\n".replace("'", '"'))

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

    def release(self, version, text, *, published=True):
        self.commit(text)
        self.git("tag", "-a", f"v{version}", "-m", "Release")
        self.released.append(version)
        if not published:
            self.unpublished.add(version)

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

    def floor(self, product, version):
        return release_floor.floor(product, version, self.root, self.repository.published)

    def test_the_first_release_with_data_generations_is_its_own_floor(self):
        self.repository.release("0.6.8", None)
        self.repository.release("0.7.0", None)
        self.repository.commit(source())
        self.assertEqual(self.floor("core", "1.0.0"), "1.0.0")
        self.assertEqual(self.floor("desktop", "1.0.0"), "1.0.0")

    def test_the_floor_is_the_oldest_release_at_or_above_every_oldest_of_the_product(self):
        self.repository.release("0.6.8", None)
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CLI_STATE=(1, 2)))
        self.repository.release("1.2.0", source(CLI_STATE=(1, 2), DESKTOP_STATE=(1, 2)))
        # Only a release older than the candidate counts, as when a patch follows a newer line.
        self.repository.release("1.3.0", source(CLI_STATE=(1, 3), DESKTOP_STATE=(1, 3)))
        self.repository.commit(source(CLI_STATE=(2, 2), DESKTOP_STATE=(2, 2)))
        self.assertEqual(self.floor("core", "1.2.1"), "1.1.0")
        self.assertEqual(self.floor("desktop", "1.2.1"), "1.2.0")
        self.assertEqual(
            release_floor.floors("1.2.1", self.root, self.repository.published),
            {"core": "1.1.0", "desktop": "1.2.0"},
        )

    def test_a_release_with_no_published_release_to_step_from_is_refused(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CORE_DATA=(1, 2)))
        # A newer release line reaches generation 3, but a patch of 1.1 cannot step from it.
        self.repository.release("1.3.0", source(CORE_DATA=(3, 3)))
        self.repository.commit(source(CORE_DATA=(3, 3)))
        with self.assertRaisesRegex(TaskError, "no published release"):
            self.floor("core", "1.2.0")
        self.assertEqual(self.floor("desktop", "1.2.0"), "1.0.0")

    def test_only_a_published_release_is_a_floor(self):
        # 1.0.0 failed before GitHub published it, so its tag exists without a release.
        self.repository.release("1.0.0", source(), published=False)
        self.repository.release("1.1.0", source())
        self.repository.commit(source())
        self.assertEqual(self.floor("core", "1.2.0"), "1.1.0")
        self.repository.unpublished.add("1.1.0")
        self.assertEqual(self.floor("core", "1.2.0"), "1.2.0")

    def test_a_published_release_needs_its_tag_in_this_checkout(self):
        self.repository.commit(source())
        self.repository.released.append("1.0.0")
        with self.assertRaisesRegex(TaskError, "fetch"):
            self.floor("core", "1.1.0")

    def test_a_withdrawn_release_is_skipped_and_its_repair_takes_its_place(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CORE_DATA=(1, 2)))
        self.repository.release("1.1.1", source(CORE_DATA=(1, 2)))
        self.repository.commit(source(CORE_DATA=(2, 2)))
        # An ordinary patch release adds no step.
        self.assertEqual(self.floor("core", "1.2.0"), "1.1.0")
        self.repository.withdraw("1.1.0")
        self.assertEqual(self.floor("core", "1.2.0"), "1.1.1")
        self.assertEqual(self.floor("desktop", "1.2.0"), "1.0.0")
        # The newest release before this one is withdrawn, and an older one still qualifies.
        self.repository.withdraw("1.1.1")
        self.assertEqual(self.floor("core", "1.2.0"), "1.1.0")

    def test_the_withdrawn_list_names_distinct_published_releases(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(), published=False)
        self.repository.commit(source())
        path = self.root / release_floor.WITHDRAWN
        for text in (
            'releases = ["1.01.0"]\n',
            'releases = ["v1.0.0"]\n',
            'releases = ["1.1.0"]\n',
            'releases = ["1.0.0", "1.0.0"]\n',
            'releases = "1.0.0"\n',
            "other = []\n",
            "releases = [\n",
        ):
            with self.subTest(text=text):
                path.write_text(text)
                with self.assertRaises(TaskError):
                    self.floor("core", "1.2.0")
        path.unlink()
        with self.assertRaises(TaskError):
            self.floor("core", "1.2.0")

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
            self.floor("core", "1.1.0")


class ReleaseCheckTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repository = Repository(Path(temporary.name))
        self.root = self.repository.root

    def check(self, version):
        return release_floor.check(version, self.root, self.repository.published)

    def complete_fixtures(self, **ranges):
        for name, root in release_floor.ROOTS.items():
            oldest, current = ranges.get(name, (1, 1))
            self.repository.fixtures(root, *range(oldest, current + 1))

    def test_a_consistent_release_passes_and_reports_both_floors(self):
        self.repository.release("1.0.0", source())
        self.repository.release("1.1.0", source(CORE_DATA=(1, 2)))
        self.repository.commit(source(CORE_DATA=(2, 3)))
        self.complete_fixtures(CORE_DATA=(2, 3))
        self.assertEqual(self.check("1.2.0"), {"core": "1.1.0", "desktop": "1.0.0"})

    def test_a_first_release_is_its_own_floor(self):
        self.repository.commit(source())
        self.complete_fixtures()
        self.assertEqual(self.check("1.0.0"), {"core": "1.0.0", "desktop": "1.0.0"})

    def test_greenfield_history_ignores_legacy_generations_and_starts_at_zero_ten(self):
        self.repository.release("0.6.8", source(CORE_DATA=(70, 99)))
        self.repository.release("0.9.9", source(DESKTOP_STATE=(8, 12)))
        self.repository.commit(source())
        self.complete_fixtures()
        self.assertEqual(self.check("0.10.0"), {"core": "0.10.0", "desktop": "0.10.0"})

    def test_generations_compare_against_the_previous_published_release_only(self):
        self.repository.release("1.0.0", source())
        # 1.0.1 failed before publication with generations no published release ever had.
        self.repository.release("1.0.1", source(CORE_DATA=(1, 3)), published=False)
        self.repository.commit(source(CORE_DATA=(1, 2)))
        self.complete_fixtures(CORE_DATA=(1, 2))
        self.assertEqual(self.check("1.1.0"), {"core": "1.0.0", "desktop": "1.0.0"})

    def test_generations_never_decrease_against_the_previous_release(self):
        self.repository.release("1.0.0", source(DESKTOP_CONFIG=(1, 2)))
        self.repository.release("1.1.0", source(DESKTOP_CONFIG=(2, 3)))
        for declared in ((2, 2), (1, 3)):
            with self.subTest(declared=declared):
                self.repository.commit(source(DESKTOP_CONFIG=declared))
                self.complete_fixtures(DESKTOP_CONFIG=declared)
                with self.assertRaisesRegex(TaskError, "DESKTOP_CONFIG decreased"):
                    self.check("1.2.0")

    def test_every_generation_from_oldest_to_current_has_a_fixture(self):
        self.repository.commit(source(CLI_STATE=(1, 3)))
        self.complete_fixtures()
        self.repository.fixtures("cli-state", 3)
        with self.assertRaisesRegex(TaskError, "cli-state/gen-2"):
            self.check("1.0.0")
        (self.root / release_floor.FIXTURES / "cli-state/gen-2").mkdir()
        with self.assertRaisesRegex(TaskError, "cli-state/gen-2"):
            self.check("1.0.0")
        self.repository.fixtures("cli-state", 2)
        self.check("1.0.0")


if __name__ == "__main__":
    unittest.main()
