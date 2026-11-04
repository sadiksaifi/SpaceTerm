#!/usr/bin/env python
# MISE description="Build and sign SpaceTerm Core once for this host's release target"
# MISE depends=["setup:linux-toolchain"]
# USAGE arg "<tag>" help="Annotated stable release tag at HEAD, for example v1.2.3"
# USAGE flag "--target <target>" help="Cross-build this target; only a dry run on a Mac does"
"""Build and sign SpaceTerm Core once for this host's release target.

macOS signs with Developer ID under the hardened runtime, notarizes and staples, then signs
the archive with the update key. Linux signs the archive with the update key. The signed
build lands in dist/release-core/<target>.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import core

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--target")
    args = parser.parse_args()
    main(lambda: core(args.tag, args.target))
