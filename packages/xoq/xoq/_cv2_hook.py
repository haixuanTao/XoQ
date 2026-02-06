"""Startup hook that patches ``import cv2`` to include xoq remote cameras.

Installed via ``_xoq_cv2.pth`` so it runs automatically at interpreter
startup.  After the hook fires once it removes itself from
``sys.meta_path`` — zero overhead for subsequent imports.
"""

import importlib
import importlib.abc
import importlib.machinery
import importlib.util
import os
import re
import sys


# Pattern for iroh node IDs (64-char hex-encoded ed25519 public keys)
_IROH_ID_RE = re.compile(r"^[a-f0-9]{64}$")


def _is_remote_source(source):
    """Return True if *source* looks like a remote camera identifier."""
    if not isinstance(source, str):
        return False
    # iroh node ID — 64-char hex-encoded ed25519 public key
    if _IROH_ID_RE.match(source):
        return True
    return False


class _XoqVideoCapture:
    """Wrapper that routes to xoq or real OpenCV based on the source arg."""

    def __init__(self, real_cls, xoq_cls):
        self._real = real_cls
        self._xoq = xoq_cls

    def __call__(self, *args, **kwargs):
        transport = kwargs.pop("transport", None)

        # Explicit transport → always use xoq
        if transport is not None:
            source = args[0] if args else kwargs.get("source", "")
            print(f"[xoq] Connected to remote camera: {source} (transport={transport})")
            return self._xoq(source, transport)

        source = args[0] if args else kwargs.get("index", kwargs.get("filename", None))

        if _is_remote_source(source):
            print(f"[xoq] Connected to remote camera: {source}")
            return self._xoq(source)

        # Fall through to real OpenCV
        return self._real(*args, **kwargs)

    def __repr__(self):
        return f"<xoq-patched VideoCapture: xoq={self._xoq!r}, cv2={self._real!r}>"

    def __instancecheck__(cls, instance):
        return isinstance(instance, (cls._real, cls._xoq))


def _patch_cv2(mod):
    """Patch cv2.VideoCapture with xoq-aware wrapper."""
    try:
        import xoq_cv2 as _xoq
        if not isinstance(mod.VideoCapture, _XoqVideoCapture):
            mod.VideoCapture = _XoqVideoCapture(mod.VideoCapture, _xoq.VideoCapture)
    except ImportError:
        pass


class _Cv2Finder(importlib.abc.MetaPathFinder):
    """One-shot meta-path finder that intercepts ``import cv2``."""

    def find_spec(self, fullname, path, target=None):
        if fullname != "cv2":
            return None

        # Remove ourselves to avoid recursion
        sys.meta_path[:] = [f for f in sys.meta_path if f is not self]

        # Let the real cv2 import happen normally
        spec = importlib.util.find_spec("cv2")
        if spec is None:
            return None

        # Wrap the loader to patch after loading
        original_loader = spec.loader
        spec.loader = _PatchingLoader(original_loader)
        return spec


class _PatchingLoader:
    """Loader wrapper that patches cv2 after the real loader finishes."""

    def __init__(self, original):
        self._original = original

    def create_module(self, spec):
        if hasattr(self._original, "create_module"):
            return self._original.create_module(spec)
        return None

    def exec_module(self, module):
        self._original.exec_module(module)
        _patch_cv2(module)


def install():
    """Insert the cv2 import hook (idempotent, guards against re-entry)."""
    # Already imported — patch in place
    if "cv2" in sys.modules:
        _patch_cv2(sys.modules["cv2"])
        return

    # xoq_cv2 not available — nothing to do
    try:
        import xoq_cv2  # noqa: F401
    except ImportError:
        return

    # Don't double-install
    if any(isinstance(f, _Cv2Finder) for f in sys.meta_path):
        return

    sys.meta_path.insert(0, _Cv2Finder())
