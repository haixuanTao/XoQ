//! Serial port bridge server - all forwarding handled internally
//!
//! Usage: serial_server <port> [baud_rate]
//! Example: serial_server /dev/ttyUSB0 115200

use anyhow::Result;
use std::env;
use wser::Server;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: serial_server <port> [baud_rate]");
        println!("Example: serial_server /dev/ttyUSB0 115200");
        println!("\nAvailable ports:");
        for port in wser::list_ports()? {
            println!("  {} - {:?}", port.name, port.port_type);
        }
        return Ok(());
    }

    let port_name = &args[1];
    let baud_rate: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(115200);

    // Create bridge server - opens serial port and starts iroh
    let bridge = Server::new(
        port_name,
        baud_rate,
        Some(".wser_serial_bridge_key"),
    )
    .await?;

    tracing::info!("Serial bridge server started");
    tracing::info!("Port: {} @ {} baud", port_name, baud_rate);
    tracing::info!("Server ID: {}", bridge.id());
    tracing::info!("Waiting for connections...");

    // Run forever - all forwarding handled internally
    bridge.run().await?;

    Ok(())
}
