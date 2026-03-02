//! Offline test for Cartesian IK math — no hardware needed.
//! Exercises ee_pose, Jacobian, DLS solve, and verifies EE moves
//! in the expected direction for each Cartesian axis.
//!
//! Usage: cargo run --example test_cartesian_ik

use std::f64::consts::FRAC_PI_2;

// ---------------------------------------------------------------------------
// Copy of FK + IK math from openarm_teleop.rs
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

fn mat4_to_rpy(m: &Mat4) -> [f64; 3] {
    let r20 = m[2];
    let r00 = m[0];
    let r10 = m[1];
    let r21 = m[6];
    let r22 = m[10];
    let pitch = (-r20).atan2((r00 * r00 + r10 * r10).sqrt());
    let roll = r21.atan2(r22);
    let yaw = r10.atan2(r00);
    [roll, pitch, yaw]
}

struct JointDef {
    origin_xyz: [f64; 3],
    origin_rpy: [f64; 3],
    axis: [f64; 3],
}

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

const JOINT_LIMITS: [[f64; 2]; 8] = [
    [-1.396, 3.491],
    [-0.175, 3.316],
    [-1.571, 1.571],
    [-0.175, 2.967],
    [-1.571, 1.571],
    [-1.571, 1.571],
    [-1.571, 1.571],
    [-0.175, 1.745],
];

const DLS_LAMBDA: f64 = 0.001;
const JAC_EPS: f64 = 1e-6;

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
        jac[0][j] = (t1[12] - t0[12]) / JAC_EPS;
        jac[1][j] = (t1[13] - t0[13]) / JAC_EPS;
        jac[2][j] = (t1[14] - t0[14]) / JAC_EPS;
        let r_rel = mat4_rot_mul_rt(&t1, &t0);
        let omega = rotation_log(&r_rel);
        jac[3][j] = omega[0] / JAC_EPS;
        jac[4][j] = omega[1] / JAC_EPS;
        jac[5][j] = omega[2] / JAC_EPS;
    }
    jac
}

#[allow(clippy::needless_range_loop)]
fn dls_solve(jac: &[[f64; 7]; 6], dx: &[f64; 6], lambda: f64) -> [f64; 7] {
    let mut a = [[0.0f64; 7]; 6];
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
        a[i][6] = dx[i];
    }

    for col in 0..6 {
        let mut max_val = a[col][col].abs();
        let mut max_row = col;
        for row in (col + 1)..6 {
            if a[row][col].abs() > max_val {
                max_val = a[row][col].abs();
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            continue;
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

    let mut dq = [0.0f64; 7];
    for j in 0..7 {
        for i in 0..6 {
            dq[j] += jac[i][j] * y[i];
        }
    }
    dq
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn print_pose(label: &str, pose: &Mat4) {
    let rpy = mat4_to_rpy(pose);
    println!(
        "  {}  X: {:>+8.2} mm  Y: {:>+8.2} mm  Z: {:>+8.2} mm  R: {:>+6.1}°  P: {:>+6.1}°  Y: {:>+6.1}°",
        label,
        pose[12] * 1000.0,
        pose[13] * 1000.0,
        pose[14] * 1000.0,
        rpy[0].to_degrees(),
        rpy[1].to_degrees(),
        rpy[2].to_degrees(),
    );
}

fn test_config(name: &str, angles: &[f64; 7]) {
    let chain = &RIGHT_ARM_CHAIN;
    let ee_offset = &RIGHT_ARM_EE_OFFSET;

    println!("\n========== {} ==========", name);
    println!(
        "  Joints: [{:.1}°, {:.1}°, {:.1}°, {:.1}°, {:.1}°, {:.1}°, {:.1}°]",
        angles[0].to_degrees(),
        angles[1].to_degrees(),
        angles[2].to_degrees(),
        angles[3].to_degrees(),
        angles[4].to_degrees(),
        angles[5].to_degrees(),
        angles[6].to_degrees(),
    );

    let pose0 = ee_pose(angles, chain, ee_offset);
    print_pose("Current", &pose0);

    // Compute Jacobian
    let jac = numerical_jacobian(angles, chain, ee_offset);

    println!("\n  Jacobian (6x7):");
    let labels = [
        "  dX/dq", "  dY/dq", "  dZ/dq", "  dRx/dq", "  dRy/dq", "  dRz/dq",
    ];
    for (i, label) in labels.iter().enumerate() {
        print!("  {}: [", label);
        for j in 0..7 {
            if j > 0 {
                print!(", ");
            }
            print!("{:>+7.4}", jac[i][j]);
        }
        println!("]");
    }

    // Test each Cartesian direction
    let step = 0.005; // 5mm or 5deg
    let rot_step = 5.0f64.to_radians();
    let directions: &[(&str, [f64; 6])] = &[
        ("X+ (5mm)", [step, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Y+ (5mm)", [0.0, step, 0.0, 0.0, 0.0, 0.0]),
        ("Z+ (5mm)", [0.0, 0.0, step, 0.0, 0.0, 0.0]),
        ("Roll+ (5°)", [0.0, 0.0, 0.0, rot_step, 0.0, 0.0]),
        ("Pitch+ (5°)", [0.0, 0.0, 0.0, 0.0, rot_step, 0.0]),
        ("Yaw+ (5°)", [0.0, 0.0, 0.0, 0.0, 0.0, rot_step]),
    ];

    println!("\n  Direction tests (dx -> dq -> actual EE delta):");
    for (dir_name, dx) in directions {
        let dq = dls_solve(&jac, dx, DLS_LAMBDA);

        // Apply dq (with joint limit clamping)
        let mut new_angles = *angles;
        for i in 0..7 {
            new_angles[i] = (angles[i] + dq[i]).clamp(JOINT_LIMITS[i][0], JOINT_LIMITS[i][1]);
        }

        let pose1 = ee_pose(&new_angles, chain, ee_offset);
        let rpy0 = mat4_to_rpy(&pose0);
        let rpy1 = mat4_to_rpy(&pose1);

        let dx_actual = (pose1[12] - pose0[12]) * 1000.0;
        let dy_actual = (pose1[13] - pose0[13]) * 1000.0;
        let dz_actual = (pose1[14] - pose0[14]) * 1000.0;
        let dr_actual = (rpy1[0] - rpy0[0]).to_degrees();
        let dp_actual = (rpy1[1] - rpy0[1]).to_degrees();
        let dyw_actual = (rpy1[2] - rpy0[2]).to_degrees();

        let dq_max = dq.iter().map(|v| v.abs()).fold(0.0f64, f64::max);

        println!(
            "    {:>12}  dEE: dX={:>+6.2}mm dY={:>+6.2}mm dZ={:>+6.2}mm  dR={:>+5.1}° dP={:>+5.1}° dY={:>+5.1}°  |dq|max={:.4}rad",
            dir_name, dx_actual, dy_actual, dz_actual, dr_actual, dp_actual, dyw_actual, dq_max,
        );
    }
}

fn test_iterative(name: &str, angles: &[f64; 7]) {
    let chain = &RIGHT_ARM_CHAIN;
    let ee_offset = &RIGHT_ARM_EE_OFFSET;

    println!("\n========== Iterative IK: {} ==========", name);

    let step = 0.005;
    let rot_step = 5.0f64.to_radians();
    let directions: &[(&str, [f64; 6])] = &[
        ("X+ (5mm)", [step, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Y+ (5mm)", [0.0, step, 0.0, 0.0, 0.0, 0.0]),
        ("Z+ (5mm)", [0.0, 0.0, step, 0.0, 0.0, 0.0]),
        ("Roll+ (5°)", [0.0, 0.0, 0.0, rot_step, 0.0, 0.0]),
        ("Pitch+ (5°)", [0.0, 0.0, 0.0, 0.0, rot_step, 0.0]),
        ("Yaw+ (5°)", [0.0, 0.0, 0.0, 0.0, 0.0, rot_step]),
    ];

    for (dir_name, dx) in directions {
        let mut a = *angles;
        let pose0 = ee_pose(&a, chain, ee_offset);

        // Target pose
        let target_xyz = [pose0[12] + dx[0], pose0[13] + dx[1], pose0[14] + dx[2]];
        let omega_norm = (dx[3] * dx[3] + dx[4] * dx[4] + dx[5] * dx[5]).sqrt();
        let r_delta = if omega_norm > 1e-10 {
            let ax = [dx[3] / omega_norm, dx[4] / omega_norm, dx[5] / omega_norm];
            mat4_rotation_axis_angle(ax, omega_norm)
        } else {
            MAT4_IDENTITY
        };
        let target_rot = mat4_mul(&r_delta, &pose0);

        let mut iters = 0;
        for iter in 0..10 {
            let pose_cur = ee_pose(&a, chain, ee_offset);
            let ep = [
                target_xyz[0] - pose_cur[12],
                target_xyz[1] - pose_cur[13],
                target_xyz[2] - pose_cur[14],
            ];
            let r_err = mat4_rot_mul_rt(&target_rot, &pose_cur);
            let er = rotation_log(&r_err);
            let err = [ep[0], ep[1], ep[2], er[0], er[1], er[2]];
            let err_norm = err.iter().map(|e| e * e).sum::<f64>().sqrt();
            iters = iter + 1;
            if err_norm < 1e-6 {
                break;
            }
            let jac = numerical_jacobian(&a, chain, ee_offset);
            let dq = dls_solve(&jac, &err, DLS_LAMBDA);
            for i in 0..7 {
                a[i] = (a[i] + dq[i]).clamp(JOINT_LIMITS[i][0], JOINT_LIMITS[i][1]);
            }
        }

        let pose1 = ee_pose(&a, chain, ee_offset);
        let rpy0 = mat4_to_rpy(&pose0);
        let rpy1 = mat4_to_rpy(&pose1);
        println!(
            "    {:>12}  iters={}  dX={:>+6.2}mm dY={:>+6.2}mm dZ={:>+6.2}mm  dR={:>+5.1}° dP={:>+5.1}° dY={:>+5.1}°",
            dir_name,
            iters,
            (pose1[12]-pose0[12])*1000.0,
            (pose1[13]-pose0[13])*1000.0,
            (pose1[14]-pose0[14])*1000.0,
            (rpy1[0]-rpy0[0]).to_degrees(),
            (rpy1[1]-rpy0[1]).to_degrees(),
            (rpy1[2]-rpy0[2]).to_degrees(),
        );
    }
}

fn test_repeated_steps(name: &str, start_angles: &[f64; 7], dx: [f64; 6], n_steps: usize) {
    let chain = &RIGHT_ARM_CHAIN;
    let ee_offset = &RIGHT_ARM_EE_OFFSET;

    println!("\n========== Repeated steps: {} ==========", name);
    let mut angles = *start_angles;
    let pose0 = ee_pose(&angles, chain, ee_offset);
    let rpy0 = mat4_to_rpy(&pose0);
    println!(
        "  Start  X:{:>+8.2}  Y:{:>+8.2}  Z:{:>+8.2}  R:{:>+6.1}° P:{:>+6.1}° Y:{:>+6.1}°",
        pose0[12] * 1000.0,
        pose0[13] * 1000.0,
        pose0[14] * 1000.0,
        rpy0[0].to_degrees(),
        rpy0[1].to_degrees(),
        rpy0[2].to_degrees(),
    );

    for step in 0..n_steps {
        // Simulate apply_cartesian_delta
        let pose_start = ee_pose(&angles, chain, ee_offset);
        let target_xyz = [
            pose_start[12] + dx[0],
            pose_start[13] + dx[1],
            pose_start[14] + dx[2],
        ];
        let omega_norm = (dx[3] * dx[3] + dx[4] * dx[4] + dx[5] * dx[5]).sqrt();
        let r_delta = if omega_norm > 1e-10 {
            let ax = [dx[3] / omega_norm, dx[4] / omega_norm, dx[5] / omega_norm];
            mat4_rotation_axis_angle(ax, omega_norm)
        } else {
            MAT4_IDENTITY
        };
        let target_rot = mat4_mul(&r_delta, &pose_start);

        for _ in 0..10 {
            let pose_cur = ee_pose(&angles, chain, ee_offset);
            let ep = [
                target_xyz[0] - pose_cur[12],
                target_xyz[1] - pose_cur[13],
                target_xyz[2] - pose_cur[14],
            ];
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

        let pose_now = ee_pose(&angles, chain, ee_offset);
        let rpy_now = mat4_to_rpy(&pose_now);
        // Show cumulative drift from expected straight line
        let expected_x = pose0[12] * 1000.0;
        let expected_y = pose0[13] * 1000.0 + dx[1] * 1000.0 * (step as f64 + 1.0);
        let expected_z = pose0[14] * 1000.0;
        let drift_x = pose_now[12] * 1000.0 - expected_x;
        let drift_y = pose_now[13] * 1000.0 - expected_y;
        let drift_z = pose_now[14] * 1000.0 - expected_z;
        println!(
            "  #{:>2}  X:{:>+8.2}  Y:{:>+8.2}  Z:{:>+8.2}  drift: dX={:>+6.3} dY={:>+6.3} dZ={:>+6.3}  R:{:>+6.1}° P:{:>+6.1}° Y:{:>+6.1}°",
            step+1,
            pose_now[12]*1000.0, pose_now[13]*1000.0, pose_now[14]*1000.0,
            drift_x, drift_y, drift_z,
            rpy_now[0].to_degrees(), rpy_now[1].to_degrees(), rpy_now[2].to_degrees(),
        );
    }
}

fn main() {
    println!("=== Cartesian IK Test (right arm) ===");

    let ready = [
        0.0,
        45.0f64.to_radians(),
        0.0,
        90.0f64.to_radians(),
        0.0,
        0.0,
        0.0,
    ];

    // Simulate pressing A (Y+) 20 times at 1mm steps
    test_repeated_steps(
        "Y+ (left) x20 at 1mm",
        &ready,
        [0.0, 0.001, 0.0, 0.0, 0.0, 0.0],
        20,
    );

    // Simulate pressing D (Y-) 20 times
    test_repeated_steps(
        "Y- (right) x20 at 1mm",
        &ready,
        [0.0, -0.001, 0.0, 0.0, 0.0, 0.0],
        20,
    );

    // Simulate pressing Z (X+) 20 times
    test_repeated_steps(
        "X+ (fwd) x20 at 1mm",
        &ready,
        [0.001, 0.0, 0.0, 0.0, 0.0, 0.0],
        20,
    );

    // Simulate pressing E (Z+) 20 times
    test_repeated_steps(
        "Z+ (up) x20 at 1mm",
        &ready,
        [0.0, 0.0, 0.001, 0.0, 0.0, 0.0],
        20,
    );

    println!("\n=== Done ===");
}
