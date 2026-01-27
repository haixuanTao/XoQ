//! CAN bridge server - bridges local CAN interface to remote clients
//!
//! Usage: can_server <interface> [--fd]
//! Example: can_server can0
//! Example: can_server can0 --fd  (enable CAN FD)

use anyhow::Result;
use std::env;
use xoq::CanServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=debug".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: can_server <interface> [--fd]");
        println!("Example: can_server can0");
        println!("Example: can_server can0 --fd  (enable CAN FD)");
        println!("\nAvailable CAN interfaces:");
        match xoq::list_interfaces() {
            Ok(interfaces) => {
                if interfaces.is_empty() {
                    println!("  (none found)");
                    println!("\nTo create a virtual CAN interface for testing:");
                    println!("  sudo modprobe vcan");
                    println!("  sudo ip link add dev vcan0 type vcan");
                    println!("  sudo ip link set up vcan0");
                } else {
                    for iface in interfaces {
                        println!("  {}", iface.name);
                    }
                }
            }
            Err(e) => println!("  Error listing interfaces: {}", e),
        }
        return Ok(());
    }

    let interface = &args[1];
    let enable_fd = args.iter().any(|a| a == "--fd");

    // Create bridge server - opens CAN interface and starts iroh
    // Use persistent identity so server ID stays the same across restarts
    let server = CanServer::new(interface, enable_fd, Some(".xoq_can_server_key")).await?;

    tracing::info!("CAN bridge server started");
    tracing::info!("Interface: {} (FD: {})", interface, enable_fd);
    tracing::info!("Server ID: {}", server.id());
    tracing::info!("Waiting for connections...");

    // Run forever - all forwarding handled internally
    server.run().await?;

    Ok(())
}
