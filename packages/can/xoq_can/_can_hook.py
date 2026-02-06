"""Startup hook that patches ``import can`` to include xoq remote CAN buses.

Installed via ``xoq_can_hook.pth`` so it runs automatically at interpreter
startup.  After the hook fires once it removes itself from
``sys.meta_path`` — zero overhead for subsequent imports.
"""

import importlib
import importlib.abc
import importlib.machinery
import importlib.util
import re
import sys


# Pattern for iroh node IDs (64-char hex-encoded ed25519 public keys)
_IROH_ID_RE = re.compile(r"^[a-f0-9]{64}$")


def _is_remote_channel(channel):
    """Return True if *channel* looks like a remote CAN bus identifier."""
    s = str(channel) if channel is not None else ""
    return bool(_IROH_ID_RE.match(s))


class _XoqBusType(type):
    """Metaclass: dispatches Bus() construction based on channel identifier."""

    def __call__(cls, *args, **kwargs):
        channel = args[0] if args else kwargs.get("channel")

        if _is_remote_channel(channel):
            # Strip args that xoq doesn't need but python-can passes
            kwargs.pop("interface", None)
            if args:
                return cls._xoq(*args, **kwargs)
            return cls._xoq(**kwargs)

        return cls._real(*args, **kwargs)

    def __instancecheck__(cls, instance):
        return type.__instancecheck__(cls, instance) or isinstance(
            instance, (cls._real, cls._xoq)
        )


class _XoqBus(metaclass=_XoqBusType):
    _real = object
    _xoq = object


def _patch_can(mod):
    """Patch can.Bus and can.interface.Bus with xoq-aware wrapper."""
    try:
        import xoq_can as _xoq

        if not isinstance(mod.Bus, _XoqBusType):
            _XoqBus._real = mod.Bus
            _XoqBus._xoq = _xoq.Bus
            _XoqBus.__name__ = "Bus"
            _XoqBus.__qualname__ = "Bus"
            mod.Bus = _XoqBus

        # Also patch can.interface.Bus (lerobot uses this path)
        if hasattr(mod, "interface") and hasattr(mod.interface, "Bus"):
            if not isinstance(mod.interface.Bus, _XoqBusType):
                mod.interface.Bus = _XoqBus
    except ImportError:
        pass


class _CanFinder(importlib.abc.MetaPathFinder):
    """One-shot meta-path finder that intercepts ``import can``."""

    def find_spec(self, fullname, path, target=None):
        if fullname != "can":
            return None

        # Remove ourselves to avoid recursion
        sys.meta_path[:] = [f for f in sys.meta_path if f is not self]

        # Let the real can import happen normally
        spec = importlib.util.find_spec("can")
        if spec is None:
            return None

        # Wrap the loader to patch after loading
        original_loader = spec.loader
        spec.loader = _PatchingLoader(original_loader)
        return spec


class _PatchingLoader:
    """Loader wrapper that patches can after the real loader finishes."""

    def __init__(self, original):
        self._original = original

    def create_module(self, spec):
        if hasattr(self._original, "create_module"):
            return self._original.create_module(spec)
        return None

    def exec_module(self, module):
        self._original.exec_module(module)
        _patch_can(module)


def install():
    """Insert the can import hook (idempotent, guards against re-entry)."""
    # Already imported — patch in place
    if "can" in sys.modules:
        _patch_can(sys.modules["can"])
        return

    # xoq_can not available — nothing to do
    try:
        import xoq_can  # noqa: F401
    except ImportError:
        return

    # Don't double-install
    if any(isinstance(f, _CanFinder) for f in sys.meta_path):
        return

    sys.meta_path.insert(0, _CanFinder())
