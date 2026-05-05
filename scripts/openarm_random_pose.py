#!/usr/bin/env python3
"""OpenArm random-pose demo.

Connects to a can-server via xoq_can, enables MIT mode on all 8 Damiao motors,
proposes a random joint pose within the OpenArm joint limits, asks the user to
confirm, then linearly interpolates from the current pose to the target.

Per CLAUDE.md: NEVER send CAN data without explicit user approval. The script
prints the proposed pose and waits for Enter before sending any motor command.

Usage:
    python scripts/openarm_random_pose.py [<server-id>] [options]

Options:
    --seed N         RNG seed for reproducible poses
    --kp-scale F     Multiplier on per-motor KP (default 0.5)
    --max-speed F    Max joint angular speed during move, rad/s (default 1.0)
    --rate-hz F      Control loop rate, Hz (default 50)
    --loop           Keep proposing/executing poses until Ctrl+C
"""

import argparse
import math
import random
import signal
import sys
import time

import xoq_can


# Damiao MIT-mode encoding ranges (from examples/openarm_teleop.rs:44-49)
POS_MIN, POS_MAX = -12.5, 12.5
VEL_MIN, VEL_MAX = -45.0, 45.0
TAU_MIN, TAU_MAX = -18.0, 18.0
KP_MAX = 500.0
KD_MAX = 5.0

# OpenArm joint limits in radians (from examples/openarm_teleop.rs:62-71)
JOINT_LIMITS = [
    (-1.396, 3.491),  # J1
    (-0.175, 3.316),  # J2
    (-1.571, 1.571),  # J3
    (-0.175, 2.967),  # J4
    (-1.571, 1.571),  # J5
    (-1.571, 1.571),  # J6
    (-1.571, 1.571),  # J7
    (-0.175, 1.745),  # Grip
]
JOINT_NAMES = ["J1", "J2", "J3", "J4", "J5", "J6", "J7", "Grip"]

# Per-motor PD gains (from examples/openarm_playback.rs:80-81)
MOTOR_KP = [300.0, 300.0, 150.0, 150.0, 40.0, 40.0, 30.0, 30.0]
MOTOR_KD = [15.0, 15.0, 7.5, 7.5, 2.0, 2.0, 1.5, 1.5]

ENABLE_MIT = bytes([0xFF] * 7 + [0xFC])
DISABLE_MIT = bytes([0xFF] * 7 + [0xFD])
QUERY_CMD = bytes([0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00])

NUM_MOTORS = 8
CMD_ID_BASE = 0x01     # motor command IDs 0x01..0x08
RESP_ID_BASE = 0x11    # motor response IDs 0x11..0x18

DEFAULT_SERVER = "0880f98cdcdc414f0c6b2b5f8c5f87dba931d63da25324727c75fdbef9628a15"
DEFAULT_SERVER_NAME = "baguette right"


def encode_damiao_cmd(pos, vel, kp, kd, tau):
    """Pack an MIT-mode command into 8 bytes (port of openarm_teleop.rs:112-130)."""
    def clamp_u(x, lo, hi):
        return max(lo, min(hi, int(x)))

    pos_raw = clamp_u(((pos - POS_MIN) / (POS_MAX - POS_MIN)) * 65535.0, 0, 65535)
    vel_raw = clamp_u(((vel - VEL_MIN) / (VEL_MAX - VEL_MIN)) * 4095.0, 0, 4095)
    kp_raw = clamp_u((kp / KP_MAX) * 4095.0, 0, 4095)
    kd_raw = clamp_u((kd / KD_MAX) * 4095.0, 0, 4095)
    tau_raw = clamp_u(((tau - TAU_MIN) / (TAU_MAX - TAU_MIN)) * 4095.0, 0, 4095)

    return bytes([
        (pos_raw >> 8) & 0xFF,
        pos_raw & 0xFF,
        (vel_raw >> 4) & 0xFF,
        ((vel_raw & 0x0F) << 4) | ((kp_raw >> 8) & 0x0F),
        kp_raw & 0xFF,
        (kd_raw >> 4) & 0xFF,
        ((kd_raw & 0x0F) << 4) | ((tau_raw >> 8) & 0x0F),
        tau_raw & 0xFF,
    ])


def decode_response_pos(data):
    """Decode position from a Damiao response frame (openarm_query.rs:32-35)."""
    pos_raw = (data[1] << 8) | data[2]
    return pos_raw / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN


def drain(bus, timeout_s=0.05):
    while bus.recv(timeout=timeout_s) is not None:
        pass


def enable_motors(bus):
    print("Enabling MIT mode on motors 1..8...")
    for i in range(NUM_MOTORS):
        bus.send(xoq_can.Message(arbitration_id=CMD_ID_BASE + i, data=ENABLE_MIT))
        bus.recv(timeout=0.1)
    drain(bus)


def disable_motors(bus):
    for i in range(NUM_MOTORS):
        try:
            bus.send(xoq_can.Message(arbitration_id=CMD_ID_BASE + i, data=DISABLE_MIT))
        except Exception:
            pass


def query_positions(bus, retries=3):
    """Send zero-torque query and read back current positions for all 8 motors."""
    positions = [None] * NUM_MOTORS

    for attempt in range(retries):
        for i in range(NUM_MOTORS):
            if positions[i] is None:
                bus.send(xoq_can.Message(arbitration_id=CMD_ID_BASE + i, data=QUERY_CMD))

        deadline = time.perf_counter() + 0.5
        while any(p is None for p in positions) and time.perf_counter() < deadline:
            msg = bus.recv(timeout=0.1)
            if msg is None:
                continue
            cid = msg.arbitration_id
            if RESP_ID_BASE <= cid < RESP_ID_BASE + NUM_MOTORS and len(msg.data) >= 8:
                idx = cid - RESP_ID_BASE
                if positions[idx] is None:
                    positions[idx] = decode_response_pos(msg.data)

        if all(p is not None for p in positions):
            return positions

    missing = [JOINT_NAMES[i] for i, p in enumerate(positions) if p is None]
    raise RuntimeError(f"Could not read positions for: {', '.join(missing)}")


def random_pose(rng):
    return [rng.uniform(lo, hi) for lo, hi in JOINT_LIMITS]


def fmt_deg_row(label, values):
    cells = " ".join(f"{math.degrees(v):>8.1f}" for v in values)
    return f"{label:<10}{cells}"


def print_pose_proposal(current, target):
    header = "          " + " ".join(f"{n:>8}" for n in JOINT_NAMES)
    print(header)
    print("-" * len(header))
    print(fmt_deg_row("current", current))
    print(fmt_deg_row("target", target))
    deltas = [t - c for c, t in zip(current, target)]
    print(fmt_deg_row("delta", deltas))


def confirm():
    print()
    try:
        ans = input("Press Enter to execute, anything else to abort: ").strip()
    except EOFError:
        return False
    return ans == ""


def move_to(bus, current, target, max_speed, rate_hz, kp_scale):
    max_delta = max(abs(t - c) for c, t in zip(current, target))
    if max_delta < 1e-6:
        print("Already at target.")
        return

    duration_s = max_delta / max_speed
    n_steps = int(math.ceil(duration_s * rate_hz))
    n_steps = max(5, min(200, n_steps))
    dt = 1.0 / rate_hz

    print(f"Moving in {n_steps} steps over {n_steps * dt:.2f}s "
          f"(max delta {math.degrees(max_delta):.1f}°)...")

    for step in range(1, n_steps + 1):
        alpha = step / n_steps
        for i in range(NUM_MOTORS):
            pos = current[i] + alpha * (target[i] - current[i])
            kp = MOTOR_KP[i] * kp_scale
            kd = MOTOR_KD[i]
            data = encode_damiao_cmd(pos, 0.0, kp, kd, 0.0)
            bus.send(xoq_can.Message(arbitration_id=CMD_ID_BASE + i, data=data))
        time.sleep(dt)

    # Hold target for 0.5s so the PD loop converges before we release.
    hold_steps = int(round(0.5 * rate_hz))
    for _ in range(hold_steps):
        for i in range(NUM_MOTORS):
            kp = MOTOR_KP[i] * kp_scale
            kd = MOTOR_KD[i]
            data = encode_damiao_cmd(target[i], 0.0, kp, kd, 0.0)
            bus.send(xoq_can.Message(arbitration_id=CMD_ID_BASE + i, data=data))
        time.sleep(dt)


def parse_args():
    p = argparse.ArgumentParser(description="OpenArm random-pose demo")
    p.add_argument("server_id", nargs="?", default=DEFAULT_SERVER,
                   help=f"can-server iroh node ID (default: {DEFAULT_SERVER_NAME})")
    p.add_argument("--seed", type=int, default=None, help="RNG seed")
    p.add_argument("--kp-scale", type=float, default=0.5, dest="kp_scale",
                   help="Per-motor KP multiplier (default 0.5)")
    p.add_argument("--max-speed", type=float, default=1.0, dest="max_speed",
                   help="Max joint angular speed, rad/s (default 1.0)")
    p.add_argument("--rate-hz", type=float, default=50.0, dest="rate_hz",
                   help="Control loop rate, Hz (default 50)")
    p.add_argument("--loop", action="store_true",
                   help="Propose poses repeatedly until Ctrl+C")
    return p.parse_args()


def main():
    args = parse_args()
    rng = random.Random(args.seed)

    server_label = (DEFAULT_SERVER_NAME if args.server_id == DEFAULT_SERVER
                    else args.server_id[:12] + "...")
    print(f"=== OpenArm random-pose demo ===")
    print(f"Server:    {server_label}")
    print(f"kp_scale:  {args.kp_scale}")
    print(f"max_speed: {args.max_speed} rad/s")
    print(f"rate_hz:   {args.rate_hz}")
    print()

    print(f"Connecting to {args.server_id[:12]}...")
    bus = xoq_can.Bus(channel=args.server_id, timeout=5.0)
    print("Connected.\n")

    interrupted = {"flag": False}

    def on_sigint(_sig, _frm):
        interrupted["flag"] = True
        print("\nInterrupted — releasing motors.")

    signal.signal(signal.SIGINT, on_sigint)

    try:
        enable_motors(bus)

        while True:
            if interrupted["flag"]:
                break

            current = query_positions(bus)
            target = random_pose(rng)

            print()
            print_pose_proposal(current, target)

            if not confirm():
                print("Aborted.")
                if args.loop and not interrupted["flag"]:
                    continue
                break

            move_to(bus, current, target,
                    max_speed=args.max_speed,
                    rate_hz=args.rate_hz,
                    kp_scale=args.kp_scale)
            print("Done.")

            if not args.loop:
                break
    finally:
        disable_motors(bus)
        try:
            bus.shutdown()
        except Exception:
            pass


if __name__ == "__main__":
    main()
