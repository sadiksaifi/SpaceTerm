#!/usr/bin/env python
# MISE description="Decide the compatibility workflow from its plan and pair results"
"""Pass only when every planned compatibility pair passed, or none was planned."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.compatibility import verdict

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("planning", help="The plan job's result")
    parser.add_argument("planned", help="The plan's any output: true or false")
    parser.add_argument("pairs", help="The pair job's result")
    args = parser.parse_args()
    main(lambda: verdict(args.planning, args.planned, args.pairs))
