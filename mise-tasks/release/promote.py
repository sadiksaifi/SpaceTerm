#!/usr/bin/env python
# MISE description="Verify an immutable release and prepare its Homebrew cask"
# USAGE arg "<tag>"
# USAGE arg "<tap>"
import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release_promote import prepare

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("tag")
    parser.add_argument("tap")
    args = parser.parse_args()
    main(lambda: prepare(args.tag, args.tap))
