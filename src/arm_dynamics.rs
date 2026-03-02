//! Rigid-body arm dynamics for gravity simulation.
//!
//! Provides forward kinematics, gravity torque computation, and a simple
//! physics step that integrates PD + feedforward torque against gravity.
//! Used by `fake-can-server --gravity` to simulate arm sag and PD tracking.

use std::f64::consts::FRAC_PI_2;

// ── Mat4 helpers (column-major 4×4) ─────────────────────────────────────────

type Mat4 = [f64; 16];

const MAT4_IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [0.0f64; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[col * 4 + k];
            }
            r[col * 4 + row] = sum;
        }
    }
    r
}

fn mat4_translation(x: f64, y: f64, z: f64) -> Mat4 {
    let mut m = MAT4_IDENTITY;
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

/// Rotation from roll (X), pitch (Y), yaw (Z) — URDF convention: R = Rz * Ry * Rx.
fn mat4_rotation_rpy(roll: f64, pitch: f64, yaw: f64) -> Mat4 {
    let (sr, cr) = roll.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    [
        cy * cp,
        sy * cp,
        -sp,
        0.0,
        cy * sp * sr - sy * cr,
        sy * sp * sr + cy * cr,
        cp * sr,
        0.0,
        cy * sp * cr + sy * sr,
        sy * sp * cr - cy * sr,
        cp * cr,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

/// Rotation about an arbitrary unit axis by angle (Rodrigues' formula).
fn mat4_rotation_axis_angle(axis: [f64; 3], angle: f64) -> Mat4 {
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    let [x, y, z] = axis;
    [
        t * x * x + c,
        t * y * x + z * s,
        t * z * x - y * s,
        0.0,
        t * x * y - z * s,
        t * y * y + c,
        t * z * y + x * s,
        0.0,
        t * x * z + y * s,
        t * y * z - x * s,
        t * z * z + c,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

/// Transform a point by a 4×4 matrix (homogeneous, w=1).
fn mat4_transform_point(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// Transform a direction by a 4×4 matrix (rotation only, no translation).
fn mat4_transform_dir(m: &Mat4, d: [f64; 3]) -> [f64; 3] {
    [
        m[0] * d[0] + m[4] * d[1] + m[8] * d[2],
        m[1] * d[0] + m[5] * d[1] + m[9] * d[2],
        m[2] * d[0] + m[6] * d[1] + m[10] * d[2],
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

// ── Arm model ───────────────────────────────────────────────────────────────

pub struct LinkDef {
    pub mass: f64,
    pub com_local: [f64; 3],
}

pub struct JointDef {
    pub origin_xyz: [f64; 3],
    pub origin_rpy: [f64; 3],
    pub axis: [f64; 3],
}

pub struct ArmModel {
    pub joints: [JointDef; 7],
    pub links: [LinkDef; 7],
}

// ── URDF constants (openarm_v10.urdf) ───────────────────────────────────────

pub fn left_arm_model() -> ArmModel {
    ArmModel {
        joints: [
            JointDef {
                origin_xyz: [0.0, 0.0, 0.0625],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [-0.0301, 0.0, 0.06],
                origin_rpy: [-FRAC_PI_2, 0.0, 0.0],
                axis: [-1.0, 0.0, 0.0],
            },
            JointDef {
                origin_xyz: [0.0301, 0.0, 0.06625],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [0.0, 0.0315, 0.15375],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 1.0, 0.0],
            },
            JointDef {
                origin_xyz: [0.0, -0.0315, 0.0955],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [0.0375, 0.0, 0.1205],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [1.0, 0.0, 0.0],
            },
            JointDef {
                origin_xyz: [-0.0375, 0.0, 0.0],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, -1.0, 0.0],
            },
        ],
        links: [
            LinkDef {
                mass: 1.142,
                com_local: [0.001, 0.0, 0.054],
            },
            LinkDef {
                mass: 0.278,
                com_local: [0.008, 0.0, 0.033],
            },
            LinkDef {
                mass: 1.074,
                com_local: [-0.002, 0.001, 0.088],
            },
            LinkDef {
                mass: 1.370,
                com_local: [-0.003, -0.030, 0.063],
            },
            LinkDef {
                mass: 0.551,
                com_local: [-0.003, 0.001, 0.043],
            },
            LinkDef {
                mass: 0.354,
                com_local: [-0.037, 0.0, 0.0],
            },
            LinkDef {
                mass: 0.550,
                com_local: [0.0, -0.018, 0.067],
            },
        ],
    }
}

pub fn right_arm_model() -> ArmModel {
    ArmModel {
        joints: [
            JointDef {
                origin_xyz: [0.0, 0.0, 0.0625],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [-0.0301, 0.0, 0.06],
                origin_rpy: [FRAC_PI_2, 0.0, 0.0],
                axis: [-1.0, 0.0, 0.0],
            },
            JointDef {
                origin_xyz: [0.0301, 0.0, 0.06625],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [0.0, 0.0315, 0.15375],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 1.0, 0.0],
            },
            JointDef {
                origin_xyz: [0.0, -0.0315, 0.0955],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 0.0, 1.0],
            },
            JointDef {
                origin_xyz: [0.0375, 0.0, 0.1205],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [1.0, 0.0, 0.0],
            },
            JointDef {
                origin_xyz: [-0.0375, 0.0, 0.0],
                origin_rpy: [0.0, 0.0, 0.0],
                axis: [0.0, 1.0, 0.0],
            },
        ],
        links: [
            LinkDef {
                mass: 1.142,
                com_local: [0.001, 0.0, 0.054],
            },
            LinkDef {
                mass: 0.278,
                com_local: [0.008, 0.0, 0.033],
            },
            LinkDef {
                mass: 1.074,
                com_local: [-0.002, 0.001, 0.088],
            },
            LinkDef {
                mass: 1.370,
                com_local: [-0.003, -0.030, 0.063],
            },
            LinkDef {
                mass: 0.551,
                com_local: [-0.003, 0.001, 0.043],
            },
            LinkDef {
                mass: 0.354,
                com_local: [-0.037, 0.0, 0.0],
            },
            LinkDef {
                mass: 0.550,
                com_local: [0.0, -0.018, 0.067],
            },
        ],
    }
}

// ── Forward kinematics ──────────────────────────────────────────────────────

/// Compute the world-frame transform for each joint.
/// Returns T[0..7] where T[i] is the transform of joint i's frame after rotation.
fn forward_kinematics(model: &ArmModel, angles: &[f64; 7]) -> [Mat4; 7] {
    let mut transforms = [MAT4_IDENTITY; 7];
    let mut t = MAT4_IDENTITY;
    for i in 0..7 {
        let j = &model.joints[i];
        let origin = mat4_mul(
            &mat4_translation(j.origin_xyz[0], j.origin_xyz[1], j.origin_xyz[2]),
            &mat4_rotation_rpy(j.origin_rpy[0], j.origin_rpy[1], j.origin_rpy[2]),
        );
        t = mat4_mul(&t, &origin);
        t = mat4_mul(&t, &mat4_rotation_axis_angle(j.axis, angles[i]));
        transforms[i] = t;
    }
    transforms
}

// ── Gravity torque ──────────────────────────────────────────────────────────

const GRAVITY: [f64; 3] = [0.0, 0.0, -9.81];

/// Compute gravity torque on each joint for a given configuration.
/// gravity_world = [0, 0, -9.81] (Z-up in arm base frame).
pub fn compute_gravity_torques(model: &ArmModel, angles: &[f64; 7]) -> [f64; 7] {
    let transforms = forward_kinematics(model, angles);

    // Precompute world-frame COM and gravity force for each link
    let mut com_world = [[0.0f64; 3]; 7];
    let mut force = [[0.0f64; 3]; 7]; // m * g
    for j in 0..7 {
        com_world[j] = mat4_transform_point(&transforms[j], model.links[j].com_local);
        let m = model.links[j].mass;
        force[j] = [m * GRAVITY[0], m * GRAVITY[1], m * GRAVITY[2]];
    }

    let mut tau_gravity = [0.0f64; 7];
    for i in 0..7 {
        let joint_pos = mat4_transform_point(&transforms[i], [0.0, 0.0, 0.0]);
        let axis_world = mat4_transform_dir(&transforms[i], model.joints[i].axis);

        let mut torque = 0.0;
        for (com_w, f) in com_world[i..].iter().zip(force[i..].iter()) {
            let r = [
                com_w[0] - joint_pos[0],
                com_w[1] - joint_pos[1],
                com_w[2] - joint_pos[2],
            ];
            let moment = cross(r, *f);
            torque += dot(axis_world, moment);
        }
        tau_gravity[i] = torque;
    }
    tau_gravity
}

// ── Approximate effective inertia ───────────────────────────────────────────

/// Compute approximate effective rotational inertia for each joint.
/// I_eff[i] = Σ_{j≥i} m_j * r_j² where r_j is the distance from joint i axis
/// to link j's COM, projected perpendicular to the joint axis.
fn compute_effective_inertia(model: &ArmModel, angles: &[f64; 7]) -> [f64; 7] {
    let transforms = forward_kinematics(model, angles);

    let mut com_world = [[0.0f64; 3]; 7];
    for j in 0..7 {
        com_world[j] = mat4_transform_point(&transforms[j], model.links[j].com_local);
    }

    let mut inertia = [0.0f64; 7];
    for i in 0..7 {
        let joint_pos = mat4_transform_point(&transforms[i], [0.0, 0.0, 0.0]);
        let axis_world = mat4_transform_dir(&transforms[i], model.joints[i].axis);

        let mut i_eff = 0.0;
        for (com_w, link) in com_world[i..].iter().zip(model.links[i..].iter()) {
            let r = [
                com_w[0] - joint_pos[0],
                com_w[1] - joint_pos[1],
                com_w[2] - joint_pos[2],
            ];
            // Distance perpendicular to axis: |r|² - (r·axis)²
            let r_dot_axis = dot(r, axis_world);
            let r_sq = dot(r, r);
            let perp_sq = (r_sq - r_dot_axis * r_dot_axis).max(0.0);
            i_eff += link.mass * perp_sq;
        }
        // Floor includes motor rotor inertia (~0.01 kg·m² for Damiao actuators)
        inertia[i] = i_eff.max(0.01);
    }
    inertia
}

// ── Physics step ────────────────────────────────────────────────────────────

// Damiao motor velocity limit (rad/s)
const VEL_LIMIT: f64 = 45.0;
// Damiao motor torque limit (Nm)
const TAU_LIMIT: f64 = 18.0;

/// Single physics step. Integrates PD + feedforward torque against gravity.
///
/// Returns the motor torque per joint (for reporting in CAN responses).
#[allow(clippy::too_many_arguments)]
pub fn physics_step(
    model: &ArmModel,
    pos: &mut [f64; 7],
    vel: &mut [f64; 7],
    p_des: &[f64; 7],
    v_des: &[f64; 7],
    kp: &[f64; 7],
    kd: &[f64; 7],
    tau_ff: &[f64; 7],
    dt: f64,
    damping: f64,
) -> [f64; 7] {
    let tau_gravity = compute_gravity_torques(model, pos);
    let i_eff = compute_effective_inertia(model, pos);

    let mut tau_motor = [0.0f64; 7];
    for i in 0..7 {
        let tau_pd = kp[i] * (p_des[i] - pos[i]) + kd[i] * (v_des[i] - vel[i]) + tau_ff[i];
        // Clamp motor torque to physical limits
        tau_motor[i] = tau_pd.clamp(-TAU_LIMIT, TAU_LIMIT);

        let accel = (tau_motor[i] - tau_gravity[i] - damping * vel[i]) / i_eff[i];
        vel[i] += accel * dt;
        // Clamp velocity to motor speed limits
        vel[i] = vel[i].clamp(-VEL_LIMIT, VEL_LIMIT);
        pos[i] += vel[i] * dt;
    }
    tau_motor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_angles_gravity_torques() {
        let model = left_arm_model();
        let angles = [0.0; 7];
        let tau = compute_gravity_torques(&model, &angles);
        // At zero angles the arm points straight up (Z), so gravity torques
        // on J1 (Z-axis) should be near zero (gravity is along the axis).
        assert!(
            tau[0].abs() < 0.01,
            "J1 gravity torque should be ~0, got {}",
            tau[0]
        );
        // Total gravity torques should be finite
        for (i, t) in tau.iter().enumerate() {
            assert!(t.is_finite(), "J{} gravity torque is not finite", i + 1);
        }
    }

    #[test]
    fn test_physics_step_stays_finite() {
        let model = left_arm_model();
        let mut pos = [0.0; 7];
        let mut vel = [0.0; 7];
        let p_des = [0.1; 7];
        let v_des = [0.0; 7];
        let kp = [200.0; 7];
        let kd = [20.0; 7];
        let dt = 0.001;

        for _ in 0..5000 {
            let tau_ff = compute_gravity_torques(&model, &pos);
            physics_step(
                &model, &mut pos, &mut vel, &p_des, &v_des, &kp, &kd, &tau_ff, dt, 2.0,
            );
        }
        for i in 0..7 {
            assert!(
                pos[i].is_finite(),
                "J{} pos is not finite: {}",
                i + 1,
                pos[i]
            );
            assert!(
                (pos[i] - p_des[i]).abs() < 0.1,
                "J{} didn't converge: pos={:.4}, target={}",
                i + 1,
                pos[i],
                p_des[i]
            );
        }
    }

    #[test]
    fn test_left_right_model_differ_at_j2_and_j7() {
        let left = left_arm_model();
        let right = right_arm_model();
        // J2 rpy differs
        assert_ne!(left.joints[1].origin_rpy[0], right.joints[1].origin_rpy[0]);
        // J7 axis differs
        assert_ne!(left.joints[6].axis[1], right.joints[6].axis[1]);
        // Masses are the same
        for i in 0..7 {
            assert_eq!(left.links[i].mass, right.links[i].mass);
        }
    }
}
