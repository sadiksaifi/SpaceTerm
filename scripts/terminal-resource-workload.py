#!/usr/bin/env python3
"""Run a paced synthetic workload in a dedicated terminal Pane."""

import argparse
import base64
import json
import math
import os
import re
import select
import struct
import sys
import time
import zlib


class CursorReportParser:
    """Retain only a bounded partial standard DSR cursor-position response."""

    _report = re.compile(rb"\x1b\[([0-9]{1,5});([0-9]{1,5})R")
    MAX_BYTES = 14

    def __init__(self):
        self.pending = bytearray()

    def feed(self, data):
        for byte in data:
            if byte == 0x1B:
                self.pending.clear()
                self.pending.append(byte)
                continue
            if not self.pending:
                continue
            self.pending.append(byte)
            if len(self.pending) > self.MAX_BYTES:
                self.pending.clear()
                continue
            if byte == ord("R"):
                match = self._report.fullmatch(self.pending)
                coordinates = match.groups() if match else None
                self.pending.clear()
                if coordinates:
                    row, column = (int(value) for value in coordinates)
                    if 1 <= row <= 65535 and 1 <= column <= 65535:
                        return row, column
        return None


def acknowledge_consumption(smoke, timeout=1.5):
    """Send a fixed DSR after output, keeping protocol bytes outside fixture counts."""
    result = {"received": False, "skipped_smoke": bool(smoke), "cursor_row": 0,
              "cursor_column": 0, "query_bytes": 0, "elapsed_ms": 0.0}
    if smoke:
        return result
    started = time.monotonic()
    try:
        import termios
        import tty
    except ImportError:
        return result
    try:
        input_fd, output_fd = sys.stdin.fileno(), sys.stdout.fileno()
        terminal_input = os.isatty(input_fd) and os.isatty(output_fd)
    except (OSError, ValueError, AttributeError):
        return result
    if not terminal_input:
        return result
    attributes = None
    output_blocking = None
    try:
        attributes = termios.tcgetattr(input_fd)
        tty.setraw(input_fd, when=termios.TCSANOW)
        output_blocking = os.get_blocking(output_fd)
        os.set_blocking(output_fd, False)
        deadline = started + timeout
        query = b"\x1b[6n"
        while result["query_bytes"] < len(query):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([], [output_fd], [], remaining)[1]:
                break
            try:
                written = os.write(output_fd, query[result["query_bytes"]:])
            except (BlockingIOError, InterruptedError):
                continue
            if written <= 0:
                break
            result["query_bytes"] += written
        parser = CursorReportParser()
        remaining_bytes = 4096
        while result["query_bytes"] == len(query) and remaining_bytes:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([input_fd], [], [], remaining)[0]:
                break
            try:
                data = os.read(input_fd, min(256, remaining_bytes))
            except (BlockingIOError, InterruptedError):
                continue
            if not data:
                break
            remaining_bytes -= len(data)
            position = parser.feed(data)
            if position is not None:
                result["received"] = True
                result["cursor_row"], result["cursor_column"] = position
                break
    except (OSError, ValueError, termios.error):
        result["received"] = False
    finally:
        if output_blocking is not None:
            try:
                os.set_blocking(output_fd, output_blocking)
            except OSError:
                result["received"] = False
        if attributes is not None:
            try:
                termios.tcsetattr(input_fd, termios.TCSANOW, attributes)
            except (OSError, termios.error):
                result["received"] = False
        result["elapsed_ms"] = (time.monotonic() - started) * 1000
    if not result["received"]:
        result["cursor_row"] = result["cursor_column"] = 0
    return result


def write_benchmark_file(path, value):
    if not path:
        return
    temporary = f"{path}.{os.getpid()}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(value, output)
            output.write("\n")
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


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
        "--mode", choices=("idle", "partial", "scroll", "history", "image"),
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
    parser.add_argument("--hold-seconds", type=bounded_number(0, 60),
                        default=os.environ.get("SPACETERM_BENCH_HOLD_SECONDS", "0"),
                        help="Keep the synthetic shell alive after reporting completed output")
    args = parser.parse_args()
    if args.mode not in ("idle", "partial", "scroll", "history", "image"):
        parser.error("SPACETERM_BENCH_MODE must be idle, partial, scroll, history, or image")
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
    write_benchmark_file(os.environ.get("SPACETERM_BENCH_PID_FILE"), {"pid": os.getpid()})

    def write(data):
        nonlocal emitted_bytes, emitted_lines
        encoded = data.encode("ascii") if isinstance(data, str) else data
        sys.stdout.buffer.write(encoded)
        sys.stdout.buffer.flush()
        emitted_bytes += len(encoded)
        emitted_lines += encoded.count(b"\n")

    frame = 0
    if args.mode not in ("image", "history"):
        write("\x1b[2J\x1b[H" + "".join(f"{row:04d} {payload}\r\n" for row in range(40)))
    if args.mode == "image":
        if not args.smoke:
            time.sleep(1)
        emit_image(write)
        frame = 1
        time.sleep(max(0, started + args.duration - time.monotonic()))
    elif args.mode == "idle":
        time.sleep(args.duration)
    elif args.mode == "history":
        for first in range(0, 10000, 100):
            write("".join(f"{row:08d} {payload}\r\n" for row in range(first, first + 100)))
        frame = 10000
        write_benchmark_file(os.environ.get("SPACETERM_BENCH_READY_FILE"), {
            "event": "history_ready", "emitted_lines": emitted_lines,
        })
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

    output_elapsed = time.monotonic() - started
    acknowledgment = (acknowledge_consumption(args.smoke)
                      if os.environ.get("SPACETERM_BENCH_ACK") == "1" else None)
    summary = {
        "event": "workload_complete",
        "mode": args.mode,
        "updates": frame,
        "emitted_lines": emitted_lines,
        "emitted_bytes": emitted_bytes,
        "elapsed_s": output_elapsed,
        "grid": dict(zip(("columns", "rows"), os.get_terminal_size()))
        if sys.stdout.isatty() else None,
    }
    if acknowledgment is not None:
        summary["consumption_ack"] = acknowledgment
    write_benchmark_file(os.environ.get("SPACETERM_BENCH_SUMMARY_FILE"), summary)
    print(json.dumps(summary), file=sys.stderr, flush=True)
    if args.hold_seconds:
        time.sleep(args.hold_seconds)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except BrokenPipeError:
        sys.exit(1)
