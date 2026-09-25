#!/usr/bin/env python3
"""Run a paced synthetic workload in a dedicated terminal Pane."""

import argparse
import base64
import json
import math
import os
import struct
import sys
import time
import zlib


def bounded_number(minimum, maximum):
    def parse(value):
        number = float(value)
        if not math.isfinite(number) or not minimum <= number <= maximum:
            raise argparse.ArgumentTypeError(f"must be between {minimum} and {maximum}")
        return number

    return parse


def emit_image(write):
    def chunk(kind, data):
        return (
            struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data))
        )

    dimension = 2048
    raw_row = b"\x00" + bytes((32, 128, 224, 255)) * dimension
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", dimension, dimension, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw_row * dimension)) + chunk(b"IEND", b"")
    encoded = base64.b64encode(png)
    write(b"\x1b[2J\x1b[H\x1b[?2026h")
    try:
        for offset in range(0, len(encoded), 4096):
            part = encoded[offset:offset + 4096]
            more = int(offset + len(part) < len(encoded))
            control = (
                f"a=T,f=100,t=d,i=1,q=2,c=48,r=20,m={more}"
                if offset == 0 else f"m={more}"
            )
            write(b"\x1b_G" + control.encode("ascii") + b";" + part + b"\x1b\\")
        write(b"\r\nSynthetic 2048 x 2048 image; one image per Tab.\r\n")
    finally:
        write(b"\x1b[?2026l")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-l", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument(
        "--mode", choices=("idle", "partial", "scroll", "image"),
        default=os.environ.get("SPACETERM_BENCH_MODE", "scroll"),
    )
    parser.add_argument(
        "--duration", type=bounded_number(0.1, 3600),
        default=os.environ.get("SPACETERM_BENCH_DURATION", "60"),
    )
    parser.add_argument(
        "--rate", type=bounded_number(1, 240),
        default=os.environ.get("SPACETERM_BENCH_RATE", "60"),
        help="updates per second; each scroll update writes eight lines",
    )
    parser.add_argument("--smoke", action="store_true", help="allow redirected output, up to 5 seconds")
    args = parser.parse_args()
    if args.mode not in ("idle", "partial", "scroll", "image"):
        parser.error("SPACETERM_BENCH_MODE must be idle, partial, scroll, or image")
    if not sys.stdout.isatty() and not args.smoke:
        parser.error("stdout must be a terminal; use --smoke for a redirected check")
    if args.smoke and args.duration > 5:
        parser.error("--smoke requires --duration of at most 5 seconds")
    if args.mode == "image" and not args.smoke and args.duration <= 1:
        parser.error("image mode requires duration greater than its one-second startup delay")

    payload = ("resource workload abcdefghijklmnopqrstuvwxyz 0123456789 " * 2)[:95]
    emitted_bytes = 0
    emitted_lines = 0
    started = time.monotonic()

    def write(data):
        nonlocal emitted_bytes, emitted_lines
        encoded = data.encode("ascii") if isinstance(data, str) else data
        sys.stdout.buffer.write(encoded)
        sys.stdout.buffer.flush()
        emitted_bytes += len(encoded)
        emitted_lines += encoded.count(b"\n")

    frame = 0
    if args.mode != "image":
        write("\x1b[2J\x1b[H" + "".join(f"{row:04d} {payload}\r\n" for row in range(40)))
    if args.mode == "image":
        if not args.smoke:
            time.sleep(1)
        emit_image(write)
        frame = 1
        time.sleep(max(0, started + args.duration - time.monotonic()))
    elif args.mode == "idle":
        time.sleep(args.duration)
    else:
        deadline = time.monotonic() + args.duration
        next_tick = time.monotonic()
        while time.monotonic() < deadline:
            if args.mode == "partial":
                write(f"\x1b[3;1Hprogress {frame:08d} {payload}\x1b[K")
            else:
                write("".join(f"{frame:08d} {row:02d} {payload}\r\n" for row in range(8)))
            frame += 1
            # A blocked producer skips catch-up bursts instead of flooding the PTY.
            next_tick = max(next_tick + 1 / args.rate, time.monotonic())
            time.sleep(max(0, min(next_tick, deadline) - time.monotonic()))

    print(json.dumps({
        "event": "workload_complete",
        "mode": args.mode,
        "updates": frame,
        "emitted_lines": emitted_lines,
        "emitted_bytes": emitted_bytes,
        "elapsed_s": time.monotonic() - started,
    }), file=sys.stderr, flush=True)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except BrokenPipeError:
        sys.exit(1)
