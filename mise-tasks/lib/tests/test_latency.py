"""Check the bench:latency reporter and route guard parsing."""

import argparse
import fcntl
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from spaceterm_tasks import TaskError, latency

ROUTE = """   route to: 192.168.1.206
destination: 192.168.1.206
  interface: en8
      flags: <UP,HOST,DONE,LLINFO,WASCLONED,IFSCOPE,IFREF>
"""
IFCONFIG = """en8: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet6 fe80::1c2b:3d4e:5f60:7182%en8 prefixlen 64 secured scopeid 0x13
\tinet 192.168.1.36 netmask 0xffffff00 broadcast 192.168.1.255
"""
LSOF = """p98367
f4
n192.168.1.36:54413->192.168.1.206:9231
f17
n192.168.1.36:54413->192.168.1.206:9231
p100
f3
n[fe80::1]:50000->[fe80::2]:22
"""


class PercentileTests(unittest.TestCase):
    def test_interpolates_between_neighbors(self):
        values = [4.0, 1.0, 3.0, 2.0]
        self.assertEqual(latency.percentile(values, 0), 1.0)
        self.assertEqual(latency.percentile(values, 50), 2.5)
        self.assertAlmostEqual(latency.percentile(values, 90), 3.7)
        self.assertEqual(latency.percentile(values, 100), 4.0)

    def test_summary_reports_misses_and_empty_runs(self):
        self.assertEqual(
            latency.summary("echo", [1.0, 3.0], misses=1),
            "echo: n=2 misses=1 p50=2.00 p90=2.80 p99=2.98 max=3.00 min=1.00 mean=2.00 ms",
        )
        self.assertEqual(latency.summary("echo", [], misses=3), "echo: n=0 misses=3")


class RouteGuardParsingTests(unittest.TestCase):
    def test_reads_the_route_interface(self):
        self.assertEqual(latency.parse_route_interface(ROUTE), "en8")
        self.assertIsNone(latency.parse_route_interface("route: writing to routing socket"))

    def test_reads_interface_addresses_without_scope(self):
        self.assertEqual(
            latency.parse_interface_addresses(IFCONFIG),
            {"192.168.1.36", "fe80::1c2b:3d4e:5f60:7182"},
        )

    def test_counts_each_connection_once(self):
        self.assertEqual(latency.parse_local_addresses(LSOF), ["192.168.1.36", "fe80::1"])


class RouteGuardPlatformTests(unittest.TestCase):
    def test_refuses_an_interface_outside_macos(self):
        args = argparse.Namespace(interface="eth0", route_host="192.168.1.206")
        with mock.patch.object(latency.sys, "platform", "linux"):
            with self.assertRaisesRegex(TaskError, "needs macOS"):
                latency.check_route(args)

    def test_skips_the_guard_without_an_interface(self):
        with mock.patch.object(latency, "guard_route") as guard:
            latency.check_route(argparse.Namespace(interface=None, route_host="host"))
        guard.assert_not_called()


class ScreenLockTests(unittest.TestCase):
    def test_excludes_other_holders_until_released(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "screen.flock"
            with mock.patch.object(latency, "SCREEN_LOCK", path):
                other = os.open(path, os.O_RDWR | os.O_CREAT)
                try:
                    with latency.screen_lock():
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    os.close(other)


class ShellDirectoryTests(unittest.TestCase):
    def test_keeps_the_home_prefix_unquoted(self):
        self.assertEqual(
            latency.shell_directory("~/Projects/Space Term"), "~/'Projects/Space Term'"
        )
        self.assertEqual(latency.shell_directory("/srv/space term"), "'/srv/space term'")

    def test_defaults_to_this_checkout_from_home(self):
        with mock.patch.object(latency, "ROOT", Path.home() / "Projects" / "SpaceTerm"):
            self.assertEqual(latency.shell_directory(), "~/Projects/SpaceTerm")


if __name__ == "__main__":
    unittest.main()


class NetemTests(unittest.TestCase):
    def test_impairs_only_traffic_to_the_client(self):
        commands = latency.netem_commands("eno1", "192.168.1.36", 2, 25)
        self.assertEqual(
            commands[0],
            "sudo -n tc qdisc add dev eno1 root handle 1: prio bands 4 "
            "priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1",
        )
        self.assertIn("parent 1:4 handle 40: netem loss 2% delay 25ms", commands[1])
        self.assertTrue(commands[2].endswith("match ip dst 192.168.1.36/32 flowid 1:4"))

    def test_sees_a_qdisc_a_run_would_replace(self):
        clean = "qdisc fq_codel 0: root refcnt 2 limit 10240p flows 1024\n"
        impaired = (
            "qdisc prio 1: root refcnt 2 bands 4 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1\n"
            "qdisc netem 40: parent 1:4 limit 1000 delay 25ms loss 2%\n"
        )
        self.assertFalse(latency.netem_active(clean))
        self.assertTrue(latency.netem_active(impaired))

    def test_tables_each_cell_across_rounds(self):
        results = [
            {"measurement": "key to echo", "condition": "clean", "samples": [1.0, 2.0]},
            {"measurement": "key to echo", "condition": "clean", "samples": [3.0]},
            {"measurement": "key to echo", "condition": "impaired", "samples": [30.0]},
        ]
        rows = latency.table(results).splitlines()
        self.assertEqual(len(rows), 4)
        self.assertEqual(rows[2], "| key to echo | clean | 3 | 2.00 | 2.80 | 2.98 | 3.00 |")
        self.assertIn("| impaired | 1 |", rows[3])
