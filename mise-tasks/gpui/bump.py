#!/usr/bin/env python
# MISE description="Bump the pinned GPUI fork tag and matching Rust toolchain"
# USAGE arg "<tag>" help="Published SpaceTerm Zed fork tag, for example spaceterm-2026-10-05"
"""Bump the pinned GPUI fork tag and matching Rust toolchain."""

import sys

from spaceterm_tasks.gpui_bump import BumpCancelled, main

if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except (KeyboardInterrupt, BumpCancelled):
        sys.stderr.write("gpui:bump: cancelled\n")
        raise SystemExit(130) from None
    except BaseException:
        sys.stderr.write("gpui:bump: failed\n")
        raise SystemExit(1) from None
