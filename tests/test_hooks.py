"""Local tests for import hooks (no hardware required).

Tests that the MetaPathFinder hooks correctly patch `import can` and
`import serial` to dispatch between xoq (remote) and real libraries (local).

Run:
    pytest tests/test_hooks.py -v
"""

import importlib
import sys
import types

import pytest


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _fresh_import(name):
    """Remove *name* (and submodules) from sys.modules, then re-import."""
    to_remove = [k for k in sys.modules if k == name or k.startswith(name + ".")]
    for k in to_remove:
        del sys.modules[k]
    return importlib.import_module(name)


# ---------------------------------------------------------------------------
# CAN hook tests
# ---------------------------------------------------------------------------

@pytest.mark.skipif(
    importlib.util.find_spec("xoq_can") is None,
    reason="xoq-can not installed",
)
class TestCanHook:
    """Test the xoq_can import hook dispatching logic."""

    def test_can_hook_detection_remote(self):
        """64-char hex strings are detected as remote channels."""
        from xoq_can._can_hook import _is_remote_channel

        remote_id = "a" * 64
        assert _is_remote_channel(remote_id) is True

    def test_can_hook_detection_local(self):
        """Short CAN interface names are detected as local."""
        from xoq_can._can_hook import _is_remote_channel

        assert _is_remote_channel("can0") is False
        assert _is_remote_channel("vcan0") is False
        assert _is_remote_channel("slcan0") is False
        assert _is_remote_channel(None) is False
        assert _is_remote_channel("") is False

    def test_can_hook_detection_mixed_case(self):
        """Only lowercase hex is treated as remote."""
        from xoq_can._can_hook import _is_remote_channel

        # uppercase should NOT match (iroh IDs are lowercase)
        assert _is_remote_channel("A" * 64) is False
        # 63 chars — too short
        assert _is_remote_channel("a" * 63) is False
        # 65 chars — too long
        assert _is_remote_channel("a" * 65) is False

    def test_can_hook_install_idempotent(self):
        """Calling install() multiple times doesn't duplicate finders."""
        from xoq_can._can_hook import _CanFinder, install

        install()
        install()
        count = sum(1 for f in sys.meta_path if isinstance(f, _CanFinder))
        assert count <= 1

    def test_xoq_can_exports_bus_and_message(self):
        """xoq_can exports Bus and Message classes."""
        import xoq_can

        assert hasattr(xoq_can, "Bus")
        assert hasattr(xoq_can, "Message")

    def test_xoq_can_message_creation(self):
        """xoq_can.Message can be constructed with python-can compatible args."""
        import xoq_can

        msg = xoq_can.Message(arbitration_id=0x123, data=[1, 2, 3, 4])
        assert msg.arbitration_id == 0x123
        assert msg.data == [1, 2, 3, 4]
        assert msg.is_fd is False

    def test_xoq_can_message_fd(self):
        """CAN FD messages can be created."""
        import xoq_can

        msg = xoq_can.Message(
            arbitration_id=0x100,
            data=list(range(64)),
            is_fd=True,
            bitrate_switch=True,
        )
        assert msg.is_fd is True
        assert msg.bitrate_switch is True
        assert len(msg.data) == 64

    @pytest.mark.skipif(
        "can" not in sys.modules
        and importlib.util.find_spec("can") is None,
        reason="python-can not installed",
    )
    def test_can_hook_patches_bus(self):
        """When python-can is installed, import can patches Bus."""
        from xoq_can._can_hook import _XoqBusType, install

        can = _fresh_import("can")
        install()  # patches the freshly-imported module in sys.modules
        assert isinstance(can.Bus, _XoqBusType), (
            f"can.Bus should be patched, got {type(can.Bus)}"
        )

    @pytest.mark.skipif(
        "can" not in sys.modules
        and importlib.util.find_spec("can") is None,
        reason="python-can not installed",
    )
    def test_can_hook_patches_interface_bus(self):
        """can.interface.Bus is also patched."""
        from xoq_can._can_hook import _XoqBusType, install

        can = _fresh_import("can")
        install()
        assert hasattr(can, "interface")
        assert isinstance(can.interface.Bus, _XoqBusType)


# ---------------------------------------------------------------------------
# Serial hook tests
# ---------------------------------------------------------------------------

@pytest.mark.skipif(
    importlib.util.find_spec("xoq_serial") is None,
    reason="xoq-serial not installed",
)
class TestSerialHook:
    """Test the xoq_serial import hook dispatching logic."""

    def test_serial_hook_detection_remote(self):
        """64-char hex strings are detected as remote ports."""
        from xoq_serial._serial_hook import _is_remote_port

        remote_id = "b" * 64
        assert _is_remote_port(remote_id) is True

    def test_serial_hook_detection_local(self):
        """Local serial port paths are detected as local."""
        from xoq_serial._serial_hook import _is_remote_port

        assert _is_remote_port("/dev/ttyUSB0") is False
        assert _is_remote_port("/dev/ttyACM0") is False
        assert _is_remote_port("COM3") is False
        assert _is_remote_port(None) is False
        assert _is_remote_port("") is False

    def test_serial_hook_detection_edge_cases(self):
        """Edge cases for remote port detection."""
        from xoq_serial._serial_hook import _is_remote_port

        # uppercase — not a valid iroh ID
        assert _is_remote_port("B" * 64) is False
        # wrong length
        assert _is_remote_port("b" * 63) is False
        assert _is_remote_port("b" * 65) is False
        # non-hex chars
        assert _is_remote_port("g" * 64) is False

    def test_serial_hook_install_idempotent(self):
        """Calling install() multiple times doesn't duplicate finders."""
        from xoq_serial._serial_hook import _SerialFinder, install

        install()
        install()
        count = sum(1 for f in sys.meta_path if isinstance(f, _SerialFinder))
        assert count <= 1

    def test_xoq_serial_exports_serial(self):
        """xoq_serial exports Serial class."""
        import xoq_serial

        assert hasattr(xoq_serial, "Serial")

    def test_xoq_serial_constants(self):
        """xoq_serial exports pyserial-compatible constants."""
        import xoq_serial

        assert xoq_serial.PARITY_NONE == "N"
        assert xoq_serial.EIGHTBITS == 8
        assert xoq_serial.STOPBITS_ONE == 1.0

    @pytest.mark.skipif(
        "serial" not in sys.modules
        and importlib.util.find_spec("serial") is None,
        reason="pyserial not installed",
    )
    def test_serial_hook_patches_serial(self):
        """When pyserial is installed, import serial patches Serial."""
        from xoq_serial._serial_hook import _XoqSerialType, install

        serial = _fresh_import("serial")
        install()
        assert isinstance(serial.Serial, _XoqSerialType), (
            f"serial.Serial should be patched, got {type(serial.Serial)}"
        )


# ---------------------------------------------------------------------------
# CV2 hook tests (for completeness)
# ---------------------------------------------------------------------------

@pytest.mark.skipif(
    importlib.util.find_spec("xoq_cv2") is None,
    reason="xoq-cv2 not installed",
)
class TestCv2Hook:
    """Test the xoq_cv2 import hook dispatching logic."""

    def test_cv2_hook_detection_remote(self):
        """64-char hex strings are detected as remote cameras."""
        from xoq_cv2._cv2_hook import _is_remote_source

        remote_id = "c" * 64
        assert _is_remote_source(remote_id) is True

    def test_cv2_hook_detection_local(self):
        """Integer indices and local paths are detected as local."""
        from xoq_cv2._cv2_hook import _is_remote_source

        assert _is_remote_source(0) is False
        assert _is_remote_source(1) is False
        assert _is_remote_source("/dev/video0") is False
        assert _is_remote_source(None) is False

    def test_xoq_cv2_exports_videocapture(self):
        """xoq_cv2 exports VideoCapture class."""
        import xoq_cv2

        assert hasattr(xoq_cv2, "VideoCapture")

    @pytest.mark.skipif(
        "cv2" not in sys.modules
        and importlib.util.find_spec("cv2") is None,
        reason="opencv-python not installed",
    )
    def test_cv2_hook_patches_videocapture(self):
        """When opencv-python is installed, import cv2 patches VideoCapture."""
        from xoq_cv2._cv2_hook import _XoqVideoCaptureType, install

        cv2 = _fresh_import("cv2")
        install()
        assert isinstance(cv2.VideoCapture, _XoqVideoCaptureType)
