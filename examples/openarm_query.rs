//! OpenArm motor position poller
//!
//! Connects to all non-test CAN servers via iroh, enables motors in MIT mode,
//! and polls positions at ~20Hz. The CAN servers publish responses to MoQ
//! so browsers can monitor motor state in real-time.
//!
//! Usage:
//!   cargo run --example openarm_query --features="iroh can"
//!
//! The server IDs are read from machine.json on each robot.

use anyhow::Result;
use std::time::Duration;
use xoq::socketcan;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];

// Zero-torque query: p=0, v=0, kp=0, kd=0, tau=0
// Encodes to: pos_raw=0x8000, vel_raw=0x800, kp=0, kd=0, tau_raw=0x800
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

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("warn".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    // Default: all 4 non-test arms
    let default_arms = vec![
        // baguette
        (
            "baguette L",
            "d3cd5a3b4e5e877b1092333559b2673ca29b2f9e3257cd0945049dd1627f9b24",
        ),
        (
            "baguette R",
            "d1c0840bab8324b50dfc67b5c3c736629000ff7b3d269f79cda321feb1f79824",
        ),
        // champagne
        (
            "champagne L",
            "b370fdea33b52371b89d1b4c029d992c02a2591ee7b3e204ff1b606f75c43309",
        ),
        (
            "champagne R",
            "9280c3883e7bc2d41c219d9a0bf156fcff818da7fbdcb29cef33aeb1650ac426",
        ),
    ];

    // Allow passing custom server IDs: openarm_query <name1> <id1> <name2> <id2> ...
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

    println!("\nEnabling motors...");
    for arm in &mut arms {
        for motor_id in 0x01..=0x08u32 {
            let frame = socketcan::CanFrame::new(motor_id, &ENABLE_MIT)?;
            arm.socket.write_frame(&frame)?;
            // Read response
            let _ = arm.socket.read_frame();
        }
        println!("  {} enabled", arm.name);
    }

    println!("\nPolling positions at 20Hz (Ctrl+C to stop)...\n");

    // Print header
    print!("{:<14}", "Arm");
    for name in &JOINT_NAMES {
        print!("{:>8}", name);
    }
    println!();
    println!("{}", "-".repeat(14 + 8 * JOINT_NAMES.len()));

    loop {
        for arm in &mut arms {
            let mut positions = [0.0f64; 8];

            // Query all 8 motors
            for motor_id in 0x01..=0x08u32 {
                let frame = socketcan::CanFrame::new(motor_id, &QUERY_CMD)?;
                arm.socket.write_frame(&frame)?;
            }

            // Read responses (with timeout)
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

            // Print positions
            print!("{:<14}", arm.name);
            for pos in &positions {
                print!("{:>8.2}", pos.to_degrees());
            }
            println!();
        }

        std::thread::sleep(Duration::from_millis(50)); // ~20Hz
    }
}
