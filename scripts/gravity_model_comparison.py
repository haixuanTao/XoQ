#!/usr/bin/env python3
"""Compare Pinocchio gravity torques with real hardware data from baguette right arm.

Real data from CSV playback log (2026-03-04), converged EMA tau_ff values
during hold periods at t≈20.93s.
"""

import numpy as np
import pinocchio as pin

URDF_PATH = "js/examples/assets/openarm_v10.urdf"

# Load the full two-arm model
model = pin.buildModelFromUrdf(URDF_PATH)
data = model.createData()

print(f"Model: {model.name}")
print(f"  nq={model.nq}, nv={model.nv}, njoints={model.njoints}")
print(f"  Gravity: {model.gravity}")
print()

# Print joint names and indices
print("Joints:")
for i in range(model.njoints):
    print(f"  [{i}] {model.names[i]}")
print()

# Real hardware data from baguette right arm at t≈20.93s
# Actual joint angles (radians) — what the arm was physically at
real_angles = {
    "R_J1": -0.620851,
    "R_J2":  0.235561,
    "R_J3": -0.325971,
    "R_J4":  0.796330,
    "R_J5":  0.206569,
    "R_J6":  0.787556,
    "R_J7":  1.487564,
}

# Converged tau_ff from EMA gravity compensation on real hardware
# Only J2 and J6 are fully converged (stable >5s in hold state)
real_tau_ff = {
    "R_J1": ( 0.1478, False),  # partial — always in motion
    "R_J2": ( 0.5362, True),   # converged
    "R_J3": (-0.2269, False),  # partial
    "R_J4": (-0.0619, False),  # partial
    "R_J5": ( 0.0000, False),  # partial, near zero
    "R_J6": ( 1.5365, True),   # converged
    "R_J7": (-0.0516, False),  # partial
}

# Build configuration vector (all zeros, then set right arm joints)
q = pin.neutral(model)
for i in range(1, model.njoints):
    name = model.names[i]
    if name in real_angles:
        # Find the joint's index in q
        joint = model.joints[i]
        idx_q = joint.idx_q
        q[idx_q] = real_angles[name]

# Compute gravity torques
tau_g = pin.computeGeneralizedGravity(model, data, q)

# Extract right arm results
print(f"{'Joint':>8} {'Pinocchio':>10} {'Real':>10} {'Ratio':>8} {'Status':>10}")
print("-" * 52)
for name in ["R_J1", "R_J2", "R_J3", "R_J4", "R_J5", "R_J6", "R_J7"]:
    # Find joint index
    for i in range(1, model.njoints):
        if model.names[i] == name:
            joint = model.joints[i]
            idx_v = joint.idx_v
            model_val = tau_g[idx_v]
            real_val, converged = real_tau_ff[name]
            ratio = model_val / real_val if abs(real_val) > 0.01 else float('nan')
            status = "converged" if converged else "partial"
            print(f"{name:>8} {model_val:>+10.4f} {real_val:>+10.4f} {ratio:>8.2f} {status:>10}")
            break

# Our hand-coded arm_dynamics.rs model values (for comparison)
rust_model = {
    "R_J1":  0.0000,
    "R_J2": -9.9279,
    "R_J3": -2.4701,
    "R_J4":  1.3132,
    "R_J5": -0.3232,
    "R_J6": -0.0520,
    "R_J7": -0.2598,
}

print(f"\n{'Joint':>8} {'Pinocchio':>10} {'Rust model':>10} {'Real':>10} {'Pin/Real':>8} {'Rust/Real':>9}")
print("-" * 65)
for name in ["R_J1", "R_J2", "R_J3", "R_J4", "R_J5", "R_J6", "R_J7"]:
    for i in range(1, model.njoints):
        if model.names[i] == name:
            joint = model.joints[i]
            idx_v = joint.idx_v
            pin_val = tau_g[idx_v]
            rust_val = rust_model[name]
            real_val, converged = real_tau_ff[name]
            pin_ratio = pin_val / real_val if abs(real_val) > 0.01 else float('nan')
            rust_ratio = rust_val / real_val if abs(real_val) > 0.01 else float('nan')
            marker = " <-- converged" if converged else ""
            print(f"{name:>8} {pin_val:>+10.4f} {rust_val:>+10.4f} {real_val:>+10.4f} {pin_ratio:>+8.2f} {rust_ratio:>+9.2f}{marker}")
            break
