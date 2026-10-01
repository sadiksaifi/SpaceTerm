#!/usr/bin/env python3
"""Deterministic visual Kitty graphics fixtures using only direct pixel transport.

Run in a dedicated Pane with at least 76 columns and 24 rows. These are visual
checks, not an automatic conformance verdict. A single scenario leaves its image
placements visible after exit, including terminal-driven animation. The next run
cleans up only this fixture's reserved image IDs before clearing the screen.
"""

import argparse
import base64
import struct
import sys
import time
import zlib


ESC = "\x1b"
PLACEHOLDER = "\U0010eeee"
DIACRITICS = "\u0305\u030d\u030e\u0310\u0312\u033d\u033e\u033f\u0346\u034a\u034b\u034c"
IMAGE_IDS = (*range(8901, 8912), 0x02010203)
COLORS = ((230, 70, 70, 255), (70, 200, 110, 255), (65, 125, 240, 255), (245, 200, 65, 255))


def emit(text):
    sys.stdout.write(text)
    sys.stdout.flush()


def at(row, column, text=""):
    emit(f"{ESC}[{row};{column}H{text}")


def command(data=None, **fields):
    fields.setdefault("q", 2)
    control = ",".join(f"{key}={value}" for key, value in fields.items())
    payload = "" if data is None else ";" + base64.b64encode(data).decode("ascii")
    emit(f"{ESC}_G{control}{payload}{ESC}\\")


def pixels(color=None, alpha=255):
    result = bytearray()
    for row in range(24):
        for column in range(24):
            rgba = color or COLORS[(row // 12) * 2 + column // 12]
            result.extend((*rgba[:3], alpha))
    return bytes(result)


def png(data):
    def chunk(kind, content):
        return (
            struct.pack(">I", len(content))
            + kind
            + content
            + struct.pack(">I", zlib.crc32(kind + content))
        )

    scanlines = b"".join(b"\x00" + data[row * 96:(row + 1) * 96] for row in range(24))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">2I5B", 24, 24, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(scanlines))
        + chunk(b"IEND", b"")
    )


def transmit(image_id, data=None, **fields):
    options = dict(a="T", t="d", f=32, i=image_id, s=24, v=24, C=1)
    options.update(fields)
    command(pixels() if data is None else data, **options)


def page(title, explanation):
    for image_id in IMAGE_IDS:
        command(a="d", d="I", i=image_id)
    emit(f"{ESC}[0m{ESC}[2J{ESC}[H")
    at(1, 1, f"SpaceTerm Kitty graphics: {title}")
    at(2, 1, explanation)
    at(23, 1, "Visual check only. Resize, scroll, switch Panes, and compare with Ghostty.")


def basics(_dump):
    page("crops, alpha, and layers", "Four quadrants: red/green above blue/yellow. No image may escape this Pane.")
    at(4, 2, "RGBA alpha, below text")
    at(5, 2)
    transmit(8901, pixels(alpha=180), c=12, r=6, z=-1)
    at(7, 2, "TEXT IS VISIBLE")
    at(4, 36, "PNG, above text")
    at(5, 36, "THIS IS COVERED")
    at(5, 36)
    transmit(8902, png(pixels()), f=100, c=12, r=6, z=0)
    at(12, 2, "Crop: only green quadrant")
    at(13, 2)
    transmit(8903, c=10, r=5, x=12, y=0, w=12, h=12)
    at(12, 36, "Below background: right half hidden")
    at(13, 36)
    transmit(8904, c=12, r=6, z=-1073741825)
    for row in range(13, 19):
        at(row, 42, f"{ESC}[48;2;90;90;90m      {ESC}[0m")
    at(21, 1, "Grey cells hide half of the last image; its other half remains visible.")


def placeholder_grid(row, column, compact=False, gap=False):
    for y in range(6):
        at(row + y, column, f"{ESC}[38;2;1;2;3m{ESC}[58;5;7m")
        for x in range(12):
            if gap and x in (5, 6):
                emit(" ")
                continue
            marks = "" if compact and x else DIACRITICS[y] + DIACRITICS[x] + DIACRITICS[2]
            emit(PLACEHOLDER + marks)
        emit(f"{ESC}[0m")


def placeholders(_dump):
    page("Unicode placeholders", "All previews use a 32-bit image ID and underline color as placement ID.")
    transmit(0x02010203, U=1, p=7, c=12, r=6)
    at(4, 2, "Explicit row/column/high-byte marks")
    placeholder_grid(5, 2)
    at(4, 40, "Omitted marks inherit from left")
    placeholder_grid(5, 40, compact=True)
    at(12, 2, "Text gap: two blank columns")
    placeholder_grid(13, 2, gap=True)
    at(12, 40, "Missing image: blank, no boxes")
    at(13, 40, f"{ESC}[38;5;99m{PLACEHOLDER}{DIACRITICS[0]}{DIACRITICS[0]}{ESC}[0m")
    at(21, 1, "Expect images, never colored missing-glyph boxes or floating diacritics.")


def relative(_dump):
    page("relative placements", "Magenta child overlaps the lower-right of its quadrant parent.")
    at(4, 2, "Parent at row 5, column 2")
    at(5, 2)
    transmit(8901, p=1, c=10, r=5)
    transmit(8902, pixels((220, 70, 200, 255)), p=1, c=4, r=2, P=8901, Q=1, H=8, V=3, z=1)
    at(12, 2, "Virtual transparent parent at row 14, column 2")
    transmit(8903, pixels(alpha=0), U=1, p=1, c=1, r=1)
    transmit(8904, p=1, c=8, r=4, P=8903, Q=1, H=2, V=0)
    at(14, 2, f"{ESC}[38;2;0;34;199m{ESC}[58;5;1m{PLACEHOLDER}{DIACRITICS[0]}{DIACRITICS[0]}{ESC}[0m")
    at(19, 2, "Its visible child begins two columns to the right of the blank anchor.")
    at(21, 1, "Scroll both groups. Children must remain attached to their parents.")


def animation(_dump):
    page("animation", "Left switches red/blue every 600 ms without further output; right stays blue.")
    at(4, 2, "Terminal-driven animation")
    at(5, 2)
    transmit(8901, pixels(COLORS[0]), p=1, c=12, r=6)
    command(pixels(COLORS[2]), a="f", t="d", f=32, i=8901, s=24, v=24, z=600)
    command(a="a", i=8901, r=1, z=600)
    command(a="a", i=8901, s=3, v=1)
    at(4, 38, "Explicit frame 2, stopped")
    at(5, 38)
    transmit(8902, pixels(COLORS[1]), p=1, c=12, r=6)
    command(pixels(COLORS[2]), a="f", t="d", f=32, i=8902, s=24, v=24, z=600)
    command(a="a", i=8902, c=2, s=1)
    at(12, 2, "Second placement of the same animation")
    at(13, 2)
    command(a="p", i=8901, p=2, c=12, r=6, C=1)
    at(21, 1, "Both left images stay in sync. Leave the Pane and return; animation resumes.")


def lifecycle(dump):
    page("replace and delete", "After two seconds: left turns green; middle disappears; right stays blue.")
    for column, image_id, color in ((2, 8901, COLORS[0]), (28, 8902, COLORS[0]), (54, 8903, COLORS[2])):
        at(4, column, {8901: "Replace", 8902: "Delete", 8903: "Unchanged"}[image_id])
        at(5, column)
        transmit(image_id, pixels(color), p=1, c=10, r=5)
    if not dump:
        time.sleep(2)
    at(5, 2)
    transmit(8901, pixels(COLORS[1]), p=1, c=10, r=5)
    command(a="d", d="I", i=8902)
    at(12, 2, "Final state: green / blank / blue. No stale red texture may remain.")
    at(21, 1, "Resize or switch Panes after replacement; the final state must stay intact.")


def screens(dump):
    page("main and alternate screens", "Primary screen has a quadrant image; alternate screen should start empty.")
    at(5, 2)
    transmit(8901, p=1, c=12, r=6)
    if not dump:
        at(21, 1)
        input("Press Enter to enter the alternate screen.")
    emit(f"{ESC}[?1049h{ESC}[2J{ESC}[H")
    try:
        at(1, 1, "Alternate screen: only a blue image should appear here.")
        at(5, 2)
        transmit(8902, pixels(COLORS[2]), p=1, c=12, r=6)
        if not dump:
            at(21, 1)
            input("Press Enter to restore the primary screen.")
    finally:
        command(a="d", d="I", i=8902)
        emit(f"{ESC}[?1049l")
    at(21, 1, "Primary quadrant image restored; alternate blue image must be absent.")


SCENARIOS = {
    "basics": basics,
    "placeholders": placeholders,
    "relative": relative,
    "animation": animation,
    "lifecycle": lifecycle,
    "screens": screens,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", nargs="?", choices=("all", *SCENARIOS), default="all")
    parser.add_argument("--dump", action="store_true", help="Emit a synthetic escape-stream fixture without prompts or delays")
    args = parser.parse_args()
    if not args.dump and not sys.stdout.isatty():
        parser.error("Run inside a terminal Pane, or use --dump to capture a fixture")
    if not args.dump and (args.scenario in ("all", "screens")) and not sys.stdin.isatty():
        parser.error("This scenario needs interactive input; use a named case or --dump")
    chosen = SCENARIOS.items() if args.scenario == "all" else ((args.scenario, SCENARIOS[args.scenario]),)
    try:
        for index, (_, show) in enumerate(chosen):
            if index and not args.dump:
                at(24, 1)
                if input("Press Enter for the next page, or q to finish: ").strip().lower() == "q":
                    break
            show(args.dump)
    except (EOFError, KeyboardInterrupt):
        pass
    finally:
        emit(f"{ESC}[0m")
        at(24, 1, "\r\n")


if __name__ == "__main__":
    main()
