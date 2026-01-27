//! CAN bridge client - connects to remote CAN interface
//!
//! Usage: can_client <server-endpoint-id> [--send <id> <data>]
//!
//! Examples:
//!   can_client abc123...  (listen for frames)
//!   can_client abc123... --send 0x123 01020304  (send a frame then listen)
//!
//! Set RUST_LOG=debug for verbose output

use anyhow::Result;
use std::env;
use xoq::socketcan;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=debug".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: can_client <server-endpoint-id> [--send <id> <data>]");
        println!("\nExamples:");
        println!("  can_client abc123...              # Listen for frames");
        println!("  can_client abc123... --send 0x123 01020304  # Send a frame then listen");
        return Ok(());
    }

    let server_id = &args[1];
    println!("Connecting to CAN bridge: {}", server_id);

    // Connect to the remote CAN interface
    let mut socket = socketcan::new(server_id)
        .timeout(std::time::Duration::from_secs(5))
        .open()?;

    println!("Connected!");

    // Check if we should send a frame
    if let Some(send_idx) = args.iter().position(|a| a == "--send") {
        if args.len() > send_idx + 2 {
            let can_id_str = &args[send_idx + 1];
            let data_str = &args[send_idx + 2];

            // Parse CAN ID (supports 0x prefix)
            let can_id: u32 = if can_id_str.starts_with("0x") || can_id_str.starts_with("0X") {
                u32::from_str_radix(&can_id_str[2..], 16)?
            } else {
                can_id_str.parse()?
            };

            // Parse data as hex bytes
            let data: Vec<u8> = (0..data_str.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&data_str[i..i + 2], 16))
                .collect::<Result<Vec<_>, _>>()?;

            println!("Sending frame: ID=0x{:x}, Data={:02x?}", can_id, data);
            let frame = socketcan::CanFrame::new(can_id, &data)?;
            socket.write_frame(&frame)?;
            println!("Frame sent!");
        } else {
            println!("Error: --send requires <id> and <data> arguments");
            return Ok(());
        }
    }

    println!("\nListening for CAN frames (Ctrl+C to exit)...\n");

    // Read and display frames
    loop {
        match socket.read_frame()? {
            Some(frame) => {
                let type_str = if frame.is_fd() { "FD" } else { "  " };
                println!(
                    "[{}] ID=0x{:08x} Len={:2} Data={:02x?}",
                    type_str,
                    frame.id(),
                    frame.data().len(),
                    frame.data()
                );
            }
            None => {
                // Timeout, continue waiting
                continue;
            }
        }
    }
}
