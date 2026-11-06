"""Build the macOS disk image whose Finder window shows the brand background.

dmgbuild writes the window's layout into the image's .DS_Store itself. Laying the window out
through Finder scripting needs a logged-in session and Automation consent, which release runners
do not have, so no packaging path depends on Finder.
"""

import json
import sys
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, brand, venv
from spaceterm_tasks.packaging import checked

REQUIREMENTS = ROOT / "mise-tasks" / "disk-image" / "requirements.txt"
BACKGROUND = ROOT / brand.BRAND / "macos" / "dmg" / "background.tiff"
# dmgbuild stores the background as this hidden file at the root of the volume.
BACKGROUND_IN_IMAGE = ".background.tiff"


def build(app, image, volume_icon, environment):
    """Write `image` holding `app` and an Applications link, laid out over the background.

    dmgbuild and its installer run with `environment` alone, so release credentials that the
    caller removed never reach them.
    """
    app = Path(app)
    settings = {
        "format": "ULFO",
        "files": [str(app)],
        "symlinks": {"Applications": "/Applications"},
        "icon": str(volume_icon),
        "background": str(BACKGROUND),
        "window_rect": ((200, 120), brand.DMG_WINDOW),
        "icon_size": brand.DMG_ICON_SIZE,
        "icon_locations": {app.name: brand.DMG_APP, "Applications": brand.DMG_APPLICATIONS},
        "show_status_bar": False,
        "show_tab_view": False,
        "show_toolbar": False,
        "show_pathbar": False,
        "show_sidebar": False,
    }
    request = {"image": str(image), "volume_name": app.stem, "settings": settings}
    checked(
        [
            venv.python(REQUIREMENTS, environment),
            "-m",
            "spaceterm_tasks.disk_image",
            json.dumps(request),
        ],
        "dmgbuild could not create the disk image",
        env={**environment, "PYTHONPATH": str(ROOT / "mise-tasks" / "lib")},
    )
    if not Path(image).is_file():
        raise TaskError("dmgbuild did not produce the disk image")


def _run(request):
    """Run inside the dmgbuild environment."""
    from dmgbuild import build_dmg

    build_dmg(request["image"], request["volume_name"], settings=request["settings"])


if __name__ == "__main__":
    _run(json.loads(sys.argv[1]))
