//! OpenArm interactive teleop
//!
//! Cartesian (WASD/arrows + rotation) and joint-space keyboard control with smooth
//! interpolation and safety monitoring. Cartesian mode uses differential IK (numerical
//! Jacobian + damped least squares). Toggle modes with `m`.
//! 50Hz control loop: poll keyboard → ramp commanded positions → send CAN → read responses.
//!
//! Usage:
//!   openarm_teleop [waypoints.json] [<arm-name> <server-id> ...]
//!
//! Examples:
//!   # Teleop default champagne arms
//!   openarm_teleop
//!
//!   # Teleop with waypoints for goto
//!   openarm_teleop waypoints.json
//!
//!   # Teleop specific arm
//!   openarm_teleop right 9280c388...

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use std::collections::{HashMap, VecDeque};
use std::f64::consts::FRAC_PI_2;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xoq::socketcan;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];
const QUERY_CMD: [u8; 8] = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00];

const TICK_MS: u64 = 20; // 50Hz control loop
const MAX_STEP_PER_TICK: f64 = 3.0 * 0.020; // 3.0 rad/s * 20ms = 0.06 rad per tick
const DASHBOARD_INTERVAL_MS: u64 = 100; // 10Hz dashboard refresh

const POS_MIN: f64 = -12.5;
const POS_MAX: f64 = 12.5;
const VEL_MIN: f64 = -45.0;
const VEL_MAX: f64 = 45.0;
const TAU_MIN: f64 = -18.0;
const TAU_MAX: f64 = 18.0;

const DEFAULT_KP: f64 = 30.0;
const DEFAULT_KD: f64 = 1.0;

const EE_ERROR_THRESHOLD: f64 = 0.05;
const EE_ESTOP_THRESHOLD: f64 = 0.30;
const MIN_JOINTS_FOR_FK: usize = 5;
const SAFETY_LAG_MS: u64 = 200;
const CONVERGENCE_MM: f64 = 0.001;
const CONVERGENCE_COUNT: usize = 10;

// Joint limits in radians: [min, max] for J1-J7 + gripper
const JOINT_LIMITS: [[f64; 2]; 8] = [
    [-1.396, 3.491], // J1: -80° to +200°
    [-0.175, 3.316], // J2: -10° to +190°
    [-1.571, 1.571], // J3: -90° to +90°
    [-0.175, 2.967], // J4: -10° to +170°
    [-1.571, 1.571], // J5: -90° to +90°
    [-1.571, 1.571], // J6: -90° to +90°
    [-1.571, 1.571], // J7: -90° to +90°
    [-0.175, 1.745], // Grip: -10° to +100°
];

// Lift sequence parameters: use Cartesian IK to move EE straight up.
const LIFT_HEIGHT_M: f64 = 0.15; // lift 150mm straight up
const LIFT_STEP_M: f64 = 0.005; // 5mm per IK step

const STEP_SIZES_DEG: [f64; 3] = [1.0, 5.0, 10.0];

// Cartesian control step sizes
const CART_POS_STEPS: [f64; 3] = [0.0005, 0.001, 0.005]; // meters: 0.5mm, 1mm, 5mm
const CART_ROT_STEPS_DEG: [f64; 3] = [0.5, 1.0, 5.0]; // degrees

const DLS_LAMBDA: f64 = 0.001;
const JAC_EPS: f64 = 1e-6;

#[derive(Clone, Copy, PartialEq)]
enum ControlMode {
    Cartesian,
    Joint,
}

// ---------------------------------------------------------------------------
// Damiao protocol
// ---------------------------------------------------------------------------

fn decode_damiao_cmd(data: &[u8]) -> (f64, f64, f64, f64, f64) {
    let pos_raw = ((data[0] as u16) << 8) | data[1] as u16;
    let vel_raw = ((data[2] as u16) << 4) | ((data[3] as u16) >> 4);
    let kp_raw = (((data[3] & 0x0F) as u16) << 8) | data[4] as u16;
    let kd_raw = ((data[5] as u16) << 4) | ((data[6] as u16) >> 4);
    let tau_raw = (((data[6] & 0x0F) as u16) << 8) | data[7] as u16;

    (
        pos_raw as f64 / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN,
        vel_raw as f64 / 4095.0 * (VEL_MAX - VEL_MIN) + VEL_MIN,
        kp_raw as f64 / 4095.0 * 500.0,
        kd_raw as f64 / 4095.0 * 5.0,
        tau_raw as f64 / 4095.0 * (TAU_MAX - TAU_MIN) + TAU_MIN,
    )
}

fn encode_damiao_cmd(pos: f64, vel: f64, kp: f64, kd: f64, tau: f64) -> [u8; 8] {
    let pos_raw = (((pos - POS_MIN) / (POS_MAX - POS_MIN)) * 65535.0).clamp(0.0, 65535.0) as u16;
    let vel_raw = (((vel - VEL_MIN) / (VEL_MAX - VEL_MIN)) * 4095.0).clamp(0.0, 4095.0) as u16;
    let kp_raw = ((kp / 500.0) * 4095.0).clamp(0.0, 4095.0) as u16;
    let kd_raw = ((kd / 5.0) * 4095.0).clamp(0.0, 4095.0) as u16;
    let tau_raw = (((tau - TAU_MIN) / (TAU_MAX - TAU_MIN)) * 4095.0).clamp(0.0, 4095.0) as u16;
    [
        (pos_raw >> 8) as u8,
        (pos_raw & 0xFF) as u8,
        (vel_raw >> 4) as u8,
        (((vel_raw & 0x0F) << 4) | ((kp_raw >> 8) & 0x0F)) as u8,
        (kp_raw & 0xFF) as u8,
        (kd_raw >> 4) as u8,
        (((kd_raw & 0x0F) << 4) | ((tau_raw >> 8) & 0x0F)) as u8,
        (tau_raw & 0xFF) as u8,
    ]
}

fn decode_response_pos(data: &[u8]) -> f64 {
    let pos_raw = ((data[1] as u16) << 8) | data[2] as u16;
    pos_raw as f64 / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN
}

// ---------------------------------------------------------------------------
// Motor helpers
// ---------------------------------------------------------------------------

fn query_motor_positions(socket: &mut socketcan::RemoteCanSocket) -> Result<HashMap<u32, f64>> {
    for motor_id in 0x01..=0x08u32 {
        let frame = socketcan::CanFrame::new(motor_id, &QUERY_CMD)?;
        socket.write_frame(&frame)?;
    }
    let mut positions = HashMap::new();
    for _ in 0..8 {
        match socket.read_frame()? {
            Some(frame) => {
                let can_id = frame.id();
                if (0x11..=0x18).contains(&can_id) && frame.data().len() >= 8 {
                    let cmd_id = can_id - 0x10;
                    positions.insert(cmd_id, decode_response_pos(frame.data()));
                }
            }
            None => break,
        }
    }
    Ok(positions)
}

fn read_response_positions(socket: &mut socketcan::RemoteCanSocket) -> HashMap<u32, f64> {
    let mut positions = HashMap::new();
    while let Ok(Some(frame)) = socket.read_frame() {
        let can_id = frame.id();
        if (0x11..=0x18).contains(&can_id) && frame.data().len() >= 8 {
            positions.insert(can_id - 0x10, decode_response_pos(frame.data()));
        }
    }
    positions
}

// ---------------------------------------------------------------------------
// Forward kinematics
// ---------------------------------------------------------------------------

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

struct JointDef {
    origin_xyz: [f64; 3],
    origin_rpy: [f64; 3],
    axis: [f64; 3],
}

const LEFT_ARM_CHAIN: [JointDef; 7] = [
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
];

const LEFT_ARM_EE_OFFSET: [f64; 3] = [1e-6, 0.0205, 0.0];

const RIGHT_ARM_CHAIN: [JointDef; 7] = [
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
];

const RIGHT_ARM_EE_OFFSET: [f64; 3] = [1e-6, 0.0205, 0.0];

fn ee_position(angles: &[f64; 7], chain: &[JointDef; 7], ee_offset: &[f64; 3]) -> [f64; 3] {
    let mut t = MAT4_IDENTITY;
    for i in 0..7 {
        let j = &chain[i];
        let origin = mat4_mul(
            &mat4_translation(j.origin_xyz[0], j.origin_xyz[1], j.origin_xyz[2]),
            &mat4_rotation_rpy(j.origin_rpy[0], j.origin_rpy[1], j.origin_rpy[2]),
        );
        t = mat4_mul(&t, &origin);
        t = mat4_mul(&t, &mat4_rotation_axis_angle(j.axis, angles[i]));
    }
    let ee = mat4_translation(ee_offset[0], ee_offset[1], ee_offset[2]);
    t = mat4_mul(&t, &ee);
    [t[12], t[13], t[14]]
}

fn ee_pose(angles: &[f64; 7], chain: &[JointDef; 7], ee_offset: &[f64; 3]) -> Mat4 {
    let mut t = MAT4_IDENTITY;
    for i in 0..7 {
        let j = &chain[i];
        let origin = mat4_mul(
            &mat4_translation(j.origin_xyz[0], j.origin_xyz[1], j.origin_xyz[2]),
            &mat4_rotation_rpy(j.origin_rpy[0], j.origin_rpy[1], j.origin_rpy[2]),
        );
        t = mat4_mul(&t, &origin);
        t = mat4_mul(&t, &mat4_rotation_axis_angle(j.axis, angles[i]));
    }
    mat4_mul(
        &t,
        &mat4_translation(ee_offset[0], ee_offset[1], ee_offset[2]),
    )
}

/// Compute R_a * R_b^T for the 3x3 rotation parts of two column-major Mat4s.
/// Returns [[row][col]] indexing.
fn mat4_rot_mul_rt(a: &Mat4, b: &Mat4) -> [[f64; 3]; 3] {
    let mut r = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                r[i][j] += a[k * 4 + i] * b[k * 4 + j];
            }
        }
    }
    r
}

/// SO(3) logarithm: extract rotation vector from 3x3 rotation matrix.
fn rotation_log(r: &[[f64; 3]; 3]) -> [f64; 3] {
    let trace = r[0][0] + r[1][1] + r[2][2];
    let cos_angle = ((trace - 1.0) / 2.0).clamp(-1.0, 1.0);
    let angle = cos_angle.acos();
    if angle < 1e-10 {
        return [0.0, 0.0, 0.0];
    }
    let s = 2.0 * angle.sin();
    [
        (r[2][1] - r[1][2]) / s * angle,
        (r[0][2] - r[2][0]) / s * angle,
        (r[1][0] - r[0][1]) / s * angle,
    ]
}

/// 6x7 numerical Jacobian via forward finite differences.
/// Rows 0-2: position (xyz), rows 3-5: orientation (rotation vector).
fn numerical_jacobian(
    angles: &[f64; 7],
    chain: &[JointDef; 7],
    ee_offset: &[f64; 3],
) -> [[f64; 7]; 6] {
    let t0 = ee_pose(angles, chain, ee_offset);
    let mut jac = [[0.0f64; 7]; 6];
    for j in 0..7 {
        let mut a = *angles;
        a[j] += JAC_EPS;
        let t1 = ee_pose(&a, chain, ee_offset);
        // Position columns
        jac[0][j] = (t1[12] - t0[12]) / JAC_EPS;
        jac[1][j] = (t1[13] - t0[13]) / JAC_EPS;
        jac[2][j] = (t1[14] - t0[14]) / JAC_EPS;
        // Rotation columns: log(R1 * R0^T)
        let r_rel = mat4_rot_mul_rt(&t1, &t0);
        let omega = rotation_log(&r_rel);
        jac[3][j] = omega[0] / JAC_EPS;
        jac[4][j] = omega[1] / JAC_EPS;
        jac[5][j] = omega[2] / JAC_EPS;
    }
    jac
}

/// Damped least squares: dq = J^T (J J^T + λI)^{-1} dx
/// J is 6x7, dx is 6-vector, returns 7-vector of joint deltas.
#[allow(clippy::needless_range_loop)]
fn dls_solve(jac: &[[f64; 7]; 6], dx: &[f64; 6], lambda: f64) -> [f64; 7] {
    // A = J * J^T + lambda * I  (6x6)
    let mut a = [[0.0f64; 7]; 6]; // augmented [6x6 | 6x1]
    for i in 0..6 {
        for j in 0..6 {
            let mut sum = 0.0;
            for k in 0..7 {
                sum += jac[i][k] * jac[j][k];
            }
            a[i][j] = sum;
            if i == j {
                a[i][j] += lambda;
            }
        }
        a[i][6] = dx[i]; // RHS
    }

    // Gaussian elimination with partial pivoting (6x6)
    for col in 0..6 {
        // Find pivot
        let mut max_val = a[col][col].abs();
        let mut max_row = col;
        for row in (col + 1)..6 {
            if a[row][col].abs() > max_val {
                max_val = a[row][col].abs();
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            continue; // singular, skip
        }
        if max_row != col {
            a.swap(col, max_row);
        }
        let pivot = a[col][col];
        for row in (col + 1)..6 {
            let factor = a[row][col] / pivot;
            for c in col..7 {
                a[row][c] -= factor * a[col][c];
            }
        }
    }

    // Back substitution → y (stored in a[i][6])
    let mut y = [0.0f64; 6];
    for i in (0..6).rev() {
        let mut sum = a[i][6];
        for j in (i + 1)..6 {
            sum -= a[i][j] * y[j];
        }
        if a[i][i].abs() > 1e-12 {
            y[i] = sum / a[i][i];
        }
    }

    // dq = J^T * y
    let mut dq = [0.0f64; 7];
    for j in 0..7 {
        for i in 0..6 {
            dq[j] += jac[i][j] * y[i];
        }
    }
    dq
}

/// Apply a Cartesian delta to arm joint targets via iterative differential IK.
/// dx is [pos_x, pos_y, pos_z, rot_x, rot_y, rot_z].
/// Iterates up to 10 times to converge on the requested EE displacement.
fn apply_cartesian_delta(
    arm: &mut ArmState,
    dx: &[f64; 6],
    chain: &[JointDef; 7],
    ee_offset: &[f64; 3],
) {
    let mut angles = [0.0f64; 7];
    angles.copy_from_slice(&arm.target_pos[..7]);

    // Compute target pose from current + delta
    let pose_start = ee_pose(&angles, chain, ee_offset);
    let target_xyz = [
        pose_start[12] + dx[0],
        pose_start[13] + dx[1],
        pose_start[14] + dx[2],
    ];

    // Target rotation: R_target = R_delta * R_start
    let omega_norm = (dx[3] * dx[3] + dx[4] * dx[4] + dx[5] * dx[5]).sqrt();
    let r_delta = if omega_norm > 1e-10 {
        let axis = [dx[3] / omega_norm, dx[4] / omega_norm, dx[5] / omega_norm];
        mat4_rotation_axis_angle(axis, omega_norm)
    } else {
        MAT4_IDENTITY
    };
    let target_rot = mat4_mul(&r_delta, &pose_start);

    for _ in 0..10 {
        let pose_cur = ee_pose(&angles, chain, ee_offset);

        // Position error
        let ep = [
            target_xyz[0] - pose_cur[12],
            target_xyz[1] - pose_cur[13],
            target_xyz[2] - pose_cur[14],
        ];

        // Rotation error: log(R_target * R_cur^T)
        let r_err = mat4_rot_mul_rt(&target_rot, &pose_cur);
        let er = rotation_log(&r_err);

        let err = [ep[0], ep[1], ep[2], er[0], er[1], er[2]];
        let err_norm = err.iter().map(|e| e * e).sum::<f64>().sqrt();
        if err_norm < 1e-6 {
            break;
        }

        let jac = numerical_jacobian(&angles, chain, ee_offset);
        let dq = dls_solve(&jac, &err, DLS_LAMBDA);
        for i in 0..7 {
            angles[i] = (angles[i] + dq[i]).clamp(JOINT_LIMITS[i][0], JOINT_LIMITS[i][1]);
        }
    }

    arm.target_pos[..7].copy_from_slice(&angles);
}

/// Extract Roll-Pitch-Yaw (ZYX Euler) from column-major Mat4 rotation part.
fn mat4_to_rpy(m: &Mat4) -> [f64; 3] {
    // R[row][col] = m[col*4 + row]
    let r20 = m[2]; // R[2][0]
    let r00 = m[0]; // R[0][0]
    let r10 = m[1]; // R[1][0]
    let r21 = m[6]; // R[2][1]
    let r22 = m[10]; // R[2][2]
    let pitch = (-r20).atan2((r00 * r00 + r10 * r10).sqrt());
    let roll = r21.atan2(r22);
    let yaw = r10.atan2(r00);
    [roll, pitch, yaw]
}

fn arm_chain(arm: &ArmState) -> (&'static [JointDef; 7], &'static [f64; 3]) {
    if arm.name.contains("left") {
        (&LEFT_ARM_CHAIN, &LEFT_ARM_EE_OFFSET)
    } else {
        (&RIGHT_ARM_CHAIN, &RIGHT_ARM_EE_OFFSET)
    }
}

fn compute_cmd_ee(commanded: &HashMap<u32, f64>, arm_name: &str) -> Option<[f64; 3]> {
    let mut angles = [0.0f64; 7];
    let mut count = 0;
    for i in 0..7 {
        if let Some(&pos) = commanded.get(&(i as u32 + 1)) {
            angles[i] = pos;
            count += 1;
        }
    }
    if count < MIN_JOINTS_FOR_FK {
        return None;
    }
    let (chain, ee_offset) = if arm_name.contains("left") {
        (&LEFT_ARM_CHAIN, &LEFT_ARM_EE_OFFSET)
    } else {
        (&RIGHT_ARM_CHAIN, &RIGHT_ARM_EE_OFFSET)
    };
    Some(ee_position(&angles, chain, ee_offset))
}

fn compute_act_ee(
    commanded: &HashMap<u32, f64>,
    actual: &HashMap<u32, f64>,
    arm_name: &str,
) -> Option<[f64; 3]> {
    let mut angles = [0.0f64; 7];
    let mut joint_count = 0;
    for i in 0..7 {
        let id = i as u32 + 1;
        if let Some(&a) = actual.get(&id) {
            angles[i] = a;
            joint_count += 1;
        } else if let Some(&c) = commanded.get(&id) {
            angles[i] = c;
        }
    }
    if joint_count < MIN_JOINTS_FOR_FK {
        return None;
    }
    let (chain, ee_offset) = if arm_name.contains("left") {
        (&LEFT_ARM_CHAIN, &LEFT_ARM_EE_OFFSET)
    } else {
        (&RIGHT_ARM_CHAIN, &RIGHT_ARM_EE_OFFSET)
    };
    Some(ee_position(&angles, chain, ee_offset))
}

// ---------------------------------------------------------------------------
// Safety monitor (same as playback)
// ---------------------------------------------------------------------------

struct SafetyMonitor {
    cmd_history: HashMap<String, VecDeque<(Instant, [f64; 3])>>,
    violation_start: HashMap<String, Option<Instant>>,
    last_actual_ee: HashMap<String, [f64; 3]>,
    stable_count: HashMap<String, usize>,
}

impl SafetyMonitor {
    fn new() -> Self {
        SafetyMonitor {
            cmd_history: HashMap::new(),
            violation_start: HashMap::new(),
            last_actual_ee: HashMap::new(),
            stable_count: HashMap::new(),
        }
    }

    fn check(
        &mut self,
        commanded: &HashMap<u32, f64>,
        actual: &HashMap<u32, f64>,
        arm_name: &str,
    ) -> Result<bool> {
        let now = Instant::now();

        if let Some(ee) = compute_cmd_ee(commanded, arm_name) {
            let history = self.cmd_history.entry(arm_name.to_string()).or_default();
            history.push_back((now, ee));
            let cutoff = now - Duration::from_secs(4);
            while history.front().map_or(false, |(t, _)| *t < cutoff) {
                history.pop_front();
            }
        }

        let lag = Duration::from_millis(SAFETY_LAG_MS);
        let ref_ee = self.cmd_history.get(arm_name).and_then(|h| {
            h.iter()
                .rev()
                .find(|(t, _)| now.duration_since(*t) >= lag)
                .map(|(_, ee)| *ee)
        });

        let ref_ee = match ref_ee {
            Some(ee) => ee,
            None => return Ok(false),
        };

        let act_ee = match compute_act_ee(commanded, actual, arm_name) {
            Some(ee) => ee,
            None => return Ok(false),
        };

        let dx = act_ee[0] - ref_ee[0];
        let dy = act_ee[1] - ref_ee[1];
        let dz = act_ee[2] - ref_ee[2];
        let error = (dx * dx + dy * dy + dz * dz).sqrt();

        if error > EE_ESTOP_THRESHOLD {
            anyhow::bail!(
                "SAFETY: {} end-effector error {:.1}cm exceeds hard limit {:.1}cm — EMERGENCY STOP",
                arm_name,
                error * 100.0,
                EE_ESTOP_THRESHOLD * 100.0,
            );
        }

        if error > EE_ERROR_THRESHOLD {
            let stable = self.stable_count.entry(arm_name.to_string()).or_insert(0);
            if let Some(prev) = self.last_actual_ee.get(arm_name) {
                let mx = act_ee[0] - prev[0];
                let my = act_ee[1] - prev[1];
                let mz = act_ee[2] - prev[2];
                let movement = (mx * mx + my * my + mz * mz).sqrt();
                if movement < CONVERGENCE_MM {
                    *stable += 1;
                    if *stable >= CONVERGENCE_COUNT {
                        *stable = 0;
                        self.violation_start.insert(arm_name.to_string(), None);
                        return Ok(false);
                    }
                } else {
                    *stable = 0;
                }
            }
            self.last_actual_ee.insert(arm_name.to_string(), act_ee);

            let v_start = self
                .violation_start
                .entry(arm_name.to_string())
                .or_insert(None);
            if v_start.is_none() {
                *v_start = Some(now);
            }
            return Ok(true);
        }

        self.violation_start.insert(arm_name.to_string(), None);
        self.stable_count.insert(arm_name.to_string(), 0);
        self.last_actual_ee.remove(arm_name);
        Ok(false)
    }
}

fn emergency_stop(arms: &mut [ArmState]) {
    for arm in arms.iter_mut() {
        for motor_id in 0x01..=0x08u32 {
            if let Ok(frame) = socketcan::CanFrame::new(motor_id, &DISABLE_MIT) {
                let _ = arm.socket.write_frame(&frame);
                let _ = arm.socket.read_frame();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JSON waypoint parsing (v2 format)
// ---------------------------------------------------------------------------

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in input.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' || b == b' ' {
            continue;
        }
        let val = TABLE
            .iter()
            .position(|&c| c == b)
            .ok_or_else(|| anyhow::anyhow!("invalid base64 char: {}", b as char))?
            as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn extract_string_field(s: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let quote1 = after.find('"')?;
    let rest = &after[quote1 + 1..];
    let quote2 = rest.find('"')?;
    Some(rest[..quote2].to_string())
}

fn find_matching_bracket(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        if ch == open {
            depth += 1;
        }
        if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn iter_objects(s: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0;
    let mut obj_start = None;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start {
                        results.push(&s[start..=i]);
                    }
                }
            }
            _ => {}
        }
    }
    results
}

/// A waypoint: per-motor target positions + kp/kd from recording.
struct Waypoint {
    /// motor_id -> (position_rad, kp, kd)
    targets: HashMap<u32, (f64, f64, f64)>,
}

/// Parse waypoints from a v2 JSON recording file.
/// Returns (arm_name_from_file, waypoints).
fn parse_waypoints(path: &str) -> Result<(String, Vec<Waypoint>)> {
    let content = std::fs::read_to_string(path)?;
    let content = content.trim();

    if !content.starts_with('{') {
        anyhow::bail!("Teleop waypoints only support v2 JSON format");
    }

    let arm_name = extract_string_field(content, "arm").unwrap_or_else(|| "right".to_string());

    let commands_idx = content
        .find("\"commands\"")
        .ok_or_else(|| anyhow::anyhow!("v2: missing 'commands' field"))?;
    let after = &content[commands_idx..];
    let arr_start = after
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("v2: missing commands array"))?;
    let arr_content = &after[arr_start..];
    let arr_end = find_matching_bracket(arr_content, '[', ']')
        .ok_or_else(|| anyhow::anyhow!("v2: unterminated commands array"))?;
    let arr_inner = &arr_content[1..arr_end];

    let mut waypoints = Vec::new();
    for obj_str in iter_objects(arr_inner) {
        let frames_idx = match obj_str.find("\"frames\"") {
            Some(i) => i,
            None => continue,
        };
        let after = &obj_str[frames_idx..];
        let arr_start = after.find('[').unwrap_or(0);
        let arr_end = after.rfind(']').unwrap_or(after.len());
        let arr_inner = &after[arr_start + 1..arr_end];

        let mut targets = HashMap::new();
        for frame_str in iter_objects(arr_inner) {
            let id_str = match extract_string_field(frame_str, "id") {
                Some(s) => s,
                None => continue,
            };
            let data_b64 = match extract_string_field(frame_str, "data") {
                Some(s) => s,
                None => continue,
            };

            let raw = base64_decode(&data_b64)?;
            let (can_id, payload) = if raw.len() == 72 {
                let can_id = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) & 0x1FFFFFFF;
                let len = (raw[4] as usize).min(64);
                (can_id, raw[8..8 + len].to_vec())
            } else {
                let can_id = u32::from_str_radix(id_str.trim_start_matches("0x"), 16)?;
                (can_id, raw)
            };

            if payload.len() == 8 && (0x01..=0x08).contains(&can_id) {
                let (pos, _vel, kp, kd, _tau) = decode_damiao_cmd(&payload);
                targets.insert(can_id, (pos, kp, kd));
            }
        }

        if !targets.is_empty() {
            waypoints.push(Waypoint { targets });
        }
    }

    Ok((arm_name, waypoints))
}

// ---------------------------------------------------------------------------
// RawModeGuard — ensures terminal is always restored
// ---------------------------------------------------------------------------

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        // Hide cursor and switch to alternate screen
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = crossterm::execute!(
            stdout,
            crossterm::cursor::Show,
            terminal::LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

// ---------------------------------------------------------------------------
// Arm state
// ---------------------------------------------------------------------------

struct ArmState {
    name: String,
    socket: socketcan::RemoteCanSocket,
    target_pos: [f64; 8],    // where the user wants each joint
    commanded_pos: [f64; 8], // what we're actually sending (ramps toward target)
    actual_pos: [f64; 8],    // last read from motor responses
    actual_valid: [bool; 8], // have we received a response for this joint?
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Parse args: optional json file, then arm-name server-id pairs
    let mut json_path: Option<String> = None;
    let mut rest_args: Vec<String> = Vec::new();

    for arg in args.iter().skip(1) {
        if arg.ends_with(".json") {
            json_path = Some(arg.clone());
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: openarm_teleop [waypoints.json] [<arm-name> <server-id> ...]");
            println!();
            println!("Cartesian mode (default):");
            println!("  Z/Up      X forward");
            println!("  S/Down    X back");
            println!("  A/Left    Y left");
            println!("  D/Right   Y right");
            println!("  E         Z up");
            println!("  Q         Z down");
            println!("  U/O       Roll +/-");
            println!("  I/K       Pitch +/-");
            println!("  J/L       Yaw +/-");
            println!();
            println!("Joint mode (toggle with m):");
            println!("  1-7       Select joint J1-J7");
            println!("  8/g       Select gripper");
            println!("  Tab       Cycle to next joint");
            println!("  Up/k      Move selected joint positive");
            println!("  Down/j    Move selected joint negative");
            println!("  Left/Right  Switch between arms");
            println!();
            println!("Shared:");
            println!("  m         Toggle Cartesian/Joint mode");
            println!("  [/]       Decrease/increase step size");
            println!("  n/Space   Go to next waypoint");
            println!("  p         Go to previous waypoint");
            println!("  0         Go to first waypoint (home)");
            println!("  x         Emergency stop");
            println!("  Esc       Quit");
            return Ok(());
        } else {
            rest_args.push(arg.clone());
        }
    }

    // Load waypoints if provided
    let (waypoint_arm, waypoints) = match &json_path {
        Some(path) => {
            let (arm, wps) = parse_waypoints(path)?;
            eprintln!("Loaded {} waypoints from {}", wps.len(), path);
            (Some(arm), wps)
        }
        None => (None, Vec::new()),
    };

    // Arm configs
    let arm_configs: Vec<(String, String)> = if rest_args.len() >= 2 {
        rest_args
            .chunks(2)
            .filter_map(|c| {
                if c.len() == 2 {
                    Some((c[0].clone(), c[1].clone()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        // Default: if waypoints specify an arm, use just that one; otherwise both
        match &waypoint_arm {
            Some(arm) if arm == "left" => vec![(
                "left".to_string(),
                "b370fdea33b52371b89d1b4c029d992c02a2591ee7b3e204ff1b606f75c43309".to_string(),
            )],
            Some(arm) if arm == "right" => vec![(
                "right".to_string(),
                "9280c3883e7bc2d41c219d9a0bf156fcff818da7fbdcb29cef33aeb1650ac426".to_string(),
            )],
            _ => vec![
                (
                    "left".to_string(),
                    "b370fdea33b52371b89d1b4c029d992c02a2591ee7b3e204ff1b606f75c43309".to_string(),
                ),
                (
                    "right".to_string(),
                    "9280c3883e7bc2d41c219d9a0bf156fcff818da7fbdcb29cef33aeb1650ac426".to_string(),
                ),
            ],
        }
    };

    // Connect to arms
    eprintln!("Connecting...");
    let mut arm_states: Vec<ArmState> = Vec::new();
    for (name, server_id) in &arm_configs {
        eprint!("  {} ({})... ", name, &server_id[..8]);
        match socketcan::new(server_id)
            .timeout(Duration::from_secs(10))
            .open()
        {
            Ok(mut socket) => {
                // Use 5ms timeout for non-blocking reads in 20ms control loop
                let _ = socket.set_timeout(Duration::from_millis(5));
                eprintln!("connected");
                arm_states.push(ArmState {
                    name: name.clone(),
                    socket,
                    target_pos: [0.0; 8],
                    commanded_pos: [0.0; 8],
                    actual_pos: [0.0; 8],
                    actual_valid: [false; 8],
                });
            }
            Err(e) => {
                eprintln!("FAILED: {}", e);
            }
        }
    }

    if arm_states.is_empty() {
        eprintln!("No arms connected.");
        return Ok(());
    }

    // Ctrl-C handler
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || {
            running.store(false, Ordering::SeqCst);
        })?;
    }

    // Enable motors and query initial positions
    eprintln!("Enabling motors and querying positions...");
    for arm in &mut arm_states {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
            // Zero-torque query to hold in place
            let frame = socketcan::CanFrame::new(motor_id, &QUERY_CMD)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
        }

        // Query current positions to initialize target/commanded
        let positions = query_motor_positions(&mut arm.socket)?;
        for (&motor_id, &pos) in &positions {
            if (1..=8).contains(&motor_id) {
                let idx = (motor_id - 1) as usize;
                arm.target_pos[idx] = pos;
                arm.commanded_pos[idx] = pos;
                arm.actual_pos[idx] = pos;
                arm.actual_valid[idx] = true;
            }
        }
        eprintln!(
            "  {} enabled ({} motors responding)",
            arm.name,
            positions.len()
        );
    }

    // Lift sequence: use Cartesian IK to move EE straight up in Z.
    // This guarantees a vertical path with no lateral sweep into the table.
    {
        let n_steps = (LIFT_HEIGHT_M / LIFT_STEP_M).ceil() as usize;
        eprintln!(
            "Lifting EE straight up by {:.0}mm ({} steps)...",
            LIFT_HEIGHT_M * 1000.0,
            n_steps
        );

        for step in 0..n_steps {
            if !running.load(Ordering::SeqCst) {
                break;
            }

            // Compute IK: move EE up by LIFT_STEP_M in Z (no X/Y/rotation change)
            let dz = if step == n_steps - 1 {
                LIFT_HEIGHT_M - (step as f64 * LIFT_STEP_M) // remainder for last step
            } else {
                LIFT_STEP_M
            };
            let dx = [0.0, 0.0, dz, 0.0, 0.0, 0.0];

            for arm in &mut arm_states {
                let (chain, ee_offset) = arm_chain(arm);
                apply_cartesian_delta(arm, &dx, chain, ee_offset);
            }

            // Ramp commanded_pos to target_pos with rate limiting
            loop {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                let tick_start = Instant::now();

                let mut all_converged = true;
                for arm in &mut arm_states {
                    let max_ratio = (0..8)
                        .map(|j| {
                            (arm.target_pos[j] - arm.commanded_pos[j]).abs() / MAX_STEP_PER_TICK
                        })
                        .fold(0.0f64, f64::max);
                    if max_ratio > 0.001 / MAX_STEP_PER_TICK {
                        all_converged = false;
                        if max_ratio > 1.0 {
                            for j in 0..8 {
                                let diff = arm.target_pos[j] - arm.commanded_pos[j];
                                arm.commanded_pos[j] += diff / max_ratio;
                            }
                        } else {
                            arm.commanded_pos = arm.target_pos;
                        }
                    }
                }

                // Send CAN commands
                for arm in &mut arm_states {
                    for motor_id in 1..=8u32 {
                        let idx = (motor_id - 1) as usize;
                        let cmd = encode_damiao_cmd(
                            arm.commanded_pos[idx],
                            0.0,
                            DEFAULT_KP,
                            DEFAULT_KD,
                            0.0,
                        );
                        if let Ok(frame) = socketcan::CanFrame::new(motor_id, &cmd) {
                            let _ = arm.socket.write_frame(&frame);
                        }
                    }
                }

                // Read responses
                for arm in &mut arm_states {
                    let responses = read_response_positions(&mut arm.socket);
                    for (&motor_id, &pos) in &responses {
                        if (1..=8).contains(&motor_id) {
                            let idx = (motor_id - 1) as usize;
                            arm.actual_pos[idx] = pos;
                            arm.actual_valid[idx] = true;
                        }
                    }
                }

                if all_converged {
                    break;
                }

                let elapsed = tick_start.elapsed();
                let tick_dur = Duration::from_millis(TICK_MS);
                if elapsed < tick_dur {
                    std::thread::sleep(tick_dur - elapsed);
                }
            }

            eprint!(
                "\r  lifted {:.0}/{:.0}mm   ",
                (step + 1) as f64 * LIFT_STEP_M * 1000.0,
                LIFT_HEIGHT_M * 1000.0
            );
        }
        eprintln!("\r  lift done.                    ");
    }

    // Teleop state
    let mut selected_arm: usize = 0;
    let mut selected_joint: usize = 0; // 0-7 (J1-J7 + grip)
    let mut step_idx: usize = 1; // index into STEP_SIZES_DEG / CART_*_STEPS (default 5°/5mm)
    let mut waypoint_idx: usize = 0;
    let mut estop = false;
    let mut control_mode = ControlMode::Cartesian;
    let mut safety_monitor = SafetyMonitor::new();
    let mut last_dashboard = Instant::now();
    let mut status_msg: String = String::new();

    // Enter raw mode for keyboard input
    let _raw_guard = RawModeGuard::enable()?;

    // Main 50Hz control loop
    while running.load(Ordering::SeqCst) && !estop {
        let tick_start = Instant::now();

        // 1. Poll keyboard (non-blocking)
        while event::poll(Duration::ZERO)? {
            if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
            {
                // Ctrl-C fallback
                if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                    running.store(false, Ordering::SeqCst);
                    break;
                }

                match code {
                    // Quit
                    KeyCode::Esc => {
                        running.store(false, Ordering::SeqCst);
                    }

                    // Emergency stop
                    KeyCode::Char('x') => {
                        emergency_stop(&mut arm_states);
                        estop = true;
                        status_msg = "EMERGENCY STOP".to_string();
                    }

                    // Mode toggle
                    KeyCode::Char('m') => {
                        control_mode = match control_mode {
                            ControlMode::Cartesian => ControlMode::Joint,
                            ControlMode::Joint => ControlMode::Cartesian,
                        };
                        status_msg = match control_mode {
                            ControlMode::Cartesian => "Mode: CARTESIAN".to_string(),
                            ControlMode::Joint => "Mode: JOINT".to_string(),
                        };
                    }

                    // --- Joint mode keys ---

                    // Joint selection: 1-7
                    KeyCode::Char(c @ '1'..='7') if control_mode == ControlMode::Joint => {
                        selected_joint = (c as usize) - ('1' as usize);
                        status_msg = format!("Selected J{}", selected_joint + 1);
                    }

                    // Joint 8 / gripper
                    KeyCode::Char('8') | KeyCode::Char('g')
                        if control_mode == ControlMode::Joint =>
                    {
                        selected_joint = 7;
                        status_msg = "Selected Gripper".to_string();
                    }

                    // Tab: cycle joint
                    KeyCode::Tab if control_mode == ControlMode::Joint => {
                        selected_joint = (selected_joint + 1) % 8;
                        let label = if selected_joint < 7 {
                            format!("J{}", selected_joint + 1)
                        } else {
                            "Gripper".to_string()
                        };
                        status_msg = format!("Selected {}", label);
                    }

                    // Joint move positive
                    KeyCode::Up | KeyCode::Char('k') if control_mode == ControlMode::Joint => {
                        if !estop && selected_arm < arm_states.len() {
                            let step_rad = STEP_SIZES_DEG[step_idx].to_radians();
                            let arm = &mut arm_states[selected_arm];
                            let new_val = arm.target_pos[selected_joint] + step_rad;
                            arm.target_pos[selected_joint] = new_val.clamp(
                                JOINT_LIMITS[selected_joint][0],
                                JOINT_LIMITS[selected_joint][1],
                            );
                            status_msg = format!(
                                "{} -> {:.1}°",
                                if selected_joint < 7 {
                                    format!("J{}", selected_joint + 1)
                                } else {
                                    "Grip".to_string()
                                },
                                arm.target_pos[selected_joint].to_degrees()
                            );
                        }
                    }

                    // Joint move negative
                    KeyCode::Down | KeyCode::Char('j') if control_mode == ControlMode::Joint => {
                        if !estop && selected_arm < arm_states.len() {
                            let step_rad = STEP_SIZES_DEG[step_idx].to_radians();
                            let arm = &mut arm_states[selected_arm];
                            let new_val = arm.target_pos[selected_joint] - step_rad;
                            arm.target_pos[selected_joint] = new_val.clamp(
                                JOINT_LIMITS[selected_joint][0],
                                JOINT_LIMITS[selected_joint][1],
                            );
                            status_msg = format!(
                                "{} -> {:.1}°",
                                if selected_joint < 7 {
                                    format!("J{}", selected_joint + 1)
                                } else {
                                    "Grip".to_string()
                                },
                                arm.target_pos[selected_joint].to_degrees()
                            );
                        }
                    }

                    // --- Cartesian mode keys ---

                    // Position: Z/Up = X+, S/Down = X-, A/Left = Y+, D/Right = Y-, E = Z+, Q = Z-
                    KeyCode::Char('z') | KeyCode::Up if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[0] = CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "X+".to_string();
                        }
                    }
                    KeyCode::Char('s') | KeyCode::Down
                        if control_mode == ControlMode::Cartesian =>
                    {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[0] = -CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "X-".to_string();
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Left
                        if control_mode == ControlMode::Cartesian =>
                    {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[1] = CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Y+".to_string();
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Right
                        if control_mode == ControlMode::Cartesian =>
                    {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[1] = -CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Y-".to_string();
                        }
                    }
                    KeyCode::Char('e') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[2] = CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Z+".to_string();
                        }
                    }
                    KeyCode::Char('q') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[2] = -CART_POS_STEPS[step_idx];
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Z-".to_string();
                        }
                    }

                    // Orientation: U/O = Roll, I/K = Pitch, J/L = Yaw
                    KeyCode::Char('u') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[3] = CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Roll+".to_string();
                        }
                    }
                    KeyCode::Char('o') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[3] = -CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Roll-".to_string();
                        }
                    }
                    KeyCode::Char('i') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[4] = CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Pitch+".to_string();
                        }
                    }
                    KeyCode::Char('k') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[4] = -CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Pitch-".to_string();
                        }
                    }
                    KeyCode::Char('j') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[5] = CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Yaw+".to_string();
                        }
                    }
                    KeyCode::Char('l') if control_mode == ControlMode::Cartesian => {
                        if !estop && selected_arm < arm_states.len() {
                            let (chain, ee_offset) = arm_chain(&arm_states[selected_arm]);
                            let mut dx = [0.0f64; 6];
                            dx[5] = -CART_ROT_STEPS_DEG[step_idx].to_radians();
                            apply_cartesian_delta(
                                &mut arm_states[selected_arm],
                                &dx,
                                chain,
                                ee_offset,
                            );
                            status_msg = "Yaw-".to_string();
                        }
                    }

                    // --- Shared keys (both modes) ---

                    // Step size
                    KeyCode::Char('[') => {
                        if step_idx > 0 {
                            step_idx -= 1;
                        }
                        status_msg = if control_mode == ControlMode::Cartesian {
                            format!(
                                "Step: {}mm / {}°",
                                (CART_POS_STEPS[step_idx] * 1000.0) as i32,
                                CART_ROT_STEPS_DEG[step_idx] as i32
                            )
                        } else {
                            format!("Step: {}°", STEP_SIZES_DEG[step_idx])
                        };
                    }
                    KeyCode::Char(']') => {
                        if step_idx < STEP_SIZES_DEG.len() - 1 {
                            step_idx += 1;
                        }
                        status_msg = if control_mode == ControlMode::Cartesian {
                            format!(
                                "Step: {}mm / {}°",
                                (CART_POS_STEPS[step_idx] * 1000.0) as i32,
                                CART_ROT_STEPS_DEG[step_idx] as i32
                            )
                        } else {
                            format!("Step: {}°", STEP_SIZES_DEG[step_idx])
                        };
                    }

                    // Next waypoint
                    KeyCode::Char('n') | KeyCode::Char(' ') => {
                        if !waypoints.is_empty() && !estop && selected_arm < arm_states.len() {
                            if waypoint_idx < waypoints.len() - 1 {
                                waypoint_idx += 1;
                            }
                            apply_waypoint(&mut arm_states[selected_arm], &waypoints[waypoint_idx]);
                            status_msg =
                                format!("Waypoint {}/{}", waypoint_idx + 1, waypoints.len());
                        }
                    }

                    // Previous waypoint
                    KeyCode::Char('p') => {
                        if !waypoints.is_empty() && !estop && selected_arm < arm_states.len() {
                            if waypoint_idx > 0 {
                                waypoint_idx -= 1;
                            }
                            apply_waypoint(&mut arm_states[selected_arm], &waypoints[waypoint_idx]);
                            status_msg =
                                format!("Waypoint {}/{}", waypoint_idx + 1, waypoints.len());
                        }
                    }

                    // Home (first waypoint)
                    KeyCode::Char('0') => {
                        if !waypoints.is_empty() && !estop && selected_arm < arm_states.len() {
                            waypoint_idx = 0;
                            apply_waypoint(&mut arm_states[selected_arm], &waypoints[0]);
                            status_msg = "Home (waypoint 1)".to_string();
                        }
                    }

                    // Switch arm
                    KeyCode::Left => {
                        if arm_states.len() > 1 {
                            selected_arm = if selected_arm == 0 {
                                arm_states.len() - 1
                            } else {
                                selected_arm - 1
                            };
                            status_msg = format!("Arm: {}", arm_states[selected_arm].name);
                        }
                    }
                    KeyCode::Right => {
                        if arm_states.len() > 1 {
                            selected_arm = (selected_arm + 1) % arm_states.len();
                            status_msg = format!("Arm: {}", arm_states[selected_arm].name);
                        }
                    }

                    _ => {}
                }
            }
        }

        if estop || !running.load(Ordering::SeqCst) {
            break;
        }

        // 2. Synchronized rate-limited interpolation: all joints arrive together
        //    The slowest joint moves at MAX_STEP_PER_TICK, others scale proportionally.
        //    This keeps the EE on a straight Cartesian path during transitions.
        for arm in &mut arm_states {
            let max_ratio = (0..8)
                .map(|j| (arm.target_pos[j] - arm.commanded_pos[j]).abs() / MAX_STEP_PER_TICK)
                .fold(0.0f64, f64::max);
            if max_ratio > 1.0 {
                for j in 0..8 {
                    let diff = arm.target_pos[j] - arm.commanded_pos[j];
                    arm.commanded_pos[j] += diff / max_ratio;
                }
            } else if max_ratio > 1e-6 / MAX_STEP_PER_TICK {
                arm.commanded_pos = arm.target_pos;
            }
        }

        // 3. Send CAN commands for all joints
        for arm in &mut arm_states {
            for motor_id in 1..=8u32 {
                let idx = (motor_id - 1) as usize;
                let cmd =
                    encode_damiao_cmd(arm.commanded_pos[idx], 0.0, DEFAULT_KP, DEFAULT_KD, 0.0);
                if let Ok(frame) = socketcan::CanFrame::new(motor_id, &cmd) {
                    let _ = arm.socket.write_frame(&frame);
                }
            }
        }

        // 4. Read responses
        for arm in &mut arm_states {
            let responses = read_response_positions(&mut arm.socket);
            for (&motor_id, &pos) in &responses {
                if (1..=8).contains(&motor_id) {
                    let idx = (motor_id - 1) as usize;
                    arm.actual_pos[idx] = pos;
                    arm.actual_valid[idx] = true;
                }
            }
        }

        // 5. Safety check
        for arm in &mut arm_states {
            let mut commanded = HashMap::new();
            let mut actual = HashMap::new();
            for j in 0..7 {
                let id = (j + 1) as u32;
                commanded.insert(id, arm.commanded_pos[j]);
                if arm.actual_valid[j] {
                    actual.insert(id, arm.actual_pos[j]);
                }
            }
            if !actual.is_empty() {
                match safety_monitor.check(&commanded, &actual, &arm.name) {
                    Ok(_) => {}
                    Err(_e) => {
                        emergency_stop(&mut arm_states);
                        estop = true;
                        status_msg = "SAFETY E-STOP".to_string();
                        break;
                    }
                }
            }
        }

        // 6. Render dashboard at 10Hz
        if last_dashboard.elapsed() >= Duration::from_millis(DASHBOARD_INTERVAL_MS) {
            last_dashboard = Instant::now();
            render_dashboard(
                &arm_states,
                selected_arm,
                selected_joint,
                step_idx,
                waypoint_idx,
                waypoints.len(),
                &status_msg,
                estop,
                control_mode,
            );
        }

        // Sleep to maintain 50Hz
        let elapsed = tick_start.elapsed();
        let tick_dur = Duration::from_millis(TICK_MS);
        if elapsed < tick_dur {
            std::thread::sleep(tick_dur - elapsed);
        }
    }

    // RawModeGuard drops here, restoring terminal
    drop(_raw_guard);

    // Disable motors on exit
    eprintln!("\nDisabling motors...");
    for arm in &mut arm_states {
        for motor_id in 0x01..=0x08u32 {
            if let Ok(frame) = socketcan::CanFrame::new(motor_id, &DISABLE_MIT) {
                let _ = arm.socket.write_frame(&frame);
                let _ = arm.socket.read_frame();
            }
        }
        eprintln!("  {} disabled", arm.name);
    }

    if estop {
        eprintln!("Exited due to emergency stop.");
    } else {
        eprintln!("Teleop ended.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Waypoint application
// ---------------------------------------------------------------------------

fn apply_waypoint(arm: &mut ArmState, waypoint: &Waypoint) {
    for (&motor_id, &(pos, _kp, _kd)) in &waypoint.targets {
        if (1..=8).contains(&motor_id) {
            let idx = (motor_id - 1) as usize;
            arm.target_pos[idx] = pos.clamp(JOINT_LIMITS[idx][0], JOINT_LIMITS[idx][1]);
        }
    }
}

// ---------------------------------------------------------------------------
// Dashboard rendering
// ---------------------------------------------------------------------------

fn render_dashboard(
    arm_states: &[ArmState],
    selected_arm: usize,
    selected_joint: usize,
    step_idx: usize,
    waypoint_idx: usize,
    waypoint_count: usize,
    status_msg: &str,
    estop: bool,
    control_mode: ControlMode,
) {
    let mut stdout = std::io::stdout();

    // Move cursor to top-left and clear
    let _ = crossterm::execute!(
        stdout,
        crossterm::cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All)
    );

    let arm = match arm_states.get(selected_arm) {
        Some(a) => a,
        None => return,
    };

    // Compute full EE pose
    let mut angles = [0.0f64; 7];
    angles.copy_from_slice(&arm.commanded_pos[..7]);
    let (chain, ee_offset) = if arm.name.contains("left") {
        (&LEFT_ARM_CHAIN, &LEFT_ARM_EE_OFFSET)
    } else {
        (&RIGHT_ARM_CHAIN, &RIGHT_ARM_EE_OFFSET)
    };
    let pose = ee_pose(&angles, chain, ee_offset);
    let pos = [pose[12], pose[13], pose[14]];
    let rpy = mat4_to_rpy(&pose);

    // Header
    let mode_label = match control_mode {
        ControlMode::Cartesian => "CARTESIAN",
        ControlMode::Joint => "JOINT",
    };
    let arm_label = if arm_states.len() > 1 {
        format!("{} ({}/{})", arm.name, selected_arm + 1, arm_states.len())
    } else {
        arm.name.clone()
    };
    let step_label = if control_mode == ControlMode::Cartesian {
        format!(
            "{}mm / {}°",
            (CART_POS_STEPS[step_idx] * 1000.0) as i32,
            CART_ROT_STEPS_DEG[step_idx] as i32
        )
    } else {
        format!("{}°", STEP_SIZES_DEG[step_idx] as i32)
    };
    let wp_label = if waypoint_count > 0 {
        format!("  WP {}/{}", waypoint_idx + 1, waypoint_count)
    } else {
        String::new()
    };
    let estop_label = if estop { "  ** E-STOP **" } else { "" };

    let _ = writeln!(
        stdout,
        "OpenArm Teleop [{}] - {}  Step: {}{}{}",
        mode_label, arm_label, step_label, wp_label, estop_label,
    );
    let _ = writeln!(stdout);

    // --- EE Pose section ---
    let _ = writeln!(stdout, "  End-Effector");
    let _ = writeln!(
        stdout,
        "    X: {:>+8.1} mm     Roll:  {:>+7.1}°",
        pos[0] * 1000.0,
        rpy[0].to_degrees(),
    );
    let _ = writeln!(
        stdout,
        "    Y: {:>+8.1} mm     Pitch: {:>+7.1}°",
        pos[1] * 1000.0,
        rpy[1].to_degrees(),
    );
    let _ = writeln!(
        stdout,
        "    Z: {:>+8.1} mm     Yaw:   {:>+7.1}°",
        pos[2] * 1000.0,
        rpy[2].to_degrees(),
    );
    let _ = writeln!(stdout);

    // --- Joints section ---
    if control_mode == ControlMode::Joint {
        // Full joint table in Joint mode
        let _ = writeln!(
            stdout,
            "  {:>5}  {:>8}  {:>8}  {:>8}  {:>14}",
            "Joint", "Target", "Command", "Actual", "Limits"
        );
        for j in 0..8 {
            let cursor = if j == selected_joint { ">" } else { " " };
            let label = if j < 7 {
                format!("J{}", j + 1)
            } else {
                "Grip".to_string()
            };
            let actual_str = if arm.actual_valid[j] {
                format!("{:>+7.1}°", arm.actual_pos[j].to_degrees())
            } else {
                "    n/a".to_string()
            };
            let ramping = (arm.target_pos[j] - arm.commanded_pos[j]).abs() > 0.001;
            let ramp_marker = if ramping { "~" } else { " " };
            let _ = writeln!(
                stdout,
                "{} {:>5}  {:>+7.1}°  {:>+7.1}°{} {}  [{:>+.0}, {:>+.0}]",
                cursor,
                label,
                arm.target_pos[j].to_degrees(),
                arm.commanded_pos[j].to_degrees(),
                ramp_marker,
                actual_str,
                JOINT_LIMITS[j][0].to_degrees(),
                JOINT_LIMITS[j][1].to_degrees(),
            );
        }
    } else {
        // Compact joint summary in Cartesian mode
        let _ = write!(stdout, "  Joints  ");
        for j in 0..7 {
            let ramping = (arm.target_pos[j] - arm.commanded_pos[j]).abs() > 0.001;
            let marker = if ramping { "~" } else { " " };
            let _ = write!(
                stdout,
                "J{}:{:>+6.1}°{}",
                j + 1,
                arm.commanded_pos[j].to_degrees(),
                marker,
            );
        }
        let _ = writeln!(stdout);
        let ramping = (arm.target_pos[7] - arm.commanded_pos[7]).abs() > 0.001;
        let marker = if ramping { "~" } else { " " };
        let _ = writeln!(
            stdout,
            "          Grip:{:>+6.1}°{}",
            arm.commanded_pos[7].to_degrees(),
            marker,
        );
    }

    let _ = writeln!(stdout);

    // Status line
    if !status_msg.is_empty() {
        let _ = writeln!(stdout, "  {}", status_msg);
    }

    let _ = writeln!(stdout);
    let help = match control_mode {
        ControlMode::Cartesian => {
            "ZASD=XY  E/Q=Z  UO=roll  IK=pitch  JL=yaw  [/]=step  m=joint  n/p=wp  x=estop  Esc=quit"
        }
        ControlMode::Joint => {
            "1-8=joint  Tab=next  Up/Down=move  [/]=step  m=cartesian  n/p=wp  x=estop  Esc=quit"
        }
    };
    let _ = write!(stdout, "{}", help);
    let _ = stdout.flush();
}
