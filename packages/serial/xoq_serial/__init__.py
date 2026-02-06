"""xoq_serial — drop-in serial replacement with remote serial port support.

Re-exports the Rust extension (Serial + constants) and delegates
everything else to the real pyserial ``serial`` package.
"""

from .xoq_serial import *  # noqa: F401,F403 — Serial, constants

def __getattr__(name):
    """Delegate unknown attributes to real pyserial."""
    import serial as _real_serial
    return getattr(_real_serial, name)

# Auto-install .pth hook if missing (handles maturin develop)
def _ensure_pth_hook():
    import site, os
    pth_name = "xoq_serial_hook.pth"
    for sp in site.getsitepackages():
        if os.path.exists(os.path.join(sp, pth_name)):
            return
    for sp in site.getsitepackages():
        try:
            with open(os.path.join(sp, pth_name), "w") as f:
                f.write("import xoq_serial._serial_hook as _h; _h.install()\n")
            return
        except OSError:
            continue

_ensure_pth_hook()
