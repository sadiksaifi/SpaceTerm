#!/usr/bin/env python
# MISE description="Check a release version against the Tagsmith policy, or validate its pushed tag"
# USAGE arg "<version>"
import argparse

from spaceterm_tasks import main, release_plan

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    args = parser.parse_args()
    main(lambda: release_plan.check_policy(args.version))
