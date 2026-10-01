#!/usr/bin/env python3
"""Sample one macOS process without privileges or terminal content collection."""

import argparse
import ctypes
import json
import math
import sys
import time


class Usage(ctypes.Structure):
    # sys/resource.h: rusage_info_v0, available since macOS 10.9.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64)
        for name in (
            "user system package_wakeups interrupt_wakeups pageins wired resident "
            "footprint start exit"
        ).split()
    ]


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


def bounded_number(minimum, maximum):
    def parse(value):
        number = float(value)
        if not math.isfinite(number) or not minimum <= number <= maximum:
            raise argparse.ArgumentTypeError(f"must be between {minimum} and {maximum}")
        return number

    return parse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int, help="PID of the application process to sample")
    parser.add_argument("--duration", type=bounded_number(0.1, 3600), default=20.0)
    parser.add_argument("--interval", type=bounded_number(0.05, 60), default=1.0)
    args = parser.parse_args()
    if not 0 < args.pid <= 2**31 - 1:
        parser.error("pid must be a positive 32-bit process identifier")
    if args.interval > args.duration:
        parser.error("interval must not exceed duration")
    if sys.platform != "darwin":
        parser.error("resource sampling requires macOS")

    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    libproc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    libproc.proc_pid_rusage.restype = ctypes.c_int
    system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    system.mach_timebase_info.argtypes = [ctypes.POINTER(Timebase)]
    system.mach_timebase_info.restype = ctypes.c_int
    timebase = Timebase()
    if system.mach_timebase_info(ctypes.byref(timebase)) != 0 or not timebase.denom:
        parser.exit(1, "could not read the Mach clock timebase\n")
    nanoseconds_per_tick = timebase.numer / timebase.denom

    def read():
        usage = Usage()
        if libproc.proc_pid_rusage(args.pid, 0, ctypes.byref(usage)) != 0:
            parser.exit(1, f"cannot sample process: OS error {ctypes.get_errno()}\n")
        return time.monotonic(), usage

    previous_time, previous = read()
    started = previous_time
    deadline = started + args.duration
    index = 0
    while previous_time < deadline:
        time.sleep(min(args.interval, max(0, deadline - time.monotonic())))
        current_time, current = read()
        if current.start != previous.start:
            parser.exit(1, "process identifier was reused; sampling stopped\n")
        elapsed = current_time - previous_time
        index += 1
        cpu_ticks = current.user + current.system - previous.user - previous.system
        print(json.dumps({
            "sample": index,
            "pid": args.pid,
            "elapsed_s": current_time - started,
            "interval_s": elapsed,
            "cpu_percent": cpu_ticks * nanoseconds_per_tick / elapsed / 1e7,
            "nanoseconds_per_tick": nanoseconds_per_tick,
            "footprint_mib": current.footprint / 1048576,
            "resident_mib": current.resident / 1048576,
            "interrupt_wakeups_s": (
                current.interrupt_wakeups - previous.interrupt_wakeups
            ) / elapsed,
            "package_wakeups_s": (
                current.package_wakeups - previous.package_wakeups
            ) / elapsed,
        }), flush=True)
        previous_time, previous = current_time, current


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
