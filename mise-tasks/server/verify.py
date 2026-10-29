#!/usr/bin/env python
# MISE description="Authenticate and verify an exact server candidate before running its executable"
"""Authenticate and verify an exact server candidate before running its executable."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.server_package import IDENTITIES, PROTOCOL_MAJOR, TARGETS, install, verify

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive")
    parser.add_argument("--signature", required=True)
    parser.add_argument("--identity", choices=IDENTITIES, required=True)
    parser.add_argument("--target", choices=TARGETS.values(), required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument(
        "--source-sha256", help="Signed source digest printed by server:package or server:catalog"
    )
    parser.add_argument(
        "--retain",
        action="store_true",
        help="Retain a verified runtime without activating or registering a service",
    )
    args = parser.parse_args()
    expected = {
        "schema": 1,
        "source_sha256": args.source_sha256,
        "identity": args.identity,
        "target": args.target,
        "commit": args.commit,
        "version": args.version,
        "protocol_major": PROTOCOL_MAJOR,
        "capabilities": ["workspace_policy"],
    }
    operation = install if args.retain else verify
    main(lambda: operation(args.archive, args.signature, expected) and None)
