"""Capture the title bar's trailing corner during the release rehearsal of ADR 0065.

A person runs the recorded update journey by hand. This task prints its steps, waits for SpaceTerm
to relaunch, and then captures the trailing corner of its title bar once a second for a minute.
agent-desktop finds the window and captures the display under it, and sips crops the corner. A
display capture keeps the crop exact, because a window capture adds a shadow of unknown size.
"""

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

from spaceterm_tasks import TaskError, main

FRAMES = 60
INTERVAL = 1.0
# How long to wait for the relaunched window before giving up, in seconds.
RELAUNCH_LIMIT = 600.0
# The captured corner in points, measured from the window's top trailing edge. It holds the split
# pill at every Appearance font size.
CORNER_WIDTH = 480
CORNER_HEIGHT = 56

STEPS = """\
Release rehearsal (docs/adr/0065-present-updates-as-one-split-pill-and-one-ladder.md)

1. On This Mac, run the previous published release with 2 open Terminal Sessions.
   On Penguin, run the previous published release with no Terminal Session. Use
   ssh penguin for its commands and Remora for its desktop.
2. Start this task while the previous release's window is open: it must see that
   window first. Leave it running. On This Mac, download the candidate and
   relaunch SpaceTerm into it from the title bar.
3. The capture starts when the relaunched SpaceTerm shows its window, and takes
   one frame a second for 60 seconds. Keep that window in front and unmoved: the
   capture records the screen under its title bar.
4. During the capture, open the update pill's chevron and keep the list open for
   a few seconds.
5. After the capture, approve one restart of SpaceTerm Core on This Mac from the
   list and confirm it.
6. Compare the frames with the expected timeline of replay R1, and attach both to
   the release PR. A mismatch blocks publication.
"""


def window(windows, app):
    """Return the window to capture: the app's focused visible window, else its largest one."""
    shown = [
        entry
        for entry in windows
        if entry.get("app_name", "").casefold() == app.casefold() and entry.get("visible")
    ]
    if not shown:
        return None
    focused = [entry for entry in shown if entry.get("is_focused")]
    return max(
        focused or shown, key=lambda entry: entry["bounds"]["width"] * entry["bounds"]["height"]
    )


def png_size(data):
    """Return the (width, height) a PNG's IHDR chunk declares."""
    if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise TaskError("agent-desktop wrote a screenshot that is not a PNG")
    return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")


REPOSITION = (
    "the trailing end of the app's title bar is not wholly on one display; move the app's window "
    "so it is, then run the task again"
)


def display(displays, bounds):
    """Return (index, display) for the display under the window's top trailing corner.

    The index is the one `agent-desktop screenshot --screen` takes.
    """
    x = bounds["x"] + bounds["width"] - 1
    y = bounds["y"]
    for index, entry in enumerate(displays):
        area = entry["bounds"]
        if (
            area["x"] <= x < area["x"] + area["width"]
            and area["y"] <= y < area["y"] + area["height"]
        ):
            return index, entry
    raise TaskError(REPOSITION)


def corner(bounds, screen, pixels):
    """Return the trailing corner of the window's title bar as (x, y, width, height) in pixels of
    a capture of `screen` that is `pixels` in size. A corner not wholly on `screen` is refused."""
    area = screen["bounds"]
    right = bounds["x"] + bounds["width"]
    left = max(bounds["x"], right - CORNER_WIDTH)
    top = bounds["y"]
    if (
        left < area["x"]
        or top < area["y"]
        or right > area["x"] + area["width"]
        or top + CORNER_HEIGHT > area["y"] + area["height"]
    ):
        raise TaskError(REPOSITION)
    scale = screen["scale"]
    x = round((left - area["x"]) * scale)
    y = round((top - area["y"]) * scale)
    width = round((right - left) * scale)
    height = round(CORNER_HEIGHT * scale)
    if x + width > pixels[0] or y + height > pixels[1]:
        raise TaskError("agent-desktop captured less of the display than it listed")
    return x, y, width, height


def prepare(folder):
    """Create the strip's folder, refusing one that already holds files."""
    folder = Path(folder)
    if folder.exists() and any(folder.iterdir()):
        raise TaskError("the capture folder must be new or empty")
    folder.mkdir(parents=True, exist_ok=True)
    return folder


def _ask(command, run):
    result = run(["agent-desktop", *command], capture_output=True, text=True, check=False)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        raise TaskError(f"agent-desktop did not answer {command[0]}") from None


def _windows(app, run):
    reply = _ask(["list-windows", "--app", app], run)
    if reply.get("ok"):
        return reply.get("data") or []
    if (reply.get("error") or {}).get("code") == "APP_NOT_FOUND":
        return []
    raise TaskError("agent-desktop could not list windows")


def _displays(run):
    reply = _ask(["list-displays"], run)
    if not reply.get("ok"):
        raise TaskError("agent-desktop could not list displays")
    return reply.get("data") or []


def _frame(folder, index, target, run):
    screen_index, screen = display(_displays(run), target["bounds"])
    screenshot = folder / "display.png"
    try:
        if not _ask(["screenshot", "--screen", str(screen_index), str(screenshot)], run).get("ok"):
            raise TaskError("agent-desktop could not capture the display")
        with screenshot.open("rb") as file:
            x, y, width, height = corner(target["bounds"], screen, png_size(file.read(24)))
        run(
            [
                "sips",
                "--cropToHeightWidth",
                str(height),
                str(width),
                "--cropOffset",
                str(y),
                str(x),
                str(screenshot),
                "--out",
                str(folder / f"second-{index:02d}.png"),
            ],
            capture_output=True,
            check=True,
        )
    finally:
        screenshot.unlink(missing_ok=True)


def capture(folder, app, *, run=subprocess.run, clock=time.monotonic, sleep=time.sleep):
    """Wait for the app to relaunch, then write one frame a second for FRAMES seconds."""
    folder = Path(folder)
    before = window(_windows(app, run), app)
    previous = before["pid"] if before else None
    waited = clock()
    while True:
        target = window(_windows(app, run), app)
        if target and target["pid"] != previous:
            break
        if clock() - waited >= RELAUNCH_LIMIT:
            raise TaskError("the app did not relaunch within 10 minutes")
        sleep(INTERVAL)
    start = clock()
    for index in range(FRAMES):
        delay = start + index * INTERVAL - clock()
        if delay > 0:
            sleep(delay)
        target = window(_windows(app, run), app)
        if target is None:
            raise TaskError("the app's window closed during the capture")
        _frame(folder, index, target, run)


def rehearse(folder, app):
    if sys.platform != "darwin":
        raise TaskError("the release rehearsal capture runs on macOS")
    folder = prepare(folder)
    print(STEPS)
    print(f"Waiting for {app} to relaunch.", flush=True)
    capture(folder, app)
    print(f"Wrote {FRAMES} frames.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("folder", help="New or empty folder for the frames")
    parser.add_argument("--app", default="SpaceTerm", help="Application name to capture")
    args = parser.parse_args()
    main(lambda: rehearse(args.folder, args.app))
