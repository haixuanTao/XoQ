"""xoq_can — drop-in can replacement with remote CAN bus support.

Re-exports the Rust extension (Bus, Message + constants) and delegates
everything else to the real python-can ``can`` package.
"""

from .xoq_can import *  # noqa: F401,F403 — Bus, Message, interface, constants

def __getattr__(name):
    """Delegate unknown attributes to real python-can."""
    import can as _real_can
    return getattr(_real_can, name)

# Auto-install .pth hook if missing (handles maturin develop)
def _ensure_pth_hook():
    import site, os
    pth_name = "xoq_can_hook.pth"
    for sp in site.getsitepackages():
        if os.path.exists(os.path.join(sp, pth_name)):
            return
    for sp in site.getsitepackages():
        try:
            with open(os.path.join(sp, pth_name), "w") as f:
                f.write("import xoq_can._can_hook as _h; _h.install()\n")
            return
        except OSError:
            continue

_ensure_pth_hook()
