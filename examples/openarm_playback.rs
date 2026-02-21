//! OpenArm command playback
//!
//! Plays back recorded motor commands from a JSON file.
//! Each entry has a timestamp and base64-encoded CAN wire frames.
//!
//! JSON format:
//! ```json
//! [
//!   {"t": 0.0,    "L": "base64...", "R": "base64..."},
//!   {"t": 0.05,   "L": "base64...", "R": "base64..."},
//!   ...
//! ]
//! ```
//!
//! Each base64 value decodes to one or more wire-encoded CAN frames:
//!   [1B flags][4B can_id LE][1B data_len][8B data] = 14 bytes per motor
//!
//! Usage:
//!   openarm_playback <json-file> [<arm-name> <server-id> ...]
//!
//! Examples:
//!   # Play to champagne arms (default)
//!   openarm_playback recording.json
//!
//!   # Play to specific arm
//!   openarm_playback recording.json L b370fdea...
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

/// One frame in the recording.
struct Frame {
    t: f64,
    /// arm_name -> raw wire-encoded CAN bytes
    commands: HashMap<String, Vec<u8>>,
}

/// Parse the JSON recording file.
/// Format: [{"t": 0.0, "L": "base64...", "R": "base64..."}, ...]
fn parse_recording(path: &str) -> Result<Vec<Frame>> {
    let content = std::fs::read_to_string(path)?;

    // Minimal JSON array-of-objects parser
    let content = content.trim();
    if !content.starts_with('[') || !content.ends_with(']') {
        anyhow::bail!("JSON must be an array");
    }

    let mut frames = Vec::new();
    // Split by objects — find matching braces
    let inner = &content[1..content.len() - 1];
    let mut depth = 0;
    let mut obj_start = None;

    for (i, ch) in inner.char_indices() {
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
                        let obj_str = &inner[start..=i];
                        frames.push(parse_frame_obj(obj_str)?);
                    }
                }
            }
            _ => {}
        }
    }

    frames.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
    Ok(frames)
}

fn parse_frame_obj(s: &str) -> Result<Frame> {
    let inner = s.trim().trim_start_matches('{').trim_end_matches('}');
    let mut t: f64 = 0.0;
    let mut commands = HashMap::new();

    // Parse key-value pairs (simple: split by comma, handle "key": value)
    let mut remaining = inner;
    while !remaining.trim().is_empty() {
        // Find key
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

        // Skip colon
        let colon = remaining
            .find(':')
            .ok_or_else(|| anyhow::anyhow!("expected colon"))?;
        remaining = remaining[colon + 1..].trim_start();

        if key == "t" {
            // Parse number
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
            // Parse string value (base64)
            let val_start = remaining
                .find('"')
                .ok_or_else(|| anyhow::anyhow!("expected string value"))?;
            let after_val_start = &remaining[val_start + 1..];
            let val_end = after_val_start
                .find('"')
                .ok_or_else(|| anyhow::anyhow!("unterminated value string"))?;
            let val = &after_val_start[..val_end];
            remaining = &after_val_start[val_end + 1..];

            // Skip comma if present
            if let Some(comma) = remaining.find(',') {
                remaining = &remaining[comma + 1..];
            }

            let decoded = base64_decode(val)?;
            commands.insert(key.to_string(), decoded);
        }
    }

    Ok(Frame { t, commands })
}

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
        println!("JSON format:");
        println!(r#"  [{{"t": 0.0, "L": "base64...", "R": "base64..."}}, ...]"#);
        println!();
        println!("Each base64 value decodes to wire-encoded CAN frames:");
        println!("  [1B flags][4B can_id LE][1B data_len][8B data] per motor");
        println!();
        println!("Default arms: champagne L + R");
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
                "L".to_string(),
                "b370fdea33b52371b89d1b4c029d992c02a2591ee7b3e204ff1b606f75c43309".to_string(),
            ),
            (
                "R".to_string(),
                "9280c3883e7bc2d41c219d9a0bf156fcff818da7fbdcb29cef33aeb1650ac426".to_string(),
            ),
        ]
    };

    // Parse recording
    println!("Loading {}...", json_path);
    let frames = parse_recording(json_path)?;
    if frames.is_empty() {
        println!("No frames in recording.");
        return Ok(());
    }

    let duration = frames.last().unwrap().t - frames.first().unwrap().t;
    let arm_names_in_file: Vec<&str> = {
        let mut names: Vec<&str> = frames
            .iter()
            .flat_map(|f| f.commands.keys().map(|k| k.as_str()))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    println!(
        "  {} frames, {:.1}s duration, arms: {:?}",
        frames.len(),
        duration,
        arm_names_in_file
    );

    // Connect to arms
    println!("Connecting...");
    let mut arms: HashMap<String, socketcan::RemoteCanSocket> = HashMap::new();
    for (name, server_id) in &arm_configs {
        // Only connect if this arm appears in the recording
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
                // Use short timeout for reads during playback
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
        frames.len(),
        duration
    );

    let start = Instant::now();
    let t_offset = frames[0].t;
    let mut sent = 0usize;

    for frame in &frames {
        // Wait until the right time
        let target = Duration::from_secs_f64(frame.t - t_offset);
        let elapsed = start.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }

        // Send commands to each arm
        for (arm_name, data) in &frame.commands {
            if let Some(socket) = arms.get_mut(arm_name) {
                // Parse wire frames from the data and send them
                let mut offset = 0;
                while offset + 6 <= data.len() {
                    let data_len = data[offset + 5] as usize;
                    if offset + 6 + data_len > data.len() {
                        break;
                    }
                    let can_id = u32::from_le_bytes([
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                        data[offset + 4],
                    ]);
                    let frame_data = &data[offset + 6..offset + 6 + data_len];

                    if let Ok(can_frame) = socketcan::CanFrame::new(can_id, frame_data) {
                        let _ = socket.write_frame(&can_frame);
                    }
                    offset += 6 + data_len;
                    sent += 1;
                }
                // Drain responses
                while socket.read_frame().ok().flatten().is_some() {}
            }
        }

        // Progress
        let pct = ((frame.t - t_offset) / duration * 100.0) as u32;
        let elapsed = start.elapsed();
        print!(
            "\r  [{:>3}%] t={:.2}s elapsed={:.2}s frames_sent={}",
            pct,
            frame.t - t_offset,
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
