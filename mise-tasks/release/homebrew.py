#!/usr/bin/env python
# MISE description="Write the Homebrew cask for the published release's assembled disk image"
# USAGE arg "<tag>"
# USAGE arg "<tap>" help="Checkout of sadiksaifi/homebrew-tap"
import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release_homebrew import write

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("tag")
    parser.add_argument("tap")
    args = parser.parse_args()
    main(lambda: write(args.tag, args.tap))
