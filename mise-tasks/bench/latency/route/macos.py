#!/usr/bin/env python
# MISE description="Refuse a remote batch unless the route to the remote host uses the expected interface"
# USAGE arg "<host>" help="Remote address or name"
# USAGE arg "<interface>" help="Expected interface, such as en8"
"""Refuse a remote batch unless the route to the remote host uses the expected interface.

Established TCP connections to the host, such as an SSH session, must also
use a local address on that interface.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.latency import guard_route

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("interface")
    arguments = parser.parse_args()
    main(lambda: guard_route(arguments.host, arguments.interface))
