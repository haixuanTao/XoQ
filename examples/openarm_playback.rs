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
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xoq::socketcan;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];

// Zero-torque query: p=0, v=0, kp=0, kd=0, tau=0
const QUERY_CMD: [u8; 8] = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00];

const POSITION_THRESHOLD_RAD: f64 = 0.175; // ~10 degrees
const MOVE_STEPS: usize = 100;
const MOVE_STEP_MS: u64 = 30;
const STEP_MAX_SPEED: f64 = 1.0; // rad/s — max interpolation speed per motor
const STEP_MIN_SUBSTEPS: usize = 3; // minimum substeps even for tiny moves
const STEP_MAX_SUBSTEPS: usize = 100; // cap for very large moves

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

        can_frames.push(CanCmd {
            can_id: u32::from_str_radix(id_str.trim_start_matches("0x"), 16)?,
            data: base64_decode(&data_b64)?,
        });
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

            // Decode wire-encoded CAN frames
            let wire = base64_decode(val)?;
            let mut can_frames = Vec::new();
            let mut offset = 0;
            while offset + 6 <= wire.len() {
                let data_len = wire[offset + 5] as usize;
                if offset + 6 + data_len > wire.len() {
                    break;
                }
                let can_id = u32::from_le_bytes([
                    wire[offset + 1],
                    wire[offset + 2],
                    wire[offset + 3],
                    wire[offset + 4],
                ]);
                can_frames.push(CanCmd {
                    can_id,
                    data: wire[offset + 6..offset + 6 + data_len].to_vec(),
                });
                offset += 6 + data_len;
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
        println!("Usage: openarm_playback <json-file> [--loop [N]] [--step] [--interp] [<arm-name> <server-id> ...]");
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
        return Ok(());
    }

    let json_path = &args[1];

    // Parse --loop, --step, and arm configs from remaining args
    let mut loop_count: Option<u64> = None; // None = no loop, Some(0) = infinite, Some(n) = n times
    let mut step_mode = false;
    let mut interp_mode = false;
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
                            let cmd_data = encode_damiao_cmd(interp_pos, 0.0, kp, kd, 0.0);
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

            for (arm_name, can_cmds) in &timestep.commands {
                if let Some(socket) = arms.get_mut(arm_name) {
                    for cmd in can_cmds {
                        if let Ok(can_frame) = socketcan::CanFrame::new(cmd.can_id, &cmd.data) {
                            let _ = socket.write_frame(&can_frame);
                            sent += 1;
                        }
                    }
                    // Drain responses
                    while socket.read_frame().ok().flatten().is_some() {}
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
