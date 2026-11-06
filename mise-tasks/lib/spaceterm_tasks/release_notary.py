"""Restore submitted signed bytes before resuming a release notarization receipt."""

import os
import shutil
import tempfile
from pathlib import Path

from spaceterm_tasks import macos_notary, macos_signing, release_plan, update_signing
from spaceterm_tasks.packaging import DIST, checked


def notarize(path, notary, environment=None):
    path = Path(path)
    environment = macos_signing.signing_environment(
        os.environ if environment is None else environment
    )
    kwargs = {"environment": environment}
    # Local package commands still use the resumable notary module without a workflow plan.
    plan = release_plan.PLAN
    if not plan.is_file():
        if environment is None:
            return macos_notary.notarize(path, notary)
        return macos_notary.notarize(path, notary, environment)
    release_plan.load()
    directory = DIST / "notarization" / path.name
    if (directory / "receipt.json").is_file():
        # Re-signing changes timestamps. Restore the original signed input before the
        # notary module checks its source, plan, input and submitted-payload bindings.
        if path.suffix == ".app":
            with tempfile.TemporaryDirectory(dir=DIST, prefix=".resume-notary-") as temporary:
                checked(
                    ["ditto", "-x", "-k", str(directory / "submission.zip"), temporary],
                    "the submitted signed app could not be restored",
                    env=environment,
                )
                shutil.rmtree(path)
                shutil.copytree(Path(temporary) / path.name, path, symlinks=True)
        else:
            shutil.copyfile(directory / "submission.dmg", path)
    macos_notary.notarize(
        path,
        notary,
        **kwargs,
        artifact_directory=directory,
        plan_sha256=update_signing.sha256(plan),
    )
