"""Exercise the release rehearsal capture without a desktop or a screen."""

import json
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.release_rehearsal import (
    CORNER_HEIGHT,
    CORNER_WIDTH,
    capture,
    corner,
    display,
    prepare,
    window,
)

PRIMARY = {
    "bounds": {"x": 0.0, "y": 0.0, "width": 1512.0, "height": 982.0},
    "is_primary": True,
    "scale": 2.0,
}
SECOND = {
    "bounds": {"x": 1512.0, "y": -200.0, "width": 1920.0, "height": 1080.0},
    "is_primary": False,
    "scale": 1.0,
}


def png(width, height):
    """Return the PNG signature and IHDR chunk, which is all the capture reads."""
    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + header + b"\0\0\0\0"


def listed(app, pid, *, focused=True, visible=True, x=10.0, width=1200.0, window_id="w-1"):
    return {
        "app_name": app,
        "pid": pid,
        "id": window_id,
        "is_focused": focused,
        "visible": visible,
        "accessible": True,
        "bounds": {"x": x, "y": 40.0, "width": width, "height": 800.0},
    }


class Desktop:
    """A fake agent-desktop and sips that record every call and write the files they name.

    By default it lists one 2x display and captures it at 3024 by 1964 pixels.
    With a clock, each command records when it started and then takes `duration` seconds.
    """

    def __init__(self, listings, displays=(PRIMARY,), clock=None, duration=0.0):
        self.listings = list(listings)
        self.displays = list(displays)
        self.clock = clock
        self.duration = duration
        self.calls = []
        self.started = []

    def __call__(self, command, **_options):
        self.calls.append(command)
        if self.clock:
            self.started.append(self.clock.now)
            self.clock.now += self.duration
        if command[:2] == ["agent-desktop", "list-windows"]:
            listing = self.listings.pop(0) if len(self.listings) > 1 else self.listings[0]
            return subprocess.CompletedProcess(
                command, 0, json.dumps({"ok": True, "data": listing}), ""
            )
        if command[:2] == ["agent-desktop", "list-displays"]:
            return subprocess.CompletedProcess(
                command, 0, json.dumps({"ok": True, "data": self.displays}), ""
            )
        if command[:2] == ["agent-desktop", "screenshot"]:
            Path(command[-1]).write_bytes(png(3024, 1964))
            return subprocess.CompletedProcess(command, 0, '{"ok":true}', "")
        if command[0] == "sips":
            Path(command[-1]).write_bytes(png(1, 1))
            return subprocess.CompletedProcess(command, 0, "", "")
        raise AssertionError(command)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class ReleaseRehearsalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_the_focused_visible_window_of_the_app_is_captured(self):
        windows = [
            listed("Helium", 1),
            listed("SpaceTerm", 2, focused=False, window_id="w-2"),
            listed("SpaceTerm", 2, visible=False, window_id="w-3"),
            listed("SpaceTerm", 2, focused=True, width=600.0, window_id="w-4"),
        ]
        self.assertEqual(window(windows, "SpaceTerm")["id"], "w-4")

    def test_without_a_focused_window_the_largest_visible_one_is_captured(self):
        windows = [
            listed("SpaceTerm", 2, focused=False, width=600.0, window_id="w-2"),
            listed("SpaceTerm", 2, focused=False, width=1400.0, window_id="w-3"),
        ]
        self.assertEqual(window(windows, "SpaceTerm")["id"], "w-3")

    def test_an_app_without_a_visible_window_has_nothing_to_capture(self):
        self.assertIsNone(window([listed("SpaceTerm", 2, visible=False)], "SpaceTerm"))

    def test_the_corner_is_the_trailing_end_of_the_title_bar_in_display_pixels(self):
        bounds = {"x": 10.0, "y": 40.0, "width": 1200.0, "height": 800.0}
        self.assertEqual(
            corner(bounds, PRIMARY, (3024, 1964)),
            ((1210 - CORNER_WIDTH) * 2, 80, CORNER_WIDTH * 2, CORNER_HEIGHT * 2),
        )

    def test_a_window_narrower_than_the_corner_is_captured_whole_across(self):
        bounds = {"x": 100.0, "y": 50.0, "width": 300.0, "height": 400.0}
        self.assertEqual(corner(bounds, PRIMARY, (3024, 1964)), (200, 100, 600, CORNER_HEIGHT * 2))

    def test_a_corner_on_a_second_display_is_measured_from_that_display(self):
        bounds = {"x": 2000.0, "y": -100.0, "width": 1000.0, "height": 600.0}
        index, screen = display([PRIMARY, SECOND], bounds)
        self.assertEqual(index, 1)
        self.assertEqual(
            corner(bounds, screen, (1920, 1080)),
            (3000 - CORNER_WIDTH - 1512, 100, CORNER_WIDTH, CORNER_HEIGHT),
        )

    def test_a_corner_that_straddles_two_displays_is_refused(self):
        # The corner spans x 1120 to 1600; only 1512 to 1600 lies on the second display.
        desktop = Desktop(
            [[], [listed("SpaceTerm", 200, x=1000.0, width=600.0)]], [PRIMARY, SECOND]
        )
        clock = Clock()
        with self.assertRaisesRegex(TaskError, "move the app's window"):
            capture(self.root, "SpaceTerm", run=desktop, clock=clock.monotonic, sleep=clock.sleep)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_corner_off_every_display_is_refused(self):
        desktop = Desktop(
            [[], [listed("SpaceTerm", 200, x=-900.0, width=600.0)]], [PRIMARY, SECOND]
        )
        clock = Clock()
        with self.assertRaisesRegex(TaskError, "move the app's window"):
            capture(self.root, "SpaceTerm", run=desktop, clock=clock.monotonic, sleep=clock.sleep)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_folder_that_already_holds_files_is_refused(self):
        (self.root / "frame.png").touch()
        with self.assertRaises(TaskError):
            prepare(self.root)

    def test_a_new_folder_is_created(self):
        folder = self.root / "strip" / "candidate"
        prepare(folder)
        self.assertTrue(folder.is_dir())

    def test_capture_waits_for_the_relaunch_then_writes_one_frame_a_second(self):
        before = [listed("SpaceTerm", 100)]
        after = [listed("SpaceTerm", 200)]
        clock = Clock()
        # Every external command takes an eighth of a second, so each frame's four commands take
        # half of its second and the schedule must wait out the rest.
        desktop = Desktop([before, before, [], after], clock=clock, duration=0.125)
        capture(self.root, "SpaceTerm", run=desktop, clock=clock.monotonic, sleep=clock.sleep)

        names = [f"second-{second:02d}.png" for second in range(60)]
        self.assertEqual(sorted(path.name for path in self.root.iterdir()), names)
        shots = [
            (started, call)
            for started, call in zip(desktop.started, desktop.calls, strict=True)
            if call[1] == "screenshot"
        ]
        # Three listings and two one-second polls put the relaunched window at 2.5 seconds. Each
        # frame lists the windows and the displays before its screenshot.
        self.assertEqual([started for started, _ in shots], [2.75 + second for second in range(60)])
        display_capture = str(self.root / "display.png")
        self.assertTrue(all(call[2:] == ["--screen", "0", display_capture] for _, call in shots))
        # The window's corner spans x 730 to 1210 and y 40 to 96 points on a 2x display.
        crops = [call for call in desktop.calls if call[0] == "sips"]
        self.assertEqual(
            crops,
            [
                [
                    "sips",
                    "--cropToHeightWidth",
                    "112",
                    "960",
                    "--cropOffset",
                    "80",
                    "1460",
                    display_capture,
                    "--out",
                    str(self.root / name),
                ]
                for name in names
            ],
        )

    def test_capture_starts_at_once_when_no_window_was_open_before(self):
        desktop = Desktop([[], [listed("SpaceTerm", 200)]])
        clock = Clock()
        capture(self.root, "SpaceTerm", run=desktop, clock=clock.monotonic, sleep=clock.sleep)
        self.assertEqual(len(list(self.root.iterdir())), 60)
        self.assertEqual(clock.now, 59.0)

    def test_a_window_that_disappears_during_capture_stops_the_task(self):
        after = [listed("SpaceTerm", 200)]
        desktop = Desktop([[], after, after, []])
        clock = Clock()
        with self.assertRaises(TaskError):
            capture(self.root, "SpaceTerm", run=desktop, clock=clock.monotonic, sleep=clock.sleep)


if __name__ == "__main__":
    unittest.main()
