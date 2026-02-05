//! SO100 teleoperation example - control a remote SO100 arm with a local one.
//!
//! This example reads positions from a local "leader" SO100 arm and sends them
//! to a remote "follower" SO100 arm over iroh P2P.
//!
//! Setup:
//! 1. Run a serial bridge server on the machine with the follower arm:
//!    `cargo run --example serial_server --features "iroh,serial" -- /dev/ttyUSB0 1000000`
//!
//! 2. Run this teleop client with the local leader arm:
//!    `cargo run --example so100_teleop --features "iroh,serial" -- /dev/ttyUSB0 <server-endpoint-id>`
//!
//! The leader arm will have torque disabled (you move it by hand).
//! The follower arm will mirror the leader's movements.

use anyhow::Result;
use rustypot::servo::feetech::sts3215::Sts3215Controller;
use std::env;
use std::thread;
use std::time::{Duration, Instant};

/// SO100 servo IDs (5-DOF arm)
const SERVO_IDS: [u8; 5] = [1, 2, 3, 4, 5];

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        println!("Usage: so100_teleop <local-serial-port> <remote-server-id>");
        println!("\nExample:");
        println!("  cargo run --example so100_teleop --features \"iroh,serial\" -- /dev/ttyUSB0 <server-id>");
        println!("\nThis reads from the local leader arm and sends to the remote follower arm.");
        return Ok(());
    }

    let local_port = &args[1];
    let remote_id = &args[2];

    println!("SO100 Teleoperation");
    println!("===================");
    println!("Leader (local):   {}", local_port);
    println!("Follower (remote): {}", remote_id);
    println!();

    // Open local serial port for leader arm
    println!("Opening local serial port...");
    let leader_port = serialport::new(local_port, 1_000_000)
        .timeout(Duration::from_millis(1000))
        .open()?;

    // Open remote serial port for follower arm
    // Use datagrams for writes: each servo command is sent as a separate QUIC datagram,
    // avoiding stream coalescing that batches multiple commands into one packet.
    // Responses still use the reliable stream.
    println!("Connecting to remote follower arm...");
    let follower_port = xoq::serialport::new(remote_id)
        .timeout(Duration::from_millis(1000))
        .use_datagrams(true)
        .open()?;

    // Create controllers
    let mut leader = Sts3215Controller::new()
        .with_protocol_v1()
        .with_serial_port(leader_port);

    let mut follower = Sts3215Controller::new()
        .with_protocol_v1()
        .with_serial_port(Box::new(follower_port));

    println!("Connected!");
    println!();

    // Disable torque on leader (so user can move it freely)
    println!("Disabling torque on leader arm...");
    for id in SERVO_IDS {
        if let Err(e) = leader.write_torque_enable(id, false) {
            println!(
                "Warning: Failed to disable torque on leader servo {}: {}",
                id, e
            );
        }
        thread::sleep(Duration::from_millis(10)); // Small delay between commands
    }

    // Enable torque on follower
    println!("Enabling torque on follower arm...");
    for id in SERVO_IDS {
        if let Err(e) = follower.write_torque_enable(id, true) {
            println!(
                "Warning: Failed to enable torque on follower servo {}: {}",
                id, e
            );
        }
        thread::sleep(Duration::from_millis(10)); // Small delay between commands
    }

    println!();
    println!("Teleoperation active! Move the leader arm to control the follower.");
    println!("Press Ctrl+C to stop.");
    println!();

    // Set up Ctrl+C handler to disable follower torque on exit
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })?;

    // Teleoperation loop
    let mut loop_count: u64 = 0;
    while running.load(std::sync::atomic::Ordering::SeqCst) {
        let t0 = Instant::now();

        // Read positions from leader
        let read_result = leader.sync_read_present_position(&SERVO_IDS);
        let t_read = t0.elapsed();

        match read_result {
            Ok(positions) => {
                // Send positions to follower
                let t1 = Instant::now();
                if let Err(e) = follower.sync_write_goal_position(&SERVO_IDS, &positions) {
                    println!("Follower write error: {}", e);
                }
                let t_write = t1.elapsed();

                if t_read > Duration::from_millis(15) || t_write > Duration::from_millis(5) {
                    println!(
                        "[{}] SLOW  read={:.1}ms  write={:.1}ms",
                        loop_count,
                        t_read.as_secs_f64() * 1000.0,
                        t_write.as_secs_f64() * 1000.0,
                    );
                }
            }
            Err(e) => {
                println!(
                    "[{}] Leader read error (took {:.1}ms): {}",
                    loop_count,
                    t_read.as_secs_f64() * 1000.0,
                    e
                );
            }
        }

        loop_count += 1;

        // Small delay to prevent overwhelming the bus
        thread::sleep(Duration::from_millis(10));
    }

    // Cleanup: disable torque on follower
    println!("\nDisabling follower torque...");
    for id in SERVO_IDS {
        let _ = follower.write_torque_enable(id, false);
    }

    println!("Teleoperation ended.");
    Ok(())
}
