"""The compatibility workflow pairs this commit with every listed release, both ways."""

import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.compatibility import RELEASES, candidate_tag, expected, plan, releases


class CompatibilityPlanTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def listed(self, text):
        path = self.root / "compatibility.toml"
        path.write_text(text)
        return path

    def planned(self, path):
        output = self.root / "output"
        output.write_text("")
        plan(output, path)
        return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_a_listed_release_is_paired_with_this_commit_as_each_side(self):
        outputs = self.planned(self.listed('releases = ["v1.2.0", "v2.0.3"]\n'))
        self.assertEqual(outputs["any"], "true")
        self.assertEqual(
            outputs["matrix"],
            '{"include":['
            '{"release":"v1.2.0","candidate":"client"},'
            '{"release":"v1.2.0","candidate":"core"},'
            '{"release":"v2.0.3","candidate":"client"},'
            '{"release":"v2.0.3","candidate":"core"}]}',
        )

    def test_the_tracked_list_plans_and_an_empty_list_plans_no_pair(self):
        releases(RELEASES)
        outputs = self.planned(self.listed("releases = []\n"))
        self.assertEqual(outputs, {"matrix": '{"include":[]}', "any": "false"})

    def test_the_list_holds_distinct_stable_releases_oldest_first(self):
        for text in [
            "",
            'releases = "v1.0.0"\n',
            'releases = ["1.0.0"]\n',
            'releases = ["v0.6.8"]\n',
            'releases = ["v1.0.0-rc.1"]\n',
            'releases = ["v1.10.0", "v1.9.0"]\n',
            'releases = ["v1.0.0", "v1.0.0"]\n',
        ]:
            with self.subTest(text=text), self.assertRaises(TaskError):
                releases(self.listed(text))
        self.assertEqual(
            releases(self.listed('releases = ["v1.9.0", "v1.10.0"]\n')), ["v1.9.0", "v1.10.0"]
        )

    def test_the_candidate_is_tagged_one_minor_past_the_newest_release(self):
        self.assertEqual(candidate_tag(["v1.2.0", "v2.0.3"]), "v2.1.0")

    def test_this_cli_works_one_major_back_and_that_releases_cli_is_refused_as_older(self):
        works = {"list": {"ok"}, "status": {"ok"}}
        self.assertEqual(expected("client", 3, current=3), works)
        self.assertEqual(expected("client", 2, current=3), works)
        self.assertEqual(
            expected("core", 3, current=3), {"list": {"ok"}, "status": {"ok", "cli_too_old"}}
        )
        self.assertEqual(
            expected("core", 2, current=3),
            {"list": {"cli_too_old"}, "status": {"ok", "cli_too_old"}},
        )
        for candidate in ("client", "core"):
            for major in (1, 4):
                with self.subTest(candidate=candidate, major=major), self.assertRaises(TaskError):
                    expected(candidate, major, current=3)
