"""xoq_cv2 — drop-in cv2 replacement with remote camera support.

Re-exports the Rust extension (VideoCapture + constants) and delegates
everything else to the real opencv-python ``cv2`` package.
"""

from .xoq_cv2 import *  # noqa: F401,F403 — VideoCapture, constants

def __getattr__(name):
    """Delegate unknown attributes to real opencv-python."""
    import cv2 as _real_cv2
    return getattr(_real_cv2, name)

# Auto-install .pth hook if missing (handles maturin develop)
def _ensure_pth_hook():
    import site, os
    # Name sorts after xoq_opencv.pth (which adds the path for editable installs)
    pth_name = "xoq_opencv_hook.pth"
    for sp in site.getsitepackages():
        if os.path.exists(os.path.join(sp, pth_name)):
            return
    for sp in site.getsitepackages():
        try:
            with open(os.path.join(sp, pth_name), "w") as f:
                f.write("import xoq_cv2._cv2_hook as _h; _h.install()\n")
            return
        except OSError:
            continue

_ensure_pth_hook()
