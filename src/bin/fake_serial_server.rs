//! Fake serial server - simulates a serial echo device over iroh P2P + optional MoQ.
//!
//! Drop-in replacement for serial-server that doesn't need serial hardware.
//! Accepts iroh connections from clients, echoes received data back, and
//! optionally publishes echo data to MoQ for browser monitoring.
//! MoQ commands are also received and echoed (via BridgeServer).
//!
//! Usage:
//!   fake-serial-server [OPTIONS]
//!
//! Options:
//!   --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)
//!   --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)
//!   --moq-insecure       Disable TLS verification for MoQ
//!   --key-dir <path>     Directory for identity key files (default: current dir)
//!
//! Examples:
//!   fake-serial-server                                             # iroh only
//!   fake-serial-server --moq-relay https://cdn.1ms.ai              # iroh + MoQ

use anyhow::Result;
use tokio::sync::mpsc;
use xoq::bridge_server::{BridgeServer, MoqConfig};

struct Args {
    iroh_relay: Option<String>,
    moq_relay: Option<String>,
    moq_path: String,
    moq_insecure: bool,
    key_dir: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut result = Args {
        iroh_relay: None,
        moq_relay: None,
        moq_path: "anon/xoq-serial".to_string(),
        moq_insecure: false,
        key_dir: ".".to_string(),
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--iroh-relay" if i + 1 < args.len() => {
                result.iroh_relay = Some(args[i + 1].clone());
                i += 2;
            }
            "--moq-relay" if i + 1 < args.len() => {
                result.moq_relay = Some(args[i + 1].clone());
                i += 2;
            }
            "--moq-path" if i + 1 < args.len() => {
                result.moq_path = args[i + 1].clone();
                i += 2;
            }
            "--moq-insecure" => {
                result.moq_insecure = true;
                i += 1;
            }
            "--key-dir" if i + 1 < args.len() => {
                result.key_dir = args[i + 1].clone();
                i += 2;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                i += 1;
            }
        }
    }

    result
}

fn print_usage() {
    println!("Fake Serial Server - simulates serial echo device over iroh P2P + MoQ");
    println!();
    println!("Usage: fake-serial-server [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)");
    println!("  --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)");
    println!("  --moq-insecure       Disable TLS verification for MoQ");
    println!("  --key-dir <path>     Directory for identity key files (default: .)");
    println!();
    println!("Examples:");
    println!("  fake-serial-server                                             # iroh only");
    println!("  fake-serial-server --moq-relay https://cdn.1ms.ai              # iroh + MoQ");
}

/// Echo backend task.
///
/// Receives data from write_rx (from both iroh and MoQ commands),
/// echoes it to read_tx (for iroh) and moq_read_tx (for MoQ state publishing).
async fn echo_task(
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    read_tx: mpsc::Sender<Vec<u8>>,
    moq_read_tx: Option<mpsc::Sender<Vec<u8>>>,
) {
    while let Some(data) = write_rx.recv().await {
        tracing::debug!("Echo: {} bytes", data.len());
        // Echo back to network
        if read_tx.send(data.clone()).await.is_err() {
            break;
        }
        // Also publish to MoQ
        if let Some(ref moq_tx) = moq_read_tx {
            let _ = moq_tx.try_send(data);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("warn".parse()?),
        )
        .init();

    let args = parse_args();

    println!();
    println!("========================================");
    println!("Fake Serial Server (echo)");
    println!("========================================");
    if let Some(ref relay) = args.moq_relay {
        println!("MoQ relay: {}", relay);
        println!("MoQ path:  {}", args.moq_path);
        println!("MoQ s2c:   {}/s2c", args.moq_path);
        println!("MoQ c2s:   {}/c2s", args.moq_path);
    } else {
        println!("MoQ:       disabled");
    }
    println!("Mode:      echo (returns received data)");
    println!("========================================");
    println!();

    // Create channels between echo backend and BridgeServer
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(1);
    let (read_tx, read_rx) = mpsc::channel::<Vec<u8>>(32);

    let (moq_read_tx, moq_read_rx) = if args.moq_relay.is_some() {
        let (tx, rx) = mpsc::channel(128);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Spawn echo backend task
    tokio::spawn(async move {
        echo_task(write_rx, read_tx, moq_read_tx).await;
    });

    // Create MoQ config
    let moq_config = args.moq_relay.as_ref().map(|relay| MoqConfig {
        relay: relay.clone(),
        path: args.moq_path.clone(),
        insecure: args.moq_insecure,
        state_subpath: "s2c".to_string(),
        command_subpath: "c2s".to_string(),
        track_name: "data".to_string(),
    });

    // Create and run BridgeServer
    let identity_path = format!("{}/.xoq_fake_serial_server_key", args.key_dir);
    let bridge = BridgeServer::new(
        Some(&identity_path),
        args.iroh_relay.as_deref(),
        write_tx,
        read_rx,
        moq_read_rx,
        moq_config,
    )
    .await?;

    tracing::info!("Server ID: {}", bridge.id());
    println!("Server ID: {}", bridge.id());
    println!();

    tracing::info!("Waiting for iroh connections...");
    bridge.run().await
}
