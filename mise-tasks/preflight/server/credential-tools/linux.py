#!/usr/bin/env python
# MISE description="Build the frozen systemd-creds closure in an isolated Jammy compiler image"
"""Construct artifacts without Host-key, TPM, custody, or native service mutation."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_server_linux import build_credential_tool

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(build_credential_tool)
