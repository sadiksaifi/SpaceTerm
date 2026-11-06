"""Exercise the final Linux Core bytes, including first-release activation."""

import json
import shutil
import tempfile
from pathlib import Path

from spaceterm_tasks import compatibility, release, release_plan, update_signing
from spaceterm_tasks.core_package import TARGETS, verified_copy


def run():
    plan = release_plan.load()
    entry = release.CORE / TARGETS["linux"]
    archive, signature = entry / "artifact.tar.gz", entry / "artifact.sig"
    if plan["floors"]["core"] != plan["version"]:
        compatibility.update(plan["tag"], archive.resolve(), signature.resolve())
        kind = "update-from-floor"
    else:
        with tempfile.TemporaryDirectory(prefix="spaceterm-first-release-") as temporary:
            directory = Path(temporary)
            tree, private, raw = verified_copy(
                archive, signature, directory, None, identity="spaceterm", target=TARGETS["linux"]
            )
            shutil.copyfile(private, tree / "artifact.tar.gz")
            shutil.copyfile(raw, tree / "artifact.sig")
            environment = compatibility._environment(directory / "account")
            compatibility.install(tree, environment, ["--network", "127.0.0.1"])
            compatibility.exercise(
                compatibility.LAUNCHER, environment, {"list": "ok", "status": "ok"}
            )
            status = compatibility._envelope(compatibility.LAUNCHER, ["status"], environment)
            if status["data"]["runtime"]["version"] != plan["version"]:
                from spaceterm_tasks import TaskError

                raise TaskError("first-release activation did not run the candidate version")
        kind = "first-release-activation"
    (release.DIST / "floor-smoke.json").write_text(
        json.dumps(
            {
                "kind": kind,
                "source": plan["source"],
                "tag": plan["tag"],
                "floor": plan["floors"]["core"],
                "sha256": update_signing.sha256(archive),
            }
        )
        + "\n"
    )
