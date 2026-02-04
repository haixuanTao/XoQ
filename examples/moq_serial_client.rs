//! MoQ serial bridge client - connects to MoQ serial server
//!
//! Usage: moq_serial_client [moq_path]
//! Example: moq_serial_client anon/xoq-serial
//!
//! Reads from stdin, sends to server. Prints server responses to stdout.

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

    let moq_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "anon/xoq-serial".to_string());

    tracing::info!("Connecting to MoQ relay at path '{}'...", moq_path);

    let mut conn = xoq::MoqBuilder::new()
        .path(&moq_path)
        .connect_duplex()
        .await?;

    tracing::info!("Connected to MoQ relay");

    // Create track for client -> server (we publish commands)
    let mut serial_in_track = conn.create_track("serial-in");
    tracing::info!("Publishing on track 'serial-in'");

    // Subscribe to server's serial output
    tracing::info!("Waiting for server to publish 'serial-out'...");
    let serial_out_reader = conn.subscribe_track("serial-out").await?;

    let mut serial_out_reader = match serial_out_reader {
        Some(r) => {
            tracing::info!("Subscribed to 'serial-out' track");
            r
        }
        None => {
            tracing::error!("Failed to subscribe to 'serial-out' track");
            return Err(anyhow::anyhow!("No serial-out track"));
        }
    };

    // Spawn task: read from server and print
    let recv_task = tokio::spawn(async move {
        let mut last_recv = Instant::now();
        loop {
            match serial_out_reader.read().await {
                Ok(Some(data)) => {
                    let gap = last_recv.elapsed();
                    if gap > Duration::from_millis(50) {
                        tracing::warn!(
                            "MoQ Serial←Server: {:.1}ms gap ({} bytes)",
                            gap.as_secs_f64() * 1000.0,
                            data.len(),
                        );
                    }
                    last_recv = Instant::now();
                    print!("{}", String::from_utf8_lossy(&data));
                }
                Ok(None) => {
                    tracing::info!("Server track ended");
                    break;
                }
                Err(e) => {
                    tracing::error!("Read error: {}", e);
                    break;
                }
            }
        }
    });

    // Main: read from stdin and send to server
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 1024];
    loop {
        use tokio::io::AsyncReadExt;
        match stdin.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                serial_in_track.write(buf[..n].to_vec());
            }
            Err(e) => {
                tracing::error!("Stdin error: {}", e);
                break;
            }
        }
    }

    recv_task.abort();
    Ok(())
}
