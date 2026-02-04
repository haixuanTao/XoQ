//! MoQ serial bridge server - uses MoQ relay instead of iroh P2P
//!
//! Usage: moq_serial_server <port> [baud_rate] [moq_path]
//! Example: moq_serial_server /dev/ttyUSB0 115200 anon/xoq-serial

use anyhow::Result;
use std::env;
use std::time::{Duration, Instant};

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
        println!("Usage: moq_serial_server <port> [baud_rate] [moq_path]");
        println!("Example: moq_serial_server /dev/ttyUSB0 115200 anon/xoq-serial");
        println!("\nAvailable ports:");
        for port in xoq::list_ports()? {
            println!("  {} - {:?}", port.name, port.port_type);
        }
        return Ok(());
    }

    let port_name = &args[1];
    let baud_rate: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(115200);
    let moq_path = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "anon/xoq-serial".to_string());

    // Open serial port
    let serial = xoq::serial::SerialPort::open_simple(port_name, baud_rate)?;
    let (mut reader, mut writer) = serial.split();

    tracing::info!("Serial port opened: {} @ {} baud", port_name, baud_rate);

    // Connect to MoQ relay as duplex
    tracing::info!("Connecting to MoQ relay at path '{}'...", moq_path);
    let mut conn = xoq::MoqBuilder::new()
        .path(&moq_path)
        .connect_duplex()
        .await?;

    tracing::info!("Connected to MoQ relay");

    // Create track for serial -> network (server publishes serial data)
    let mut serial_out_track = conn.create_track("serial-out");
    tracing::info!("Publishing on track 'serial-out'");

    // Subscribe to track for network -> serial (client publishes commands)
    tracing::info!("Waiting for client to publish on 'serial-in'...");
    let serial_in_reader = conn.subscribe_track("serial-in").await?;

    let mut serial_in_reader = match serial_in_reader {
        Some(r) => {
            tracing::info!("Subscribed to 'serial-in' track");
            r
        }
        None => {
            tracing::error!("Failed to subscribe to 'serial-in' track");
            return Err(anyhow::anyhow!("No serial-in track"));
        }
    };

    // Spawn task: network -> serial
    let net_to_serial = tokio::spawn(async move {
        let mut last_recv = Instant::now();
        loop {
            match serial_in_reader.read().await {
                Ok(Some(data)) => {
                    let gap = last_recv.elapsed();
                    if gap > Duration::from_millis(50) {
                        tracing::warn!(
                            "MoQ Net→Serial: {:.1}ms gap ({} bytes)",
                            gap.as_secs_f64() * 1000.0,
                            data.len(),
                        );
                    }
                    last_recv = Instant::now();
                    tracing::debug!("MoQ Net→Serial: {} bytes", data.len());
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::error!("Serial write error: {}", e);
                        break;
                    }
                }
                Ok(None) => {
                    tracing::info!("MoQ serial-in track ended");
                    break;
                }
                Err(e) => {
                    tracing::error!("MoQ read error: {}", e);
                    break;
                }
            }
        }
    });

    // Main task: serial -> network
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                tokio::task::yield_now().await;
            }
            Ok(n) => {
                tracing::debug!("Serial→MoQ: {} bytes", n);
                serial_out_track.write(buf[..n].to_vec());
            }
            Err(e) => {
                tracing::error!("Serial read error: {}", e);
                break;
            }
        }
    }

    net_to_serial.abort();
    Ok(())
}
