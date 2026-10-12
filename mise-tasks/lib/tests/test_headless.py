"""Check the headless boundary using Cargo's resolved package graph."""

import subprocess
import unittest
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks.headless import check_metadata, check_workspace, forbidden_package

MANDATORY = (
    "spaceterm-core",
    "spaceterm-client",
    "spaceterm-server",
    "spaceterm-host",
    "spaceterm-protocol",
    "spaceterm-terminal",
)


def metadata(edges=None, *, roots=MANDATORY):
    edges = {**dict.fromkeys(roots, ()), **(edges or {})}
    names = set(edges)
    for dependencies in edges.values():
        names.update(dependencies)
    # IDs deliberately differ from names. Cargo aliases and package sources must not
    # determine classification or appear in a diagnostic.
    ids = {name: f"path+file:///private/workspace/{name}#0.1.0" for name in names}
    return {
        "version": 1,
        "packages": [{"id": ids[name], "name": name} for name in sorted(names)],
        "workspace_members": [ids[name] for name in roots],
        "resolve": {
            "nodes": [
                {"id": ids[name], "dependencies": [ids[dep] for dep in edges.get(name, ())]}
                for name in sorted(names)
            ]
        },
    }


class HeadlessTests(unittest.TestCase):
    def test_transitive_ui_dependency_fails_with_package_chain_not_paths(self):
        graph = metadata(
            {
                "spaceterm-server": ("transport",),
                "transport": ("renderer",),
                "renderer": ("gpui_platform",),
                "spaceterm": ("wgpu",),
            }
        )
        with self.assertRaises(TaskError) as raised:
            check_metadata(graph)
        message = str(raised.exception)
        self.assertIn("spaceterm-server -> transport -> renderer -> gpui_platform", message)
        self.assertNotIn("/private", message)
        self.assertNotIn("wgpu", message)

    def test_desktop_only_dependencies_do_not_fail_headless_roots(self):
        graph = metadata({"spaceterm": ("gpui", "objc2", "wgpu")})
        self.assertEqual(check_metadata(graph), MANDATORY)

    def test_missing_required_root_fails_closed(self):
        with self.assertRaisesRegex(TaskError, "crates missing: spaceterm-terminal"):
            check_metadata(metadata(roots=MANDATORY[:-1]))

    def test_protocol_and_terminal_roots_are_checked(self):
        with self.assertRaisesRegex(TaskError, "spaceterm-terminal -> font-kit"):
            check_metadata(metadata({"spaceterm-terminal": ("font-kit",)}))
        with self.assertRaisesRegex(TaskError, "spaceterm-protocol -> gpui"):
            check_metadata(metadata({"spaceterm-protocol": ("gpui",)}))

    def test_cycle_in_a_portable_graph_terminates(self):
        graph = metadata({"spaceterm-core": ("portable",), "portable": ("spaceterm-core",)})
        self.assertEqual(check_metadata(graph), MANDATORY)

    def test_dependency_names_classify_desktop_families_without_prefix_collisions(self):
        denied = (
            "gpui",
            "gpui_platform",
            "objc2",
            "objc2-app-kit",
            "cocoa",
            "appkit",
            "core-foundation-sys",
            "font-kit",
            "yeslogic-fontconfig-sys",
            "freetype-sys",
            "rustybuzz",
            "ttf-parser",
            "wayland-client",
            "x11rb-protocol",
            "xcb",
            "xkbcommon-dl",
            "winit",
            "raw-window-handle",
            "wgpu-core",
            "metal",
            "blade-graphics",
            "ash",
            "naga",
            "glutin",
            "gl",
            "glfw",
            "sdl2",
            "gtk4",
            "ashpd",
            "arboard",
            "accesskit_unix",
            "tray-icon",
            "ksni",
        )
        for name in denied:
            with self.subTest(package=name):
                self.assertTrue(forbidden_package(name))
                with self.assertRaises(TaskError):
                    check_metadata(metadata({"spaceterm-host": (name,)}))
        for name in ("libc", "serde", "tokio", "zbus", "sha2", "wgpuish", "ashford", "x11ish"):
            with self.subTest(package=name):
                self.assertFalse(forbidden_package(name))

    def test_missing_resolution_or_dependency_node_fails_closed(self):
        graph = metadata({"spaceterm-core": ("portable",)})
        graph["resolve"]["nodes"] = [
            node for node in graph["resolve"]["nodes"] if "portable#" not in node["id"]
        ]
        with self.assertRaisesRegex(TaskError, "incomplete dependency graph"):
            check_metadata(graph)
        graph = metadata()
        graph["resolve"] = None
        with self.assertRaisesRegex(TaskError, "incomplete dependency graph"):
            check_metadata(graph)

    def test_untrusted_package_names_cannot_enter_diagnostics(self):
        graph = metadata()
        graph["packages"][0]["name"] = "bad\n/private/secret"
        with self.assertRaises(TaskError) as raised:
            check_metadata(graph)
        self.assertNotIn("secret", str(raised.exception))

    def test_cargo_resolution_failure_reports_only_classification(self):
        result = subprocess.CompletedProcess([], 1, stdout="", stderr="/private/secret")
        with patch("spaceterm_tasks.headless.subprocess.run", return_value=result) as run:
            with self.assertRaises(TaskError) as raised:
                check_workspace()
        self.assertNotIn("secret", str(raised.exception))
        self.assertIn("--all-features", run.call_args.args[0])
        self.assertNotIn("--filter-platform", run.call_args.args[0])

    def test_invalid_cargo_json_reports_only_classification(self):
        result = subprocess.CompletedProcess([], 0, stdout="/private/secret", stderr="")
        with patch("spaceterm_tasks.headless.subprocess.run", return_value=result):
            with self.assertRaisesRegex(TaskError, "invalid dependency graph"):
                check_workspace()


if __name__ == "__main__":
    unittest.main()
