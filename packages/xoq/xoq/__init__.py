"""XOQ - Remote peripherals over P2P.

Sub-packages are lazy-imported to avoid triggering side effects
(like _ensure_pth_hook) for packages that aren't installed.
"""

__version__ = "0.3.4"
__all__ = ["can", "cv2", "serial"]

_SUBPACKAGE_MAP = {
    "can": "xoq_can",
    "cv2": "xoq_cv2",
    "serial": "xoq_serial",
}


def __getattr__(name):
    if name in _SUBPACKAGE_MAP:
        try:
            import importlib
            mod = importlib.import_module(_SUBPACKAGE_MAP[name])
        except ImportError:
            mod = None
        globals()[name] = mod
        return mod
    raise AttributeError(f"module 'xoq' has no attribute {name!r}")
