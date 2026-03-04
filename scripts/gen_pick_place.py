#!/usr/bin/env python3
"""Generate a pick-and-place trajectory for OpenArm right arm.

Outputs a v2 JSON file for openarm_playback --step.
"""

import json
import math
import base64
import numpy as np
from scipy.optimize import minimize

# --- Damiao MIT protocol ---

POS_MIN, POS_MAX = -12.5, 12.5
VEL_MIN, VEL_MAX = -45.0, 45.0
TAU_MIN, TAU_MAX = -18.0, 18.0


def encode_damiao_cmd(pos, vel, kp, kd, tau):
    pos_raw = max(0, min(65535, int(((pos - POS_MIN) / (POS_MAX - POS_MIN)) * 65535)))
    vel_raw = max(0, min(4095, int(((vel - VEL_MIN) / (VEL_MAX - VEL_MIN)) * 4095)))
    kp_raw = max(0, min(4095, int((kp / 500.0) * 4095)))
    kd_raw = max(0, min(4095, int((kd / 5.0) * 4095)))
    tau_raw = max(0, min(4095, int(((tau - TAU_MIN) / (TAU_MAX - TAU_MIN)) * 4095)))
    return bytes([
        (pos_raw >> 8) & 0xFF, pos_raw & 0xFF,
        (vel_raw >> 4) & 0xFF, ((vel_raw & 0x0F) << 4) | ((kp_raw >> 8) & 0x0F),
        kp_raw & 0xFF,
        (kd_raw >> 4) & 0xFF, ((kd_raw & 0x0F) << 4) | ((tau_raw >> 8) & 0x0F),
        tau_raw & 0xFF,
    ])


GRIPPER_OPEN = -1.0472   # ~-60 deg = fully open
GRIPPER_CLOSED = 0.0      # closed


def make_frame(motor_id, pos, kp=30.0, kd=1.0):
    data = encode_damiao_cmd(pos, 0.0, kp, kd, 0.0)
    return {"id": f"0x{motor_id:02X}", "data": base64.b64encode(data).decode()}


def make_timestep(t, joint_angles, gripper_rad, kp=30.0, kd=1.0):
    frames = []
    for i, angle in enumerate(joint_angles):
        frames.append(make_frame(i + 1, angle, kp=kp, kd=kd))
    frames.append(make_frame(0x08, gripper_rad, kp=kp, kd=kd))
    return {"t": round(t, 3), "frames": frames}


# --- FK from URDF ---

def rot_x(a):
    c, s = np.cos(a), np.sin(a)
    return np.array([[1,0,0,0],[0,c,-s,0],[0,s,c,0],[0,0,0,1]])

def rot_y(a):
    c, s = np.cos(a), np.sin(a)
    return np.array([[c,0,s,0],[0,1,0,0],[-s,0,c,0],[0,0,0,1]])

def rot_z(a):
    c, s = np.cos(a), np.sin(a)
    return np.array([[c,-s,0,0],[s,c,0,0],[0,0,1,0],[0,0,0,1]])

def trans(x, y, z):
    T = np.eye(4); T[0,3], T[1,3], T[2,3] = x, y, z; return T


def fk_right_arm(q):
    """FK for right arm, returns 4x4 TCP transform in body_link0 frame."""
    T = np.eye(4)
    T = T @ trans(0, -0.031, 0.698) @ rot_x(1.5708)          # body → right_link0
    T = T @ trans(0, 0, 0.0625) @ rot_z(q[0])                 # R_J1
    T = T @ trans(-0.0301, 0, 0.06) @ rot_x(1.5708) @ rot_x(-q[1])  # R_J2
    T = T @ trans(0.0301, 0, 0.06625) @ rot_z(q[2])           # R_J3
    T = T @ trans(0, 0.0315, 0.15375) @ rot_y(q[3])           # R_J4
    T = T @ trans(0, -0.0315, 0.0955) @ rot_z(q[4])           # R_J5
    T = T @ trans(0.0375, 0, 0.1205) @ rot_x(q[5])            # R_J6
    T = T @ trans(-0.0375, 0, 0) @ rot_y(q[6])                # R_J7
    T = T @ trans(1e-6, 0.0205, 0)                            # link8
    T = T @ trans(0, -0.025, 0.1001)                           # hand
    T = T @ trans(0, 0, 0.08)                                  # TCP
    return T


# Joint limits (right arm)
LIMITS = [
    (-1.396263, 3.490659),   # J1
    (-0.174533, 3.316125),   # J2
    (-1.570796, 1.570796),   # J3
    (0.0, 2.443461),         # J4
    (-1.570796, 1.570796),   # J5
    (-0.785398, 0.785398),   # J6
    (-1.570796, 1.570796),   # J7
]


def solve_ik(target_pos, q_init, target_z=None, w_ori=0.3):
    """Solve IK using scipy L-BFGS-B."""
    bounds = [(lo + 0.02, hi - 0.02) for lo, hi in LIMITS]

    def cost(q):
        T = fk_right_arm(q)
        c = np.sum((T[:3, 3] - target_pos) ** 2)
        if target_z is not None:
            c += w_ori * (1.0 - np.dot(T[:3, 2], target_z))
        return c

    # Try from provided seed and a few random restarts
    best_q, best_cost = q_init.copy(), float('inf')
    seeds = [q_init]
    for _ in range(5):
        seed = np.array([np.random.uniform(lo, hi) for lo, hi in bounds])
        seeds.append(seed)

    for seed in seeds:
        res = minimize(cost, seed, method='L-BFGS-B', bounds=bounds,
                       options={'maxiter': 2000, 'ftol': 1e-15})
        if res.fun < best_cost:
            best_cost = res.fun
            best_q = res.x.copy()

    return best_q


def main():
    np.random.seed(42)
    down = np.array([0, 0, -1.0])

    # Verify FK
    T0 = fk_right_arm(np.zeros(7))
    print(f"Home TCP: ({T0[0,3]:.3f}, {T0[1,3]:.3f}, {T0[2,3]:.3f})")
    print(f"  Min reachable z ≈ {T0[2,3]:.3f}m (TCP pointing down)")

    # Pick-and-place waypoints in body_link0 frame
    # z=0.10 is near the lowest reachable with downward gripper
    # Positions verified reachable: (0,-0.15), (0.1,-0.20), (0.1,-0.15)

    waypoint_defs = [
        ("home",         [0.00, -0.15, 0.20], GRIPPER_OPEN),
        ("above_pick",   [0.05, -0.20, 0.20], GRIPPER_OPEN),
        ("pick_down",    [0.05, -0.20, 0.10], GRIPPER_OPEN),
        ("grasp",        [0.05, -0.20, 0.10], GRIPPER_CLOSED),
        ("lift",         [0.05, -0.20, 0.20], GRIPPER_CLOSED),
        ("above_place",  [0.10, -0.10, 0.20], GRIPPER_CLOSED),
        ("place_down",   [0.10, -0.10, 0.10], GRIPPER_CLOSED),
        ("release",      [0.10, -0.10, 0.10], GRIPPER_OPEN),
        ("lift_away",    [0.10, -0.10, 0.20], GRIPPER_OPEN),
        ("return_home",  [0.00, -0.15, 0.20], GRIPPER_OPEN),
    ]

    print(f"\nSolving IK for {len(waypoint_defs)} waypoints...")
    q_prev = np.zeros(7)
    commands = []
    all_ok = True

    for i, (name, pos, grip) in enumerate(waypoint_defs):
        target = np.array(pos)
        q_sol = solve_ik(target, q_prev, target_z=down, w_ori=0.3)
        T = fk_right_arm(q_sol)
        err = np.linalg.norm(T[:3, 3] - target)
        z_dot = np.dot(T[:3, 2], down)
        ok = err < 0.005
        if not ok:
            all_ok = False

        grip_str = "open" if grip < -0.5 else "closed"
        print(f"  {'OK' if ok else 'WARN':4s} {name:15s} err={err:.4f} z_dot={z_dot:.2f} "
              f"tcp=({T[0,3]:+.3f},{T[1,3]:+.3f},{T[2,3]:+.3f}) grip={grip_str}")

        commands.append(make_timestep(float(i), q_sol.tolist(), grip))
        q_prev = q_sol.copy()

    if not all_ok:
        print("\nWARNING: Some waypoints have position errors > 5mm!")

    recording = {
        "version": 2,
        "metadata": {"arm": "right", "description": "Pick red block, place in green box"},
        "commands": commands,
    }

    out = "pick_place.json"
    with open(out, "w") as f:
        json.dump(recording, f, indent=2)

    print(f"\nWrote {out} ({len(commands)} waypoints)")


if __name__ == "__main__":
    main()
