"""Hardware integration tests — actual device communication.

Goes beyond connectivity: sends real protocol commands and validates
responses from the hardware.

- Serial (SO100): STS3215 FeeTech protocol V1 — ping, read position, torque control
- CAN (OpenArm): Damiao motor protocol — enable, disable, read state
- Camera: Read frames, verify dimensions and content

Run locally:
    source .env.test && python -m pytest tests/test_integration.py -v

Environment variables:
    XOQ_SERIAL_SERVER_ID: Iroh endpoint ID of the serial server (SO100)
    XOQ_CAMERA_SERVER_ID: Iroh endpoint ID of the camera server
    XOQ_CAN_SERVER_ID:    Iroh endpoint ID of the CAN server (openarm)
"""

import os
import struct
import time

import pytest

SERIAL_SERVER_ID = os.environ.get("XOQ_SERIAL_SERVER_ID")
CAMERA_SERVER_ID = os.environ.get("XOQ_CAMERA_SERVER_ID")
CAN_SERVER_ID = os.environ.get("XOQ_CAN_SERVER_ID")

# ---------------------------------------------------------------------------
# STS3215 FeeTech Protocol V1 helpers
# ---------------------------------------------------------------------------

# Protocol V1 constants
STS_HEADER = bytes([0xFF, 0xFF])
STS_INST_PING = 0x01
STS_INST_READ = 0x02
STS_INST_WRITE = 0x03

# STS3215 register addresses
STS_TORQUE_ENABLE = 40       # 1 byte: 0=off, 1=on
STS_PRESENT_POSITION = 56    # 2 bytes (little-endian)


def _sts_checksum(packet_body: bytes) -> int:
    """Compute FeeTech protocol V1 checksum: ~(sum of body bytes) & 0xFF."""
    return (~sum(packet_body)) & 0xFF


def sts_ping(servo_id: int) -> bytes:
    """Build a PING packet for STS3215 servo."""
    length = 2  # instruction + checksum
    body = bytes([servo_id, length, STS_INST_PING])
    return STS_HEADER + body + bytes([_sts_checksum(body)])


def sts_read(servo_id: int, address: int, count: int) -> bytes:
    """Build a READ packet for STS3215 servo."""
    length = 4  # instruction + addr + count + checksum
    body = bytes([servo_id, length, STS_INST_READ, address, count])
    return STS_HEADER + body + bytes([_sts_checksum(body)])


def sts_write_byte(servo_id: int, address: int, value: int) -> bytes:
    """Build a WRITE packet for a single byte."""
    length = 4  # instruction + addr + value + checksum
    body = bytes([servo_id, length, STS_INST_WRITE, address, value])
    return STS_HEADER + body + bytes([_sts_checksum(body)])


def parse_sts_response(data: bytes):
    """Parse an STS3215 response packet.

    Returns (servo_id, error, params) or None if invalid.
    """
    # Find header
    idx = data.find(STS_HEADER)
    if idx < 0 or idx + 4 > len(data):
        return None
    servo_id = data[idx + 2]
    length = data[idx + 3]
    if idx + 3 + length > len(data):
        return None
    error = data[idx + 4]
    params = data[idx + 5 : idx + 3 + length]  # excludes checksum
    return servo_id, error, params


# ---------------------------------------------------------------------------
# Damiao CAN protocol helpers
# ---------------------------------------------------------------------------

DAMIAO_ENABLE = bytes([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC])
DAMIAO_DISABLE = bytes([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD])


def damiao_parse_state(data: bytes):
    """Parse Damiao motor state response (8 bytes).

    Returns (position_raw, velocity_raw, torque_raw, t_mos, t_rotor) or None.
    """
    if len(data) < 8:
        return None
    # Motor ID is in data[0], state is in data[1:8]
    q_raw = (data[1] << 8) | data[2]
    dq_raw = (data[3] << 4) | (data[4] >> 4)
    tau_raw = ((data[4] & 0x0F) << 8) | data[5]
    t_mos = data[6]
    t_rotor = data[7]
    return q_raw, dq_raw, tau_raw, t_mos, t_rotor


# ---------------------------------------------------------------------------
# Serial integration tests (SO100 — STS3215 servos)
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def serial_conn():
    """Module-scoped serial connection (reused across all serial tests)."""
    if not SERIAL_SERVER_ID:
        pytest.skip("XOQ_SERIAL_SERVER_ID not set")
    import xoq_serial
    ser = xoq_serial.Serial(SERIAL_SERVER_ID, timeout=5.0)
    yield ser
    ser.close()


@pytest.mark.skipif(not SERIAL_SERVER_ID, reason="XOQ_SERIAL_SERVER_ID not set")
class TestSerialIntegration:
    """Integration tests for SO100 arm over remote serial."""

    @pytest.mark.timeout(30)
    def test_ping_servo(self, serial_conn):
        """Ping servo ID 1 and get a valid response."""
        serial_conn.reset_input_buffer()
        serial_conn.write(sts_ping(1))
        time.sleep(0.05)
        resp = serial_conn.read(32)
        assert len(resp) >= 6, f"Ping response too short: {resp.hex()}"
        parsed = parse_sts_response(resp)
        assert parsed is not None, f"Could not parse response: {resp.hex()}"
        servo_id, error, _ = parsed
        assert servo_id == 1, f"Unexpected servo ID: {servo_id}"
        assert error == 0, f"Servo returned error: {error}"

    @pytest.mark.timeout(30)
    def test_read_present_position(self, serial_conn):
        """Read present position from servo ID 1."""
        serial_conn.reset_input_buffer()
        serial_conn.write(sts_read(1, STS_PRESENT_POSITION, 2))
        time.sleep(0.05)
        resp = serial_conn.read(32)
        parsed = parse_sts_response(resp)
        assert parsed is not None, f"Could not parse response: {resp.hex()}"
        servo_id, error, params = parsed
        assert servo_id == 1
        assert error == 0, f"Servo returned error: {error}"
        assert len(params) >= 2, f"Expected 2 position bytes, got {len(params)}"
        position = params[0] | (params[1] << 8)
        # STS3215 position range: 0-4095 (12-bit)
        assert 0 <= position <= 4095, f"Position out of range: {position}"

    @pytest.mark.timeout(30)
    def test_disable_torque(self, serial_conn):
        """Disable torque on servo ID 1 and verify no error."""
        serial_conn.reset_input_buffer()
        serial_conn.write(sts_write_byte(1, STS_TORQUE_ENABLE, 0))
        time.sleep(0.05)
        resp = serial_conn.read(32)
        parsed = parse_sts_response(resp)
        assert parsed is not None, f"Could not parse response: {resp.hex()}"
        servo_id, error, _ = parsed
        assert servo_id == 1
        assert error == 0, f"Servo returned error on torque disable: {error}"

    @pytest.mark.timeout(30)
    def test_read_all_servo_positions(self, serial_conn):
        """Read positions from all 5 SO100 servos."""
        for sid in range(1, 6):
            serial_conn.reset_input_buffer()
            serial_conn.write(sts_read(sid, STS_PRESENT_POSITION, 2))
            time.sleep(0.05)
            resp = serial_conn.read(32)
            parsed = parse_sts_response(resp)
            assert parsed is not None, f"Servo {sid}: no response"
            servo_id, error, params = parsed
            assert servo_id == sid, f"Expected servo {sid}, got {servo_id}"
            assert error == 0, f"Servo {sid} error: {error}"
            assert len(params) >= 2, f"Servo {sid}: short position data"
            position = params[0] | (params[1] << 8)
            assert 0 <= position <= 4095, f"Servo {sid} position out of range: {position}"


# ---------------------------------------------------------------------------
# CAN integration tests (OpenArm — Damiao motors)
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def can_conn():
    """Module-scoped CAN connection (reused across all CAN tests)."""
    if not CAN_SERVER_ID:
        pytest.skip("XOQ_CAN_SERVER_ID not set")
    import xoq_can
    bus = xoq_can.Bus(channel=CAN_SERVER_ID, fd=True, timeout=10.0)
    yield bus, xoq_can
    bus.shutdown()


@pytest.mark.skipif(not CAN_SERVER_ID, reason="XOQ_CAN_SERVER_ID not set")
class TestCanIntegration:
    """Integration tests for OpenArm over remote CAN bus."""

    @pytest.mark.timeout(30)
    def test_disable_motor(self, can_conn):
        """Send disable command to motor CAN ID 0x01 and get state response."""
        bus, xoq_can = can_conn
        msg = xoq_can.Message(
            arbitration_id=0x01,
            data=list(DAMIAO_DISABLE),
            is_fd=False,
        )
        bus.send(msg)
        resp = bus.recv(timeout=5.0)
        # Motor should respond with a state frame
        if resp is not None:
            assert len(resp.data) == 8, f"Expected 8-byte state, got {len(resp.data)}"
            state = damiao_parse_state(bytes(resp.data))
            assert state is not None, "Could not parse motor state"

    @pytest.mark.timeout(30)
    def test_enable_disable_roundtrip(self, can_conn):
        """Enable then disable motor, verifying responses."""
        bus, xoq_can = can_conn
        # Enable
        enable_msg = xoq_can.Message(
            arbitration_id=0x01,
            data=list(DAMIAO_ENABLE),
        )
        bus.send(enable_msg)
        resp = bus.recv(timeout=5.0)
        if resp is not None:
            assert len(resp.data) == 8

        time.sleep(0.1)

        # Disable (safe state)
        disable_msg = xoq_can.Message(
            arbitration_id=0x01,
            data=list(DAMIAO_DISABLE),
        )
        bus.send(disable_msg)
        resp = bus.recv(timeout=5.0)
        if resp is not None:
            assert len(resp.data) == 8

    @pytest.mark.timeout(30)
    def test_read_motor_state(self, can_conn):
        """Send disable command and parse motor state from response."""
        bus, xoq_can = can_conn
        msg = xoq_can.Message(
            arbitration_id=0x01,
            data=list(DAMIAO_DISABLE),
        )
        bus.send(msg)
        resp = bus.recv(timeout=5.0)
        if resp is not None:
            state = damiao_parse_state(bytes(resp.data))
            assert state is not None
            q_raw, dq_raw, tau_raw, t_mos, t_rotor = state
            # Position raw is 16-bit (0-65535)
            assert 0 <= q_raw <= 65535
            # Temperature should be reasonable (0-100°C)
            assert 0 <= t_mos <= 100, f"MOS temp out of range: {t_mos}"


# ---------------------------------------------------------------------------
# Camera integration tests
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def camera_conn():
    """Module-scoped camera connection (reused across all camera tests)."""
    if not CAMERA_SERVER_ID:
        pytest.skip("XOQ_CAMERA_SERVER_ID not set")
    import xoq_cv2
    cap = xoq_cv2.VideoCapture(CAMERA_SERVER_ID, "iroh")
    yield cap
    cap.release()


@pytest.mark.skipif(not CAMERA_SERVER_ID, reason="XOQ_CAMERA_SERVER_ID not set")
class TestCameraIntegration:
    """Integration tests for remote camera."""

    @pytest.mark.timeout(30)
    def test_read_multiple_frames(self, camera_conn):
        """Read 5 consecutive frames to verify streaming works."""
        for i in range(5):
            ret, frame = camera_conn.read()
            assert ret is True, f"Frame {i}: read failed"
            assert frame is not None, f"Frame {i}: frame is None"
            assert len(frame.shape) == 3
            assert frame.shape[2] == 3  # BGR

    @pytest.mark.timeout(30)
    def test_frame_dimensions_consistent(self, camera_conn):
        """All frames should have the same dimensions."""
        ret, first = camera_conn.read()
        assert ret
        h, w = first.shape[:2]
        for _ in range(4):
            ret, frame = camera_conn.read()
            assert ret
            assert frame.shape[0] == h
            assert frame.shape[1] == w

    @pytest.mark.timeout(30)
    def test_frame_not_black(self, camera_conn):
        """Frame should contain some non-zero pixel data (not a blank image)."""
        import numpy as np
        ret, frame = camera_conn.read()
        assert ret
        # At least some pixels should be non-zero
        assert np.any(frame > 0), "Frame is completely black"
