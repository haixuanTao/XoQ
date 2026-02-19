"""xoq_realsense — drop-in pyrealsense2 replacement with remote camera support.

Re-exports the Rust extension (pipeline, config, stream, format, etc.)
and delegates everything else to the real pyrealsense2 package.
"""

from .xoq_realsense import *  # noqa: F401,F403


def __getattr__(name):
    """Delegate unknown attributes to real pyrealsense2."""
    try:
        import pyrealsense2 as _real_rs2
        return getattr(_real_rs2, name)
    except (ImportError, AttributeError):
        raise AttributeError(f"module 'xoq_realsense' has no attribute '{name}'")


# Auto-install .pth hook if missing (handles maturin develop)
def _ensure_pth_hook():
    import site, os
    pth_name = "xoq_realsense_hook.pth"
    for sp in site.getsitepackages():
        if os.path.exists(os.path.join(sp, pth_name)):
            return
    for sp in site.getsitepackages():
        try:
            with open(os.path.join(sp, pth_name), "w") as f:
                f.write("import xoq_realsense._rs_hook as _h; _h.install()\n")
            return
        except OSError:
            continue


_ensure_pth_hook()
