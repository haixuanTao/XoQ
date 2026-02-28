//! OpenArm command playback
//!
//! Plays back recorded motor commands from a JSON file.
//! Supports two formats:
//!
//! **v1** (wire-encoded bundles):
//! ```json
//! [
//!   {"t": 0.0, "left": "base64...", "right": "base64..."},
//!   ...
//! ]
//! ```
//!
//! **v2** (per-motor frames):
//! ```json
//! {
//!   "version": 2,
//!   "metadata": {"arm": "right", ...},
//!   "commands": [
//!     {"t": 0.0, "frames": [{"id": "0x01", "data": "base64..."}, ...]},
//!     ...
//!   ]
//! }
//! ```
//!
//! Usage:
//!   openarm_playback <json-file> [--loop [N]] [--step] [--interp] [<arm-name> <server-id> ...]
//!
//! Examples:
//!   # Play to champagne arms (default)
//!   openarm_playback recording.json
//!
//!   # Play to specific arm
//!   openarm_playback recording.json left b370fdea...
//!
//!   # Play to custom arms
//!   openarm_playback recording.json left <id1> right <id2>
//!
//!   # Loop forever (Ctrl-C to stop)
//!   openarm_playback recording.json --loop right <id>
//!
//!   # Loop 5 times
//!   openarm_playback recording.json --loop 5 right <id>
//!
//!   # Continuous slow interpolation between waypoints
//!   openarm_playback recording.json --interp left <id>
//!
//!   # Manual step: press Enter before each waypoint interpolation
//!   openarm_playback recording.json --step left <id>

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::f64::consts::FRAC_PI_2;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xoq::socketcan;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];

// Zero-torque query: p=0, v=0, kp=0, kd=0, tau=0
const QUERY_CMD: [u8; 8] = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00];

const POSITION_THRESHOLD_RAD: f64 = 0.175; // ~10 degrees
const MOVE_STEPS: usize = 50;
const MOVE_STEP_MS: u64 = 20;
const STEP_MAX_SPEED: f64 = 3.0; // rad/s — max interpolation speed per motor
const STEP_MIN_SUBSTEPS: usize = 3; // minimum substeps even for tiny moves
const STEP_MAX_SUBSTEPS: usize = 50; // cap for very large moves

const EE_ERROR_THRESHOLD: f64 = 0.05; // 5cm — triggers catchup pause
const EE_ESTOP_THRESHOLD: f64 = 0.30; // 30cm — hard e-stop, no convergence accepted
const MIN_JOINTS_FOR_FK: usize = 5; // need at least 5 of 7 joints for meaningful FK
const SAFETY_LAG_MS: u64 = 200; // compare actual against commanded from 200ms ago
const CONVERGENCE_MM: f64 = 0.001; // 1mm — arm "stopped moving" if EE moves less than this
const CONVERGENCE_COUNT: usize = 10; // 10 consecutive stable readings = converged (~200ms)

const POS_KI: f64 = 0.1; // integral gain: correction += KI * (cmd_pos - actual_pos) per update
const POS_CORRECTION_MAX: f64 = 1.5; // anti-windup: max correction ±1.5 rad (~86°)

const POS_MIN: f64 = -12.5;
const POS_MAX: f64 = 12.5;
const VEL_MIN: f64 = -45.0;
const VEL_MAX: f64 = 45.0;
const TAU_MIN: f64 = -18.0;
const TAU_MAX: f64 = 18.0;

/// Decode a Damiao MIT command (8 bytes) into (pos, vel, kp, kd, tau).
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

/// Encode a Damiao MIT command from (pos, vel, kp, kd, tau) into 8 bytes.
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

/// Decode position from a motor response frame (response bytes layout: data[1..3]).
fn decode_response_pos(data: &[u8]) -> f64 {
    let pos_raw = ((data[1] as u16) << 8) | data[2] as u16;
    pos_raw as f64 / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN
}

/// Minimal base64 decoder (no external dep).
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

/// A single CAN frame to send.
struct CanCmd {
    can_id: u32,
    data: Vec<u8>,
}

/// One timestep in the recording.
struct Timestep {
    t: f64,
    /// arm_name -> list of CAN frames to send
    commands: HashMap<String, Vec<CanCmd>>,
}

/// Parse the JSON recording file (auto-detects v1 array or v2 object format).
fn parse_recording(path: &str) -> Result<Vec<Timestep>> {
    let content = std::fs::read_to_string(path)?;
    let content = content.trim();

    if content.starts_with('{') {
        parse_recording_v2(content)
    } else if content.starts_with('[') {
        parse_recording_v1(content)
    } else {
        anyhow::bail!("JSON must be an array (v1) or object (v2)");
    }
}

// ---------------------------------------------------------------------------
// v2 parser
// ---------------------------------------------------------------------------

fn parse_recording_v2(content: &str) -> Result<Vec<Timestep>> {
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

    let mut timesteps = Vec::new();
    for obj_str in iter_objects(arr_inner) {
        timesteps.push(parse_v2_command(obj_str, &arm_name)?);
    }

    timesteps.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    Ok(timesteps)
}

fn parse_v2_command(s: &str, arm_name: &str) -> Result<Timestep> {
    let t = extract_number_field(s, "t").unwrap_or(0.0);

    let frames_idx = s
        .find("\"frames\"")
        .ok_or_else(|| anyhow::anyhow!("v2 command: missing 'frames'"))?;
    let after = &s[frames_idx..];
    let arr_start = after.find('[').unwrap_or(0);
    let arr_end = after.rfind(']').unwrap_or(after.len());
    let arr_inner = &after[arr_start + 1..arr_end];

    let mut can_frames = Vec::new();
    for frame_str in iter_objects(arr_inner) {
        let id_str = extract_string_field(frame_str, "id")
            .ok_or_else(|| anyhow::anyhow!("frame missing 'id'"))?;
        let data_b64 = extract_string_field(frame_str, "data")
            .ok_or_else(|| anyhow::anyhow!("frame missing 'data'"))?;

        let raw = base64_decode(&data_b64)?;
        if raw.len() == 72 {
            // v3: full 72-byte canfd_frame wire format — extract can_id and payload
            let can_id = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) & 0x1FFFFFFF;
            let len = (raw[4] as usize).min(64);
            can_frames.push(CanCmd {
                can_id,
                data: raw[8..8 + len].to_vec(),
            });
        } else {
            // v2: 8-byte MIT payload, id from JSON field
            can_frames.push(CanCmd {
                can_id: u32::from_str_radix(id_str.trim_start_matches("0x"), 16)?,
                data: raw,
            });
        }
    }

    let mut commands = HashMap::new();
    commands.insert(arm_name.to_string(), can_frames);
    Ok(Timestep { t, commands })
}

// ---------------------------------------------------------------------------
// v1 parser
// ---------------------------------------------------------------------------

fn parse_recording_v1(content: &str) -> Result<Vec<Timestep>> {
    let inner = &content[1..content.len() - 1];
    let mut timesteps = Vec::new();

    for obj_str in iter_objects(inner) {
        timesteps.push(parse_v1_obj(obj_str)?);
    }

    timesteps.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    Ok(timesteps)
}

fn parse_v1_obj(s: &str) -> Result<Timestep> {
    let inner = s.trim().trim_start_matches('{').trim_end_matches('}');
    let mut t: f64 = 0.0;
    let mut commands = HashMap::new();

    let mut remaining = inner;
    while !remaining.trim().is_empty() {
        let key_start = remaining.find('"').unwrap_or(remaining.len());
        if key_start >= remaining.len() {
            break;
        }
        let after_key_start = &remaining[key_start + 1..];
        let key_end = after_key_start
            .find('"')
            .ok_or_else(|| anyhow::anyhow!("unterminated key string"))?;
        let key = &after_key_start[..key_end];
        remaining = &after_key_start[key_end + 1..];

        let colon = remaining
            .find(':')
            .ok_or_else(|| anyhow::anyhow!("expected colon"))?;
        remaining = remaining[colon + 1..].trim_start();

        if key == "t" {
            let end = remaining
                .find(|c: char| c == ',' || c == '}' || c == '\n')
                .unwrap_or(remaining.len());
            t = remaining[..end].trim().parse()?;
            remaining = if end < remaining.len() {
                &remaining[end + 1..]
            } else {
                ""
            };
        } else {
            let val_start = remaining
                .find('"')
                .ok_or_else(|| anyhow::anyhow!("expected string value"))?;
            let after_val_start = &remaining[val_start + 1..];
            let val_end = after_val_start
                .find('"')
                .ok_or_else(|| anyhow::anyhow!("unterminated value string"))?;
            let val = &after_val_start[..val_end];
            remaining = &after_val_start[val_end + 1..];

            if let Some(comma) = remaining.find(',') {
                remaining = &remaining[comma + 1..];
            }

            // Decode wire-encoded CAN frames (72-byte canfd_frame format)
            let wire = base64_decode(val)?;
            let mut can_frames = Vec::new();
            let mut offset = 0;
            while offset + 72 <= wire.len() {
                let can_id = u32::from_le_bytes([
                    wire[offset],
                    wire[offset + 1],
                    wire[offset + 2],
                    wire[offset + 3],
                ]);
                let len = (wire[offset + 4] as usize).min(64);
                can_frames.push(CanCmd {
                    can_id,
                    data: wire[offset + 8..offset + 8 + len].to_vec(),
                });
                offset += 72;
            }
            commands.insert(key.to_string(), can_frames);
        }
    }

    Ok(Timestep { t, commands })
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn extract_string_field(s: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let quote1 = after.find('"')?;
    let rest = &after[quote1 + 1..];
    let quote2 = rest.find('"')?;
    Some(rest[..quote2].to_string())
}

fn extract_number_field(s: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let end = rest.find(|c: char| c == ',' || c == '}' || c == '\n')?;
    rest[..end].trim().parse().ok()
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

// ---------------------------------------------------------------------------
// Motor query / slow-move helpers
// ---------------------------------------------------------------------------

/// Query all 8 motors on a socket. Returns motor_id -> position (radians).
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

// ---------------------------------------------------------------------------
// Forward kinematics for safety monitoring
// ---------------------------------------------------------------------------

/// 4x4 homogeneous transformation matrix (column-major).
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

/// Compute end-effector position via forward kinematics.
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

/// Read all available motor response frames, returning motor_id -> actual position.
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

/// Compute commanded end-effector position from joint positions.
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

/// Compute actual end-effector position, using commanded values for missing joints.
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

/// Safety monitor with lag compensation, catchup-pause, and convergence detection.
///
/// Compares actual EE position against the commanded EE from SAFETY_LAG_MS ago.
/// Returns Ok(true) when the arm is behind (caller should pause and wait).
/// Detects convergence: if the arm stops moving (steady state), accepts it and
/// returns Ok(false) even if error exceeds threshold (gravity deflection is normal).
/// Hard e-stop only if error exceeds EE_ESTOP_THRESHOLD (catastrophic failure).
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

    /// Returns Ok(false) = all good / converged, Ok(true) = arm behind (pause), Err = e-stop.
    fn check(
        &mut self,
        commanded: &HashMap<u32, f64>,
        actual: &HashMap<u32, f64>,
        arm_name: &str,
    ) -> Result<bool> {
        let now = Instant::now();

        // Record commanded EE in history
        if let Some(ee) = compute_cmd_ee(commanded, arm_name) {
            let history = self.cmd_history.entry(arm_name.to_string()).or_default();
            history.push_back((now, ee));
            let cutoff = now - Duration::from_secs(4);
            while history.front().map_or(false, |(t, _)| *t < cutoff) {
                history.pop_front();
            }
        }

        // Find reference: commanded EE from SAFETY_LAG_MS ago
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

        // Hard e-stop: catastrophic deviation, no convergence accepted
        if error > EE_ESTOP_THRESHOLD {
            anyhow::bail!(
                "SAFETY: {} end-effector error {:.1}cm exceeds hard limit {:.1}cm — EMERGENCY STOP\n\
                 Reference EE: [{:.4}, {:.4}, {:.4}]\n\
                 Actual EE:    [{:.4}, {:.4}, {:.4}]",
                arm_name,
                error * 100.0,
                EE_ESTOP_THRESHOLD * 100.0,
                ref_ee[0],
                ref_ee[1],
                ref_ee[2],
                act_ee[0],
                act_ee[1],
                act_ee[2],
            );
        }

        if error > EE_ERROR_THRESHOLD {
            // Check convergence: has the arm stopped moving?
            let stable = self.stable_count.entry(arm_name.to_string()).or_insert(0);
            if let Some(prev) = self.last_actual_ee.get(arm_name) {
                let mx = act_ee[0] - prev[0];
                let my = act_ee[1] - prev[1];
                let mz = act_ee[2] - prev[2];
                let movement = (mx * mx + my * my + mz * mz).sqrt();
                if movement < CONVERGENCE_MM {
                    *stable += 1;
                    if *stable >= CONVERGENCE_COUNT {
                        // Arm at steady state — accept as caught up
                        eprintln!(
                            "  {} converged at {:.1}cm offset (steady state), resuming",
                            arm_name,
                            error * 100.0
                        );
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
                // Print per-joint diagnostics on first detection
                eprintln!(
                    "\n  SAFETY: {} EE error {:.1}cm (threshold {:.1}cm) — joint deltas:",
                    arm_name,
                    error * 100.0,
                    EE_ERROR_THRESHOLD * 100.0
                );
                for i in 0..7 {
                    let id = i as u32 + 1;
                    match (commanded.get(&id), actual.get(&id)) {
                        (Some(&cmd), Some(&act)) => {
                            let delta = act - cmd;
                            eprintln!(
                                "    J{}: cmd={:+.4} act={:+.4} delta={:+.4} ({:+.1}°)",
                                id,
                                cmd,
                                act,
                                delta,
                                delta.to_degrees()
                            );
                        }
                        (Some(&cmd), None) => {
                            eprintln!("    J{}: cmd={:+.4} act=n/a", id, cmd);
                        }
                        _ => {}
                    }
                }
            }

            // Arm still moving — signal catchup needed
            return Ok(true);
        }

        // Error below threshold — reset all state
        self.violation_start.insert(arm_name.to_string(), None);
        self.stable_count.insert(arm_name.to_string(), 0);
        self.last_actual_ee.remove(arm_name);
        Ok(false)
    }
}

/// Emergency stop: disable MIT mode on all motors across all arms.
fn emergency_stop(arms: &mut HashMap<String, socketcan::RemoteCanSocket>) {
    eprintln!("\n*** EMERGENCY STOP — Disabling all motors ***");
    for (name, socket) in arms.iter_mut() {
        for motor_id in 0x01..=0x08u32 {
            if let Ok(frame) = socketcan::CanFrame::new(motor_id, &DISABLE_MIT) {
                let _ = socket.write_frame(&frame);
                let _ = socket.read_frame();
            }
        }
        eprintln!("  {} disabled", name);
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("warn".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: openarm_playback <json-file> [--loop [N]] [--step] [--interp] [--no-safety] [<arm-name> <server-id> ...]");
        println!();
        println!("Supports v1 (wire-encoded bundles) and v2 (per-motor frames) JSON formats.");
        println!("Default arms: champagne left + right");
        println!();
        println!("Options:");
        println!("  --loop [N]   Loop playback N times (0 or omitted = infinite, Ctrl-C to stop)");
        println!(
            "  --interp     Interpolation mode: slowly move between waypoints (constant speed)"
        );
        println!("  --step       Step mode: like --interp but press Enter before each waypoint");
        println!("  --no-safety  Disable end-effector safety monitoring");
        return Ok(());
    }

    let json_path = &args[1];

    // Parse --loop, --step, and arm configs from remaining args
    let mut loop_count: Option<u64> = None; // None = no loop, Some(0) = infinite, Some(n) = n times
    let mut step_mode = false;
    let mut interp_mode = false;
    let mut safety_disabled = false;
    let mut rest_args: Vec<String> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--loop" || args[i] == "-l" {
            // Check if next arg is a number
            if i + 1 < args.len() {
                if let Ok(n) = args[i + 1].parse::<u64>() {
                    loop_count = Some(n);
                    i += 2;
                    continue;
                }
            }
            loop_count = Some(0); // infinite
            i += 1;
        } else if args[i] == "--step" || args[i] == "-s" {
            step_mode = true;
            interp_mode = true;
            i += 1;
        } else if args[i] == "--interp" || args[i] == "-i" {
            interp_mode = true;
            i += 1;
        } else if args[i] == "--no-safety" {
            safety_disabled = true;
            i += 1;
        } else {
            rest_args.push(args[i].clone());
            i += 1;
        }
    }

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
        vec![
            (
                "left".to_string(),
                "b370fdea33b52371b89d1b4c029d992c02a2591ee7b3e204ff1b606f75c43309".to_string(),
            ),
            (
                "right".to_string(),
                "9280c3883e7bc2d41c219d9a0bf156fcff818da7fbdcb29cef33aeb1650ac426".to_string(),
            ),
        ]
    };

    // Parse recording
    println!("Loading {}...", json_path);
    let timesteps = parse_recording(json_path)?;
    if timesteps.is_empty() {
        println!("No frames in recording.");
        return Ok(());
    }

    let duration = timesteps.last().unwrap().t - timesteps.first().unwrap().t;
    let arm_names_in_file: Vec<&str> = {
        let mut names: Vec<&str> = timesteps
            .iter()
            .flat_map(|f| f.commands.keys().map(|k| k.as_str()))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    println!(
        "  {} frames, {:.1}s duration, arms: {:?}",
        timesteps.len(),
        duration,
        arm_names_in_file
    );

    // Connect to arms
    println!("Connecting...");
    let mut arms: HashMap<String, socketcan::RemoteCanSocket> = HashMap::new();
    for (name, server_id) in &arm_configs {
        if !arm_names_in_file.contains(&name.as_str()) {
            println!("  {} — skipped (not in recording)", name);
            continue;
        }
        print!("  {} ({})... ", name, &server_id[..8]);
        match socketcan::new(server_id)
            .timeout(Duration::from_secs(10))
            .open()
        {
            Ok(mut socket) => {
                let _ = socket.set_timeout(Duration::from_millis(100));
                println!("connected");
                arms.insert(name.clone(), socket);
            }
            Err(e) => {
                println!("FAILED: {}", e);
            }
        }
    }

    if arms.is_empty() {
        println!("No arms connected.");
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

    // Enable motors — immediately follow with zero-torque query to prevent position jump
    println!("Enabling motors...");
    for (name, socket) in &mut arms {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            socket.write_frame(&frame)?;
            let _ = socket.read_frame();
            // Zero-torque query holds motor in place instead of jumping to stale position
            let frame = socketcan::CanFrame::new(motor_id, &QUERY_CMD)?;
            socket.write_frame(&frame)?;
            let _ = socket.read_frame();
        }
        println!("  {} enabled", name);
    }

    // --- Pre-playback safety check ---
    // Query current motor positions and compare with first waypoint.
    // If any motor is too far from its target, offer to slow-move there.
    println!("\nChecking motor positions...");
    let first_timestep = &timesteps[0];
    let mut needs_slow_move = false;

    // Collect per-arm data: (arm_name, motor_id, current_pos, target_pos, kp, kd)
    let mut mismatches: Vec<(String, u32, f64, f64, f64, f64)> = Vec::new();

    for (arm_name, socket) in &mut arms {
        let current_positions = query_motor_positions(socket)?;

        if let Some(target_cmds) = first_timestep.commands.get(arm_name) {
            for cmd in target_cmds {
                if cmd.data.len() == 8 {
                    let (target_pos, _vel, kp, kd, _tau) = decode_damiao_cmd(&cmd.data);
                    if let Some(&current_pos) = current_positions.get(&cmd.can_id) {
                        let delta = (current_pos - target_pos).abs();
                        if delta > POSITION_THRESHOLD_RAD {
                            needs_slow_move = true;
                        }
                        mismatches.push((
                            arm_name.clone(),
                            cmd.can_id,
                            current_pos,
                            target_pos,
                            kp,
                            kd,
                        ));
                    }
                }
            }
        }
    }

    // Immediately hold motors at queried position with kp/kd so they don't drift
    // while the user reads the screen or presses Enter
    for (arm_name, motor_id, current, _target, kp, kd) in &mismatches {
        if let Some(socket) = arms.get_mut(arm_name) {
            let cmd_data = encode_damiao_cmd(*current, 0.0, *kp, *kd, 0.0);
            if let Ok(frame) = socketcan::CanFrame::new(*motor_id, &cmd_data) {
                let _ = socket.write_frame(&frame);
            }
        }
    }
    for (_name, socket) in &mut arms {
        while socket.read_frame().ok().flatten().is_some() {}
    }

    if needs_slow_move {
        println!("\n  Motors far from start position:");
        println!(
            "  {:>6} {:>6} {:>10} {:>10} {:>10}",
            "Arm", "Motor", "Current", "Target", "Delta"
        );
        for (arm_name, motor_id, current, target, _kp, _kd) in &mismatches {
            let delta = current - target;
            let flag = if delta.abs() > POSITION_THRESHOLD_RAD {
                " <<"
            } else {
                ""
            };
            println!(
                "  {:>6} 0x{:02X}  {:>8.1}° {:>8.1}° {:>8.1}°{}",
                arm_name,
                motor_id,
                current.to_degrees(),
                target.to_degrees(),
                delta.to_degrees(),
                flag,
            );
        }
        print!("\n  Press Enter to slowly move to start position, or q to quit: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("q") {
            // Disable motors before exit
            println!("Disabling motors...");
            for (name, socket) in &mut arms {
                for motor_id in 0x01..=0x08u32 {
                    let frame = socketcan::CanFrame::new(motor_id, &DISABLE_MIT)?;
                    socket.write_frame(&frame)?;
                    let _ = socket.read_frame();
                }
                println!("  {} disabled", name);
            }
            return Ok(());
        }

        // Slow-move interpolation to start position
        println!(
            "  Moving to start position ({:.1}s)...",
            MOVE_STEPS as f64 * MOVE_STEP_MS as f64 / 1000.0
        );

        // Group mismatches by arm for efficient sending
        let mut arm_targets: HashMap<String, Vec<(u32, f64, f64, f64, f64)>> = HashMap::new();
        for (arm_name, motor_id, current, target, kp, kd) in &mismatches {
            arm_targets
                .entry(arm_name.clone())
                .or_default()
                .push((*motor_id, *current, *target, *kp, *kd));
        }

        for step in 0..MOVE_STEPS {
            if !running.load(Ordering::SeqCst) {
                println!("\n  Interrupted.");
                break;
            }
            let t = (step + 1) as f64 / MOVE_STEPS as f64;

            for (arm_name, targets) in &arm_targets {
                if let Some(socket) = arms.get_mut(arm_name) {
                    for &(motor_id, current, target, kp, kd) in targets {
                        let interp_pos = current + t * (target - current);
                        let cmd_data = encode_damiao_cmd(interp_pos, 0.0, kp, kd, 0.0);
                        if let Ok(frame) = socketcan::CanFrame::new(motor_id, &cmd_data) {
                            let _ = socket.write_frame(&frame);
                        }
                    }
                    // Drain responses
                    while socket.read_frame().ok().flatten().is_some() {}
                }
            }

            std::thread::sleep(Duration::from_millis(MOVE_STEP_MS));

            let pct = ((step + 1) as f64 / MOVE_STEPS as f64 * 100.0) as u32;
            print!("\r  Moving... {:>3}%", pct);
            let _ = std::io::stdout().flush();
        }
        if running.load(Ordering::SeqCst) {
            println!("\n  Reached start position.");
        }
    } else {
        println!("  Motors within tolerance of start position.");
    }

    // For interp/step mode: track previous positions to interpolate between waypoints.
    // Built from safety-check data (no extra zero-torque query that would drop stiffness).
    let mut prev_positions: HashMap<String, HashMap<u32, f64>> = HashMap::new();
    if interp_mode {
        for (arm_name, motor_id, current, target, _, _) in &mismatches {
            let pos = if needs_slow_move { *target } else { *current };
            prev_positions
                .entry(arm_name.clone())
                .or_default()
                .insert(*motor_id, pos);
        }
    }

    let total_loops = loop_count.unwrap_or(1); // 0 = infinite
    let mut iteration = 0u64;
    let mut total_sent = 0usize;
    let mut safety_monitor = SafetyMonitor::new();
    // Integral position correction: accumulates error to compensate gravity deflection.
    // correction[arm_name][motor_id] is added to commanded position each timestep.
    let mut pos_correction: HashMap<String, HashMap<u32, f64>> = HashMap::new();
    let mut pos_last_error: HashMap<String, HashMap<u32, f64>> = HashMap::new(); // latest per-motor error
    let mut last_correction_log = Instant::now();

    loop {
        iteration += 1;
        if total_loops > 0 && iteration > total_loops {
            break;
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let loop_label = if total_loops == 0 {
            format!("Loop {} (infinite, Ctrl-C to stop)", iteration)
        } else if total_loops == 1 {
            String::new()
        } else {
            format!("Loop {}/{}", iteration, total_loops)
        };

        if !loop_label.is_empty() {
            println!("\n{}", loop_label);
        }

        println!(
            "Playing {} frames over {:.1}s...\n",
            timesteps.len(),
            duration
        );

        let start = Instant::now();
        let t_offset = timesteps[0].t;
        let mut sent = 0usize;

        for (step_i, timestep) in timesteps.iter().enumerate() {
            if !running.load(Ordering::SeqCst) {
                break;
            }

            if interp_mode {
                // Decode current waypoint targets: (arm_name, motor_id, pos, kp, kd)
                let mut curr_targets: Vec<(String, u32, f64, f64, f64)> = Vec::new();
                for (arm_name, can_cmds) in &timestep.commands {
                    for cmd in can_cmds {
                        if cmd.data.len() == 8 {
                            let (pos, _vel, kp, kd, _tau) = decode_damiao_cmd(&cmd.data);
                            curr_targets.push((arm_name.clone(), cmd.can_id, pos, kp, kd));
                        }
                    }
                }

                use std::io::Write;

                if step_mode {
                    // Read buffered responses from previous interpolation
                    let mut actual_positions: HashMap<String, HashMap<u32, f64>> = HashMap::new();
                    for (arm_name, socket) in arms.iter_mut() {
                        let mut arm_pos = HashMap::new();
                        while let Ok(Some(frame)) = socket.read_frame() {
                            let can_id = frame.id();
                            if (0x11..=0x18).contains(&can_id) && frame.data().len() >= 8 {
                                arm_pos.insert(can_id - 0x10, decode_response_pos(frame.data()));
                            }
                        }
                        actual_positions.insert(arm_name.clone(), arm_pos);
                    }

                    println!("[Step {}/{}]", step_i + 1, timesteps.len());
                    for &(ref arm_name, motor_id, target_pos, _, _) in &curr_targets {
                        let curr_pos = actual_positions
                            .get(arm_name.as_str())
                            .and_then(|m| m.get(&motor_id))
                            .copied()
                            .or_else(|| {
                                prev_positions
                                    .get(arm_name.as_str())
                                    .and_then(|m| m.get(&motor_id))
                                    .copied()
                            });
                        let delta_str = match curr_pos {
                            Some(cp) => format!("{:>+6.1}°", (target_pos - cp).to_degrees()),
                            None => "   n/a".to_string(),
                        };
                        let curr_str = match curr_pos {
                            Some(cp) => format!("{:>7.1}°", cp.to_degrees()),
                            None => "    n/a".to_string(),
                        };
                        println!(
                            "  {} 0x{:02X}: {} -> {:>7.1}° ({})",
                            arm_name,
                            motor_id,
                            curr_str,
                            target_pos.to_degrees(),
                            delta_str,
                        );
                    }
                    print!("  Press Enter to move (q to quit)...");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    if input.trim() == "q" {
                        break;
                    }
                }

                // Compute substep count from max motor delta so speed is constant
                let mut max_delta: f64 = 0.0;
                for &(ref arm_name, motor_id, target_pos, _, _) in &curr_targets {
                    let prev_pos = prev_positions
                        .get(arm_name.as_str())
                        .and_then(|m| m.get(&motor_id))
                        .copied()
                        .unwrap_or(target_pos);
                    let delta = (target_pos - prev_pos).abs();
                    if delta > max_delta {
                        max_delta = delta;
                    }
                }
                // time = distance / speed, substeps = time / step_period
                let move_time_s = max_delta / STEP_MAX_SPEED;
                let substeps = (move_time_s / (MOVE_STEP_MS as f64 / 1000.0)).ceil() as usize;
                let substeps = substeps.clamp(STEP_MIN_SUBSTEPS, STEP_MAX_SUBSTEPS);

                // Interpolate from previous positions to current targets
                for substep in 0..substeps {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    let t = (substep + 1) as f64 / substeps as f64;

                    for (arm_name, socket) in arms.iter_mut() {
                        let arm_prev = prev_positions.get(arm_name.as_str());
                        for &(ref target_arm, motor_id, target_pos, kp, kd) in &curr_targets {
                            if target_arm != arm_name {
                                continue;
                            }
                            let prev_pos = arm_prev
                                .and_then(|m| m.get(&motor_id))
                                .copied()
                                .unwrap_or(target_pos);
                            let interp_pos = prev_pos + t * (target_pos - prev_pos);
                            let correction = pos_correction
                                .get(arm_name.as_str())
                                .and_then(|m| m.get(&motor_id))
                                .copied()
                                .unwrap_or(0.0);
                            let cmd_data =
                                encode_damiao_cmd(interp_pos + correction, 0.0, kp, kd, 0.0);
                            if let Ok(frame) = socketcan::CanFrame::new(motor_id, &cmd_data) {
                                let _ = socket.write_frame(&frame);
                                sent += 1;
                            }
                        }
                    }

                    std::thread::sleep(Duration::from_millis(MOVE_STEP_MS));

                    let pct = ((substep + 1) as f64 / substeps as f64 * 100.0) as u32;
                    print!(
                        "\r[Step {}/{}] Moving... {:>3}% ({:.1}s)",
                        step_i + 1,
                        timesteps.len(),
                        pct,
                        substeps as f64 * MOVE_STEP_MS as f64 / 1000.0,
                    );
                    let _ = std::io::stdout().flush();
                }
                println!();

                // Convergence loop: keep sending corrected commands until EE error is small
                const CONVERGE_EE_THRESHOLD: f64 = 0.02; // 2cm in end-effector space
                const CONVERGE_TIMEOUT_MS: u64 = 3000; // max 3s waiting per waypoint
                let converge_start = Instant::now();
                let mut converge_iter = 0u32;

                loop {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }

                    // Read responses and update corrections
                    let mut max_ee_err: f64 = 0.0;
                    let mut safety_error: Option<String> = None;
                    for (arm_name, socket) in arms.iter_mut() {
                        let actual = read_response_positions(socket);
                        let mut commanded = HashMap::new();
                        for &(ref target_arm, motor_id, target_pos, _, _) in &curr_targets {
                            if target_arm == arm_name && (1..=7).contains(&motor_id) {
                                commanded.insert(motor_id, target_pos);
                            }
                        }
                        for (&motor_id, &actual_pos) in &actual {
                            if let Some(&cmd_pos) = commanded.get(&motor_id) {
                                let error = cmd_pos - actual_pos;
                                pos_last_error
                                    .entry(arm_name.clone())
                                    .or_default()
                                    .insert(motor_id, error);
                                let corr = pos_correction
                                    .entry(arm_name.clone())
                                    .or_default()
                                    .entry(motor_id)
                                    .or_insert(0.0);
                                *corr = (*corr + POS_KI * error)
                                    .clamp(-POS_CORRECTION_MAX, POS_CORRECTION_MAX);
                            }
                        }
                        // Compute EE error via FK
                        if let (Some(cmd_ee), Some(act_ee)) = (
                            compute_cmd_ee(&commanded, arm_name),
                            compute_act_ee(&commanded, &actual, arm_name),
                        ) {
                            let dx = cmd_ee[0] - act_ee[0];
                            let dy = cmd_ee[1] - act_ee[1];
                            let dz = cmd_ee[2] - act_ee[2];
                            let ee_err = (dx * dx + dy * dy + dz * dz).sqrt();
                            if ee_err > max_ee_err {
                                max_ee_err = ee_err;
                            }
                        }
                        if !safety_disabled && !actual.is_empty() && !commanded.is_empty() {
                            match safety_monitor.check(&commanded, &actual, arm_name) {
                                Ok(_) => {}
                                Err(e) => {
                                    safety_error = Some(e.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(err) = safety_error {
                        emergency_stop(&mut arms);
                        anyhow::bail!("{}", err);
                    }

                    // Converged or first iteration (no responses yet)
                    if max_ee_err < CONVERGE_EE_THRESHOLD || converge_iter == 0 {
                        if converge_iter > 1 {
                            eprintln!(
                                "  Converged after {:.1}s (EE err {:.1}cm)",
                                converge_start.elapsed().as_secs_f64(),
                                max_ee_err * 100.0,
                            );
                        }
                        break;
                    }

                    // Timeout
                    if converge_start.elapsed() > Duration::from_millis(CONVERGE_TIMEOUT_MS) {
                        eprintln!(
                            "  Convergence timeout ({:.1}s), EE err {:.1}cm (threshold {:.1}cm), continuing",
                            CONVERGE_TIMEOUT_MS as f64 / 1000.0,
                            max_ee_err * 100.0,
                            CONVERGE_EE_THRESHOLD * 100.0,
                        );
                        break;
                    }

                    // Log on first detection
                    if converge_iter == 1 {
                        eprintln!(
                            "  Waiting for convergence (EE err {:.1}cm, threshold {:.1}cm)...",
                            max_ee_err * 100.0,
                            CONVERGE_EE_THRESHOLD * 100.0,
                        );
                    }

                    // Re-send corrected commands and wait
                    for (arm_name, socket) in arms.iter_mut() {
                        for &(ref target_arm, motor_id, target_pos, kp, kd) in &curr_targets {
                            if target_arm != arm_name {
                                continue;
                            }
                            let correction = pos_correction
                                .get(arm_name.as_str())
                                .and_then(|m| m.get(&motor_id))
                                .copied()
                                .unwrap_or(0.0);
                            let cmd_data =
                                encode_damiao_cmd(target_pos + correction, 0.0, kp, kd, 0.0);
                            if let Ok(frame) = socketcan::CanFrame::new(motor_id, &cmd_data) {
                                let _ = socket.write_frame(&frame);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(MOVE_STEP_MS));
                    converge_iter += 1;
                }

                // Update prev_positions for next waypoint
                for &(ref arm_name, motor_id, target_pos, _, _) in &curr_targets {
                    prev_positions
                        .entry(arm_name.clone())
                        .or_default()
                        .insert(motor_id, target_pos);
                }

                continue;
            }

            // Normal mode: wait for target time
            let target = Duration::from_secs_f64(timestep.t - t_offset);
            let elapsed = start.elapsed();
            if target > elapsed {
                std::thread::sleep(target - elapsed);
            }

            let mut safety_error: Option<String> = None;
            let mut throttle = false;
            for (arm_name, can_cmds) in &timestep.commands {
                if let Some(socket) = arms.get_mut(arm_name) {
                    let mut commanded = HashMap::new();
                    for cmd in can_cmds {
                        if cmd.data.len() == 8 && (1..=7).contains(&cmd.can_id) {
                            // Motor command: apply integral correction
                            let (pos, vel, kp, kd, tau) = decode_damiao_cmd(&cmd.data);
                            commanded.insert(cmd.can_id, pos); // record ORIGINAL for safety
                            let correction = pos_correction
                                .get(arm_name)
                                .and_then(|m| m.get(&cmd.can_id))
                                .copied()
                                .unwrap_or(0.0);
                            let corrected = encode_damiao_cmd(pos + correction, vel, kp, kd, tau);
                            if let Ok(frame) = socketcan::CanFrame::new(cmd.can_id, &corrected) {
                                let _ = socket.write_frame(&frame);
                                sent += 1;
                            }
                        } else {
                            // Non-motor command (enable/disable MIT, motor 8, etc): send as-is
                            if let Ok(can_frame) = socketcan::CanFrame::new(cmd.can_id, &cmd.data) {
                                let _ = socket.write_frame(&can_frame);
                                sent += 1;
                            }
                        }
                    }
                    let actual = read_response_positions(socket);
                    // Update integral corrections from response errors
                    for (&motor_id, &actual_pos) in &actual {
                        if let Some(&cmd_pos) = commanded.get(&motor_id) {
                            let error = cmd_pos - actual_pos;
                            pos_last_error
                                .entry(arm_name.clone())
                                .or_default()
                                .insert(motor_id, error);
                            let corr = pos_correction
                                .entry(arm_name.clone())
                                .or_default()
                                .entry(motor_id)
                                .or_insert(0.0);
                            *corr = (*corr + POS_KI * error)
                                .clamp(-POS_CORRECTION_MAX, POS_CORRECTION_MAX);
                        }
                    }
                    if !safety_disabled && !actual.is_empty() && !commanded.is_empty() {
                        match safety_monitor.check(&commanded, &actual, arm_name) {
                            Ok(true) => throttle = true,
                            Ok(false) => {}
                            Err(e) => {
                                safety_error = Some(e.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            if let Some(err) = safety_error {
                emergency_stop(&mut arms);
                anyhow::bail!("{}", err);
            }
            // Arm behind — slow down by delaying the next timestep
            if throttle {
                std::thread::sleep(Duration::from_millis(20));
            }

            // Log active integral corrections periodically (~1s)
            if last_correction_log.elapsed() >= Duration::from_secs(1) {
                let mut parts: Vec<String> = Vec::new();
                for (arm_name, corrs) in &pos_correction {
                    let errors = pos_last_error.get(arm_name);
                    let mut motor_parts: Vec<String> = Vec::new();
                    for motor_id in 1..=7u32 {
                        if let Some(&c) = corrs.get(&motor_id) {
                            if c.abs() > 1.0_f64.to_radians() {
                                let err = errors
                                    .and_then(|e| e.get(&motor_id))
                                    .copied()
                                    .unwrap_or(0.0);
                                let sat = if c.abs() >= POS_CORRECTION_MAX - 0.001 {
                                    " SAT!"
                                } else {
                                    ""
                                };
                                motor_parts.push(format!(
                                    "J{}: corr {:+.1}° err {:+.1}°{}",
                                    motor_id,
                                    c.to_degrees(),
                                    err.to_degrees(),
                                    sat,
                                ));
                            }
                        }
                    }
                    if !motor_parts.is_empty() {
                        parts.push(format!("{} [{}]", arm_name, motor_parts.join(", ")));
                    }
                }
                if !parts.is_empty() {
                    eprintln!("\n  INTEGRAL CORRECTION: {}", parts.join(", "));
                    last_correction_log = Instant::now();
                }
            }

            {
                let pct = ((timestep.t - t_offset) / duration * 100.0) as u32;
                let elapsed = start.elapsed();
                print!(
                    "\r  [{:>3}%] t={:.2}s elapsed={:.2}s frames_sent={}",
                    pct,
                    timestep.t - t_offset,
                    elapsed.as_secs_f64(),
                    sent
                );
            }
        }

        total_sent += sent;
        println!();

        if total_loops == 1 {
            break;
        }
    }

    println!("\nPlayback complete ({} CAN frames sent).", total_sent);

    // Disable motors
    println!("Disabling motors...");
    for (name, socket) in &mut arms {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &DISABLE_MIT)?;
            socket.write_frame(&frame)?;
            let _ = socket.read_frame();
        }
        println!("  {} disabled", name);
    }

    Ok(())
}
