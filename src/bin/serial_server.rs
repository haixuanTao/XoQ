//! Serial port bridge server - iroh P2P with optional MoQ
//!
//! Usage: serial-server <port> [baud_rate] [OPTIONS]
//!
//! Options:
//!   --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)
//!   --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)
//!   --moq-insecure       Disable TLS verification for MoQ
//!   --key-dir <path>     Directory for identity key files (default: current dir)
//!
//! Examples:
//!   serial-server /dev/ttyUSB0 115200                                           # iroh only
//!   serial-server /dev/ttyUSB0 1000000 --moq-relay https://cdn.1ms.ai           # iroh + MoQ
//!   serial-server /dev/ttyUSB0 1000000 --moq-relay https://cdn.1ms.ai --moq-path anon/my-serial

use anyhow::Result;
use std::env;
use std::path::PathBuf;

struct Args {
    port: String,
    baud_rate: u32,
    moq_relay: Option<String>,
    moq_path: Option<String>,
    moq_insecure: bool,
    key_dir: PathBuf,
}

fn parse_args() -> Option<Args> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        return None;
    }

    let mut port = None;
    let mut baud_rate = 115200u32;
    let mut moq_relay = None;
    let mut moq_path = None;
    let mut moq_insecure = false;
    let mut key_dir = PathBuf::from(".");
    let mut i = 1;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--moq-relay" {
            if i + 1 < args.len() {
                moq_relay = Some(args[i + 1].clone());
                i += 2;
                continue;
            } else {
                eprintln!("Error: --moq-relay requires a URL argument");
                return None;
            }
        }

        if arg == "--moq-path" {
            if i + 1 < args.len() {
                moq_path = Some(args[i + 1].clone());
                i += 2;
                continue;
            } else {
                eprintln!("Error: --moq-path requires a path argument");
                return None;
            }
        }

        if arg == "--moq-insecure" {
            moq_insecure = true;
            i += 1;
            continue;
        }

        if arg == "--key-dir" {
            if i + 1 < args.len() {
                key_dir = PathBuf::from(&args[i + 1]);
                i += 2;
                continue;
            } else {
                eprintln!("Error: --key-dir requires a path argument");
                return None;
            }
        }

        if arg == "--help" || arg == "-h" {
            return None;
        }

        // Skip legacy --moq flag
        if arg == "--moq" {
            i += 1;
            // Skip optional moq_path argument after --moq
            if i < args.len() && !args[i].starts_with('-') {
                i += 1;
            }
            continue;
        }

        // First positional arg is port, second is baud_rate
        if port.is_none() {
            port = Some(arg.clone());
        } else if let Ok(br) = arg.parse::<u32>() {
            baud_rate = br;
        }
        i += 1;
    }

    let port = port?;

    Some(Args {
        port,
        baud_rate,
        moq_relay,
        moq_path,
        moq_insecure,
        key_dir,
    })
}

fn print_usage() {
    println!("Usage: serial-server <port> [baud_rate] [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)");
    println!("  --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)");
    println!("  --moq-insecure       Disable TLS verification for MoQ");
    println!("  --key-dir <path>     Directory for identity key files (default: current dir)");
    println!();
    println!("Examples:");
    println!(
        "  serial-server /dev/ttyUSB0 115200                                         # iroh only"
    );
    println!(
        "  serial-server /dev/ttyUSB0 1000000 --moq-relay https://cdn.1ms.ai         # iroh + MoQ"
    );
    println!("  serial-server /dev/ttyUSB0 1000000 --moq-relay https://cdn.1ms.ai --moq-path anon/my-serial");
    println!();
    println!("Available ports:");
    match xoq::list_ports() {
        Ok(ports) => {
            if ports.is_empty() {
                println!("  (none found)");
            } else {
                for port in ports {
                    println!("  {} - {:?}", port.name, port.port_type);
                }
            }
        }
        Err(e) => println!("  Error listing ports: {}", e),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=debug".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let args = match parse_args() {
        Some(a) => a,
        None => {
            print_usage();
            return Ok(());
        }
    };

    let identity_path = args
        .key_dir
        .join(".xoq_serial_server_key")
        .to_string_lossy()
        .to_string();

    let bridge = xoq::Server::new(
        &args.port,
        args.baud_rate,
        Some(&identity_path),
        args.moq_relay.as_deref(),
        args.moq_path.as_deref(),
        args.moq_insecure,
    )
    .await?;

    tracing::info!("Serial bridge server started");
    tracing::info!("Port: {} @ {} baud", args.port, args.baud_rate);
    tracing::info!("Server ID: {}", bridge.id());
    tracing::info!("Identity: {}", identity_path);
    if let Some(ref relay) = args.moq_relay {
        tracing::info!(
            "MoQ relay: {}{}",
            relay,
            if args.moq_insecure { " (insecure)" } else { "" }
        );
        tracing::info!(
            "MoQ path: {}",
            args.moq_path.as_deref().unwrap_or("anon/xoq-serial")
        );
    }
    tracing::info!("Waiting for connections...");

    bridge.run().await?;
    Ok(())
}
