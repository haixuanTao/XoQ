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
//!   openarm_playback <json-file> [<arm-name> <server-id> ...]
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

use anyhow::Result;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use xoq::socketcan;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];

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
        println!("Usage: openarm_playback <json-file> [<arm-name> <server-id> ...]");
        println!();
        println!("Supports v1 (wire-encoded bundles) and v2 (per-motor frames) JSON formats.");
        println!("Default arms: champagne left + right");
        return Ok(());
    }

    let json_path = &args[1];

    // Parse arm configs from CLI or use defaults
    let arm_configs: Vec<(String, String)> = if args.len() >= 4 {
        args[2..]
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

    // Enable motors
    println!("Enabling motors...");
    for (name, socket) in &mut arms {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            socket.write_frame(&frame)?;
            let _ = socket.read_frame();
        }
        println!("  {} enabled", name);
    }

    // Play back
    println!(
        "\nPlaying {} frames over {:.1}s...\n",
        timesteps.len(),
        duration
    );

    let start = Instant::now();
    let t_offset = timesteps[0].t;
    let mut sent = 0usize;

    for timestep in &timesteps {
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
    println!("\n\nPlayback complete ({} CAN frames sent).", sent);

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
