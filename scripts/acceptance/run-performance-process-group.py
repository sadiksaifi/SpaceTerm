#!/usr/bin/env python3

"""Execute one command as the leader of a new process group."""

import os
import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: run-performance-process-group.py COMMAND [ARG ...]", file=sys.stderr)
        return 64
    os.setpgid(0, 0)
    os.execvp(sys.argv[1], sys.argv[1:])
    return 70


if __name__ == "__main__":
    raise SystemExit(main())
