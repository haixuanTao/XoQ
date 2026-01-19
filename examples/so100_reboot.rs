//! Reboot SO100 servos over a remote serial connection.
//!
//! Usage:
//!   cargo run --example so100_reboot --features "iroh,serial" -- <server-endpoint-id>

use anyhow::Result;
use std::env;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// SO100 servo IDs (5-DOF arm)
const SERVO_IDS: [u8; 5] = [1, 2, 3, 4, 5];

/// Feetech protocol v1 instructions
const INST_PING: u8 = 0x01;
const INST_WRITE: u8 = 0x03;

/// STS3215 register addresses
const REG_TORQUE_ENABLE: u8 = 40;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: so100_reboot <remote-server-id>");
        println!("\nReboots/resets SO100 servos on a remote serial bridge.");
        return Ok(());
    }

    let remote_id = &args[1];
    println!("SO100 Servo Reset Utility");
    println!("=========================");
    println!("Connecting to: {}", remote_id);

    let mut port = xoq::serialport::new(remote_id)
        .timeout(Duration::from_millis(1000))
        .open()?;

    println!("Connected!\n");

    // First, disable torque on all servos
    println!("Disabling torque on all servos...");
    for id in SERVO_IDS {
        print!("  Servo {}: ", id);
        match write_torque(&mut port, id, false) {
            Ok(_) => println!("OK"),
            Err(e) => println!("Error: {}", e),
        }
        thread::sleep(Duration::from_millis(50));
    }

    println!("\nPinging servos...");
    for id in SERVO_IDS {
        print!("  Servo {}: ", id);
        match ping_servo(&mut port, id) {
            Ok(_) => println!("OK"),
            Err(e) => println!("No response ({})", e),
        }
        thread::sleep(Duration::from_millis(50));
    }

    println!("\nRe-enabling torque...");
    for id in SERVO_IDS {
        print!("  Servo {}: ", id);
        match write_torque(&mut port, id, true) {
            Ok(_) => println!("OK"),
            Err(e) => println!("Error: {}", e),
        }
        thread::sleep(Duration::from_millis(50));
    }

    println!("\nDone! Servos should be ready.");
    Ok(())
}

/// Calculate Feetech checksum
fn checksum(id: u8, length: u8, data: &[u8]) -> u8 {
    let mut sum: u16 = id as u16 + length as u16;
    for &b in data {
        sum += b as u16;
    }
    (!sum) as u8
}

/// Send a ping command to a servo
fn ping_servo(port: &mut xoq::serialport::RemoteSerialPort, id: u8) -> Result<()> {
    // Build ping packet: [0xFF, 0xFF, ID, Length, Instruction, Checksum]
    let length: u8 = 2; // instruction + checksum
    let chk = checksum(id, length, &[INST_PING]);
    let packet = [0xFF, 0xFF, id, length, INST_PING, chk];

    port.write_all(&packet)?;
    port.flush()?;

    // Read response
    thread::sleep(Duration::from_millis(10));
    let mut buf = [0u8; 32];
    let n = port.read(&mut buf)?;

    if n >= 6 && buf[0] == 0xFF && buf[1] == 0xFF && buf[2] == id {
        Ok(())
    } else if n == 0 {
        Err(anyhow::anyhow!("timeout"))
    } else {
        Err(anyhow::anyhow!("invalid response"))
    }
}

/// Write torque enable register
fn write_torque(port: &mut xoq::serialport::RemoteSerialPort, id: u8, enable: bool) -> Result<()> {
    // Build write packet: [0xFF, 0xFF, ID, Length, WRITE, RegAddr, Value, Checksum]
    let value = if enable { 1u8 } else { 0u8 };
    let length: u8 = 4; // instruction + addr + value + checksum
    let data = [INST_WRITE, REG_TORQUE_ENABLE, value];
    let chk = checksum(id, length, &data);
    let packet = [
        0xFF,
        0xFF,
        id,
        length,
        INST_WRITE,
        REG_TORQUE_ENABLE,
        value,
        chk,
    ];

    port.write_all(&packet)?;
    port.flush()?;

    // Read response
    thread::sleep(Duration::from_millis(10));
    let mut buf = [0u8; 32];
    let n = port.read(&mut buf)?;

    if n >= 6 && buf[0] == 0xFF && buf[1] == 0xFF && buf[2] == id {
        // Check for error in status byte
        if buf[4] != 0 {
            Err(anyhow::anyhow!("servo error: 0x{:02X}", buf[4]))
        } else {
            Ok(())
        }
    } else if n == 0 {
        Err(anyhow::anyhow!("timeout"))
    } else {
        Err(anyhow::anyhow!("invalid response"))
    }
}
