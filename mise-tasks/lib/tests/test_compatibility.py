"""The release workflow pairs this commit with every listed release, both ways."""

import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.compatibility import (
    PROTOCOLS,
    RELEASES,
    candidate_tag,
    expected,
    plan,
    releases,
    restart_into,
    verdict,
)


class FloorCore:
    """The CLI of a floor Core with the release waiting as its Pending Version.

    The Core may start the release on its own at call `silent`, as its watcher does with no
    live Terminal Session. That makes an issued confirmation stale. `busy` calls fail as a
    Core in the middle of a restart does. `lag` status reads after a confirmed restart still
    show the versions before it. `activating` status reads come from the middle of a silent
    restart, with nothing pending and the release not yet running, before it completes. Each
    command takes `duration` seconds on `clock`, or its timeout when that is shorter.
    """

    def __init__(self, silent=None, busy=(), lag=0, activating=0, clock=None, duration=0):
        self.runtime, self.pending = "1.0.0", "1.1.0"
        self.silent, self.busy, self.lag = silent, set(busy), lag
        self.activating = activating
        self.before = (self.runtime, self.pending)
        self.calls = 0
        self.restarts = 0
        self.token = None
        self.clock, self.duration = clock, duration
        self.started = []

    def __call__(self, arguments, status="ok", timeout=60.0):
        if self.clock is not None:
            self.started.append((self.clock.now, timeout))
            self.clock.now += min(self.duration, timeout)
            if self.duration > timeout:
                raise TaskError(f"the {arguments[0]} command returned no envelope")
        self.calls += 1
        if self.calls == self.silent:
            self.runtime, self.pending, self.token = self.pending, None, None
        if self.calls in self.busy:
            raise TaskError("the status command ended in core_busy")
        if arguments == ["status"] and self.activating:
            self.activating -= 1
            if not self.activating:
                self.runtime, self.pending, self.token = self.pending, None, None
            return {"data": {"runtime": {"version": "1.0.0"}, "running": True, "pending": None}}
        if arguments == ["status"]:
            runtime, pending = self.runtime, self.pending
            if self.restarts and self.lag:
                runtime, pending = self.before
                self.lag -= 1
            pending = {"version": pending} if pending else None
            return {"data": {"runtime": {"version": runtime}, "running": True, "pending": pending}}
        if arguments == ["restart"] and status == "confirmation_required":
            # With nothing pending, the preview offers a restart into the running version.
            self.token = f"c1-{self.calls}"
            target = {"from": self.runtime, "to": self.pending} if self.pending else None
            effects = {"ends_sessions": 0, "sessions": [], "pending": target}
            return {"data": {"confirmation": {"token": self.token, "effects": effects}}}
        if arguments[:2] == ["restart", "--confirm"] and arguments[2] == self.token:
            # A restart with nothing pending restarts the running version and ends its sessions.
            self.restarts += 1
            self.before = (self.runtime, self.pending)
            self.runtime, self.pending, self.token = self.pending or self.runtime, None, None
            return {"data": {}}
        raise TaskError(f"the {arguments[0]} command ended in confirmation_stale")


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class RestartIntoTests(unittest.TestCase):
    def restart(self, core, clock=None):
        clock = clock or Clock()
        return restart_into("1.1.0", core, clock=clock, sleep=clock.sleep)

    def test_a_confirmed_restart_moves_the_core_to_the_release(self):
        core = FloorCore()
        self.assertEqual(self.restart(core)["data"]["runtime"]["version"], "1.1.0")
        self.assertEqual(core.restarts, 1)

    def test_a_confirmed_restart_runs_once_while_status_still_shows_the_version_before_it(self):
        core = FloorCore(lag=2)
        self.assertEqual(self.restart(core)["data"]["runtime"]["version"], "1.1.0")
        self.assertEqual(core.restarts, 1)

    def test_a_silent_restart_before_the_preview_needs_no_confirmed_restart(self):
        core = FloorCore(silent=1)
        self.assertEqual(self.restart(core)["data"]["runtime"]["version"], "1.1.0")
        self.assertEqual(core.restarts, 0)

    def test_a_silent_restart_after_the_preview_leaves_a_stale_confirmation_and_passes(self):
        for silent, busy in ((2, ()), (2, {3}), (3, ()), (3, {4})):
            with self.subTest(silent=silent, busy=busy):
                core = FloorCore(silent=silent, busy=busy)
                self.assertEqual(self.restart(core)["data"]["runtime"]["version"], "1.1.0")
                self.assertEqual(core.restarts, 0)

    def test_a_status_during_a_silent_restart_starts_no_restart(self):
        core = FloorCore(activating=2)
        self.assertEqual(self.restart(core)["data"]["runtime"]["version"], "1.1.0")
        self.assertEqual(core.restarts, 0)

    def test_no_command_starts_or_runs_past_the_deadline(self):
        # A busy Core whose commands take 50 seconds, and one whose status and preview take
        # the full 60 seconds each, so the preview ends exactly at the deadline.
        for duration, busy in ((50, range(1, 1000)), (60, ())):
            with self.subTest(duration=duration):
                clock = Clock()
                core = FloorCore(busy=busy, clock=clock, duration=duration)
                with self.assertRaisesRegex(TaskError, "did not restart into 1.1.0"):
                    self.restart(core, clock)
                self.assertTrue(core.started)
                for started, timeout in core.started:
                    self.assertLess(started, 120)
                    self.assertLessEqual(started + timeout, 120)
                self.assertEqual(core.restarts, 0)

    def test_a_core_that_never_runs_the_release_fails_the_pair(self):
        core = FloorCore(busy=range(1, 1000))
        with self.assertRaisesRegex(TaskError, "did not restart into 1.1.0"):
            self.restart(core)


class CompatibilityPlanTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def listed(self, text):
        path = self.root / "compatibility.toml"
        path.write_text(text)
        return path

    def planned(self, path, version="2.1.0", published=(), majors=None):
        output = self.root / "output"
        output.write_text("")
        plan(
            output,
            version,
            path,
            published=lambda: set(published),
            work_major=lambda tag: (majors or {})[tag],
        )
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

    def test_the_plan_refuses_a_list_that_omits_a_published_release_on_a_paired_major(self):
        # A release published on this work major or the previous one must be paired before the
        # next release publishes. Older majors, releases from before the greenfield line, and the
        # planned release itself on a rerun after it published are not.
        mine = PROTOCOLS["work"]["major"]
        published = {"0.6.8", "1.0.0", "1.1.0", "1.2.0", "1.3.0"}
        majors = {"v1.0.0": mine - 2, "v1.1.0": mine - 1, "v1.2.0": mine}
        path = self.listed('releases = ["v1.1.0"]\n')
        with self.assertRaisesRegex(TaskError, "omits published releases v1.2.0$"):
            self.planned(path, "1.3.0", published, majors)
        path = self.listed('releases = ["v1.1.0", "v1.2.0"]\n')
        self.assertEqual(self.planned(path, "1.3.0", published, majors)["any"], "true")
        for version in ("1.3", "v1.3.0"):
            with self.subTest(version=version), self.assertRaises(TaskError):
                self.planned(path, version, published, majors)

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
        def protocols(work, service):
            return {"work": {"major": work, "minor": 0}, "service": {"major": service, "minor": 4}}

        current = protocols(3, 2)
        works = {"list": "ok", "status": "ok"}
        for release in (protocols(3, 2), protocols(2, 2), protocols(3, 1), protocols(2, 1)):
            with self.subTest(release=release):
                self.assertEqual(expected("client", release, current), works)
        self.assertEqual(expected("core", protocols(3, 2), current), works)
        self.assertEqual(
            expected("core", protocols(2, 2), current), {"list": "cli_too_old", "status": "ok"}
        )
        self.assertEqual(
            expected("core", protocols(3, 1), current), {"list": "ok", "status": "cli_too_old"}
        )
        for release in (protocols(1, 2), protocols(4, 2), protocols(3, 0), protocols(3, 3)):
            for candidate in ("client", "core"):
                with self.subTest(candidate=candidate, release=release):
                    with self.assertRaises(TaskError):
                        expected(candidate, release, current)

    def test_planned_pairs_must_all_pass_and_skipped_pairs_pass_only_without_any(self):
        verdict("success", "true", "success")
        verdict("success", "false", "skipped")
        for planning, planned, pairs in [
            ("success", "true", "skipped"),
            ("success", "true", "failure"),
            ("success", "true", "cancelled"),
            ("success", "false", "success"),
            ("success", "", "skipped"),
            ("failure", "false", "skipped"),
            ("failure", "true", "success"),
        ]:
            with self.subTest(planning=planning, planned=planned, pairs=pairs):
                with self.assertRaises(TaskError):
                    verdict(planning, planned, pairs)
