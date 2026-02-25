"""Startup hook that patches ``import pyrealsense2`` to include xoq remote cameras.

Installed via ``.pth`` file so it runs automatically at interpreter startup.
After the hook fires once it removes itself from ``sys.meta_path`` — zero
overhead for subsequent imports.

When pyrealsense2 is installed, ``import pyrealsense2`` loads the real package
and patches pipeline/config with dispatcher classes. When pyrealsense2 is NOT
installed, ``import pyrealsense2`` creates a synthetic module backed by
xoq_realsense.
"""

import importlib
import importlib.abc
import importlib.machinery
import importlib.util
import os
import re
import sys
import types
from concurrent.futures import ThreadPoolExecutor

_XOQ_TIMEOUT = float(os.environ.get("XOQ_REALSENSE_TIMEOUT", "15"))


def _is_remote_serial(serial):
    """Return True if *serial* looks like a MoQ path (contains /)."""
    if serial is None:
        return False
    s = str(serial)
    return "/" in s


class _MissingPyRealSense2:
    """Placeholder when pyrealsense2 is not installed."""

    def __init__(self, *args, **kwargs):
        raise ImportError(
            "pyrealsense2 is not installed. Install it with: pip install pyrealsense2\n"
            "Only remote cameras (MoQ paths like 'anon/realsense') work without pyrealsense2."
        )


class _XoqConfig:
    """Config wrapper that dispatches to xoq or real pyrealsense2."""

    _real_cls = _MissingPyRealSense2
    _xoq_cls = None
    _remote = False

    def __init__(self):
        self._remote = False
        self._real = None
        self._xoq = None
        # Always create both if available
        try:
            self._real = self._real_cls()
        except (ImportError, Exception):
            pass
        if self._xoq_cls is not None:
            self._xoq = self._xoq_cls()

    def enable_device(self, serial):
        if _is_remote_serial(serial):
            self._remote = True
            if self._xoq is not None:
                self._xoq.enable_device(serial)
        else:
            self._remote = False
            if self._real is not None:
                self._real.enable_device(serial)
            elif self._xoq is not None:
                # No real pyrealsense2, try xoq anyway
                self._xoq.enable_device(serial)

    def enable_stream(self, *args, **kwargs):
        if self._remote and self._xoq is not None:
            self._xoq.enable_stream(*args, **kwargs)
        elif self._real is not None:
            self._real.enable_stream(*args, **kwargs)
        elif self._xoq is not None:
            self._xoq.enable_stream(*args, **kwargs)

    @property
    def _active(self):
        if self._remote and self._xoq is not None:
            return self._xoq
        if self._real is not None:
            return self._real
        return self._xoq


class _XoqPipeline:
    """Pipeline wrapper that dispatches to xoq or real pyrealsense2."""

    _real_cls = _MissingPyRealSense2
    _xoq_cls = None

    def __init__(self):
        self._remote = False
        self._real = None
        self._xoq = None

    def start(self, cfg=None):
        if cfg is not None and isinstance(cfg, _XoqConfig) and cfg._remote:
            self._remote = True
            if self._xoq_cls is not None:
                self._xoq = self._xoq_cls()
                return self._xoq_call(self._xoq.start, cfg._xoq)
        else:
            self._remote = False
            try:
                self._real = self._real_cls()
            except (ImportError, Exception):
                pass
            if self._real is not None:
                if cfg is not None and isinstance(cfg, _XoqConfig):
                    return self._real.start(cfg._real)
                return self._real.start(cfg)
            elif self._xoq_cls is not None:
                self._xoq = self._xoq_cls()
                if cfg is not None and isinstance(cfg, _XoqConfig):
                    return self._xoq_call(self._xoq.start, cfg._active)
                return self._xoq_call(self._xoq.start, cfg)
            raise ImportError("No RealSense backend available")

    def wait_for_frames(self):
        if self._remote and self._xoq is not None:
            return self._xoq_call(self._xoq.wait_for_frames)
        elif self._real is not None:
            return self._real.wait_for_frames()
        elif self._xoq is not None:
            return self._xoq_call(self._xoq.wait_for_frames)
        raise RuntimeError("Pipeline not started")

    @staticmethod
    def _xoq_call(fn, *args, **kwargs):
        """Call *fn* with a timeout so remote operations don't block forever."""
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(fn, *args, **kwargs)
            try:
                return future.result(timeout=_XOQ_TIMEOUT)
            except TimeoutError:
                future.cancel()
                raise TimeoutError(
                    f"xoq_realsense: {fn.__name__}() timed out after {_XOQ_TIMEOUT}s. "
                    f"Is the remote relay running? "
                    f"Set XOQ_REALSENSE_TIMEOUT to adjust (current: {_XOQ_TIMEOUT}s)."
                )

    def stop(self):
        if self._xoq is not None:
            try:
                self._xoq.stop()
            except Exception:
                pass
        if self._real is not None:
            try:
                self._real.stop()
            except Exception:
                pass


def _patch_rs2(mod):
    """Patch pyrealsense2 module with xoq-aware wrappers."""
    try:
        import xoq_realsense as _xoq

        # Set up dispatcher classes
        real_pipeline = getattr(mod, "pipeline", _MissingPyRealSense2)
        real_config = getattr(mod, "config", _MissingPyRealSense2)

        _XoqPipeline._real_cls = real_pipeline
        _XoqPipeline._xoq_cls = _xoq.pipeline
        _XoqConfig._real_cls = real_config
        _XoqConfig._xoq_cls = _xoq.config

        mod.pipeline = _XoqPipeline
        mod.config = _XoqConfig

        # Add xoq classes that real pyrealsense2 might not have
        # but keep real ones if they exist
        for name in ("align", "stream", "format", "intrinsics"):
            if not hasattr(mod, name):
                xoq_cls = getattr(_xoq, name, None)
                if xoq_cls is not None:
                    setattr(mod, name, xoq_cls)

    except ImportError:
        pass


def _make_synthetic_rs2():
    """Create a synthetic ``pyrealsense2`` module backed by xoq_realsense."""
    import xoq_realsense as _xoq

    mod = types.ModuleType("pyrealsense2")
    mod.__package__ = "pyrealsense2"
    mod.__path__ = []

    # Set up pipeline/config with dispatchers
    _XoqPipeline._real_cls = _MissingPyRealSense2
    _XoqPipeline._xoq_cls = _xoq.pipeline
    _XoqConfig._real_cls = _MissingPyRealSense2
    _XoqConfig._xoq_cls = _xoq.config

    mod.pipeline = _XoqPipeline
    mod.config = _XoqConfig

    # Direct xoq classes
    mod.align = _xoq.align
    mod.stream = _xoq.stream
    mod.format = _xoq.format
    mod.intrinsics = _xoq.intrinsics

    return mod


class _Rs2Finder(importlib.abc.MetaPathFinder):
    """One-shot meta-path finder that intercepts ``import pyrealsense2``."""

    def find_spec(self, fullname, path, target=None):
        if fullname != "pyrealsense2":
            return None

        # Remove ourselves to avoid recursion
        sys.meta_path[:] = [f for f in sys.meta_path if f is not self]

        # Try the real pyrealsense2 first
        spec = importlib.util.find_spec("pyrealsense2")
        if spec is not None:
            original_loader = spec.loader
            spec.loader = _PatchingLoader(original_loader)
            return spec

        # pyrealsense2 not installed — provide synthetic module from xoq_realsense
        return importlib.machinery.ModuleSpec(
            "pyrealsense2",
            _SyntheticRs2Loader(),
            origin="xoq_realsense",
        )


class _PatchingLoader:
    """Loader wrapper that patches pyrealsense2 after the real loader finishes."""

    def __init__(self, original):
        self._original = original

    def create_module(self, spec):
        if hasattr(self._original, "create_module"):
            return self._original.create_module(spec)
        return None

    def exec_module(self, module):
        self._original.exec_module(module)
        _patch_rs2(module)


class _SyntheticRs2Loader:
    """Loader that creates a synthetic pyrealsense2 module backed by xoq_realsense."""

    def create_module(self, spec):
        return _make_synthetic_rs2()

    def exec_module(self, module):
        pass


def install():
    """Insert the pyrealsense2 import hook (idempotent)."""
    # Already imported — patch in place
    if "pyrealsense2" in sys.modules:
        _patch_rs2(sys.modules["pyrealsense2"])
        return

    # xoq_realsense not available — nothing to do
    try:
        import xoq_realsense  # noqa: F401
    except ImportError:
        return

    # Don't double-install
    if any(isinstance(f, _Rs2Finder) for f in sys.meta_path):
        return

    sys.meta_path.insert(0, _Rs2Finder())
