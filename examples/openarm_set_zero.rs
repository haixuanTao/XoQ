//! Set the current position as the new zero point on OpenArm motors.
//!
//! Connects to CAN servers via iroh, disables MIT mode, sends the Damiao
//! "set zero" command (0xFE) to each motor, then re-enables MIT mode.
//! This writes to the motor's non-volatile memory and persists across power cycles.
//!
//! Usage:
//!   cargo run --example openarm_set_zero --features="iroh can"
//!   cargo run --example openarm_set_zero --features="iroh can" -- <name1> <id1> [<name2> <id2> ...]

use anyhow::Result;
use std::io::{self, Write};
use std::time::Duration;
use xoq::socketcan;

const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];
const SET_ZERO: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE];
const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];

// Zero-torque query: p=0, v=0, kp=0, kd=0, tau=0
const QUERY_CMD: [u8; 8] = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00];

const POS_MIN: f64 = -12.5;
const POS_MAX: f64 = 12.5;

const JOINT_NAMES: [&str; 8] = ["J1", "J2", "J3", "J4", "J5", "J6", "J7", "Grip"];

struct Arm {
    name: String,
    socket: socketcan::RemoteCanSocket,
}

fn decode_pos(data: &[u8]) -> f64 {
    let pos_raw = ((data[1] as u16) << 8) | data[2] as u16;
    pos_raw as f64 / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN
}

fn query_positions(arm: &mut Arm) -> Result<[f64; 8]> {
    let mut positions = [0.0f64; 8];

    for motor_id in 0x01..=0x08u32 {
        let frame = socketcan::CanFrame::new(motor_id, &QUERY_CMD)?;
        arm.socket.write_frame(&frame)?;
    }

    for _ in 0..8 {
        match arm.socket.read_frame()? {
            Some(frame) => {
                let can_id = frame.id();
                if (0x11..=0x18).contains(&can_id) && frame.data().len() >= 8 {
                    let idx = (can_id - 0x11) as usize;
                    positions[idx] = decode_pos(frame.data());
                }
            }
            None => break,
        }
    }

    Ok(positions)
}

fn print_positions(name: &str, positions: &[f64; 8]) {
    print!("  {:<14}", name);
    for pos in positions {
        print!("{:>8.2}", pos.to_degrees());
    }
    println!();
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

    let default_arms = vec![
        (
            "baguette left",
            "d3cd5a3b4e5e877b1092333559b2673ca29b2f9e3257cd0945049dd1627f9b24",
        ),
        (
            "baguette right",
            "d1c0840bab8324b50dfc67b5c3c736629000ff7b3d269f79cda321feb1f79824",
        ),
    ];

    let arms_config: Vec<(&str, &str)> = if args.len() >= 3 {
        args[1..]
            .chunks(2)
            .filter_map(|c| {
                if c.len() == 2 {
                    Some((c[0].as_str(), c[1].as_str()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        default_arms
    };

    println!("Connecting to {} arms...", arms_config.len());

    let mut arms: Vec<Arm> = Vec::new();
    for (name, server_id) in &arms_config {
        print!("  {} ({})... ", name, &server_id[..8]);
        match socketcan::new(server_id)
            .timeout(Duration::from_secs(10))
            .open()
        {
            Ok(socket) => {
                println!("connected");
                arms.push(Arm {
                    name: name.to_string(),
                    socket,
                });
            }
            Err(e) => {
                println!("FAILED: {}", e);
            }
        }
    }

    if arms.is_empty() {
        println!("No arms connected, exiting.");
        return Ok(());
    }

    // Enable MIT mode and read current positions
    println!("\nEnabling motors and reading current positions...");
    for arm in &mut arms {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
        }
    }

    // Print header
    print!("  {:<14}", "Arm");
    for name in &JOINT_NAMES {
        print!("{:>8}", name);
    }
    println!();
    println!("  {}", "-".repeat(14 + 8 * JOINT_NAMES.len()));

    for arm in &mut arms {
        let positions = query_positions(arm)?;
        print_positions(&arm.name, &positions);
    }

    // Confirmation prompt
    println!("\nThis will set the CURRENT position as the new zero for ALL motors.");
    println!("This writes to non-volatile memory and persists across power cycles.");
    print!("\nType 'yes' to confirm: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    if input.trim() != "yes" {
        println!("Aborted.");
        return Ok(());
    }

    // Set zero on each arm
    for arm in &mut arms {
        println!("\nSetting zero on {}...", arm.name);

        // Disable MIT mode
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &DISABLE_MIT)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
        }
        println!("  MIT mode disabled");

        // Send set zero command
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &SET_ZERO)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("  Zero position set");

        // Re-enable MIT mode
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            arm.socket.write_frame(&frame)?;
            let _ = arm.socket.read_frame();
        }
        println!("  MIT mode re-enabled");
    }

    // Verify new positions
    println!("\nVerifying new positions (should be near zero)...");
    print!("  {:<14}", "Arm");
    for name in &JOINT_NAMES {
        print!("{:>8}", name);
    }
    println!();
    println!("  {}", "-".repeat(14 + 8 * JOINT_NAMES.len()));

    for arm in &mut arms {
        let positions = query_positions(arm)?;
        print_positions(&arm.name, &positions);
    }

    println!("\nDone.");
    Ok(())
}
