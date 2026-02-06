//! MoQ relay test
//!
//! Tests publish and subscribe through a MoQ relay.
//!
//! Usage:
//!   # One-way pub/sub test (two terminals):
//!   cargo run --example moq_test -- pub
//!   cargo run --example moq_test -- sub
//!
//!   # Bidirectional MoqStream test (single process):
//!   cargo run --example moq_test -- stream

use anyhow::Result;
use std::time::Duration;

const TEST_PATH: &str = "anon/xoq-test";
const DEFAULT_RELAY: &str = "https://172.18.133.111:4443";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_RELAY);

    match mode {
        "pub" => run_publisher(relay).await,
        "sub" => run_subscriber(relay).await,
        "stream" => run_stream_test(relay).await,
        _ => {
            eprintln!("Usage: moq_test <pub|sub|stream> [relay-url]");
            eprintln!();
            eprintln!("  pub    - Run publisher (one-way test)");
            eprintln!("  sub    - Run subscriber (one-way test)");
            eprintln!("  stream - Bidirectional MoqStream test (server + client)");
            eprintln!();
            eprintln!("Default relay: {}", DEFAULT_RELAY);
            Ok(())
        }
    }
}

async fn run_publisher(relay: &str) -> Result<()> {
    eprintln!("[pub] Connecting to {} at path '{}'...", relay, TEST_PATH);

    let (_publisher, mut track) = xoq::MoqBuilder::new()
        .relay(relay)
        .path(TEST_PATH)
        .disable_tls_verify()
        .connect_publisher_with_track("video")
        .await?;

    eprintln!("[pub] Connected! Sending frames...");

    for i in 0u64.. {
        let msg = format!("frame-{}", i);
        track.write_str(&msg);
        eprintln!("[pub] Sent: {}", msg);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

async fn run_subscriber(relay: &str) -> Result<()> {
    eprintln!("[sub] Connecting to {} at path '{}'...", relay, TEST_PATH);

    let mut subscriber = xoq::MoqBuilder::new()
        .relay(relay)
        .path(TEST_PATH)
        .disable_tls_verify()
        .connect_subscriber()
        .await?;

    eprintln!("[sub] Connected! Subscribing to track 'video'...");

    match subscriber.subscribe_track("video").await {
        Ok(Some(mut reader)) => {
            eprintln!("[sub] Subscribed! Reading frames...");
            loop {
                match reader.read_string().await {
                    Ok(Some(data)) => eprintln!("[sub] Received: {}", data),
                    Ok(None) => {
                        eprintln!("[sub] Track ended");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[sub] Error: {}", e);
                        break;
                    }
                }
            }
        }
        Ok(None) => eprintln!("[sub] Subscribe returned None"),
        Err(e) => eprintln!("[sub] Subscribe failed: {}", e),
    }

    Ok(())
}

async fn run_stream_test(relay: &str) -> Result<()> {
    eprintln!("[test] === MoqStream bidirectional test ===");
    eprintln!("[test] Relay: {}", relay);
    eprintln!("[test] Path: {}", TEST_PATH);

    // Use a channel to signal when both sides are ready
    let (server_ready_tx, server_ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (client_ready_tx, client_ready_rx) = tokio::sync::oneshot::channel::<()>();

    let relay_server = relay.to_string();
    let relay_client = relay.to_string();

    let server = tokio::spawn(async move {
        eprintln!("[server] Accepting...");
        let mut stream = xoq::MoqStream::accept_at_insecure(&relay_server, TEST_PATH).await?;
        eprintln!("[server] Connected!");

        // Signal ready and wait for client
        let _ = server_ready_tx.send(());
        let _ = client_ready_rx.await;

        // Write after both connected
        eprintln!("[server] Writing 'hello-from-server'...");
        stream.write(bytes::Bytes::from("hello-from-server"));

        // Read
        eprintln!("[server] Reading...");
        match tokio::time::timeout(Duration::from_secs(10), stream.read()).await {
            Ok(Ok(Some(data))) => {
                eprintln!(
                    "[server] SUCCESS: Received '{}'",
                    String::from_utf8_lossy(&data)
                );
            }
            Ok(Ok(None)) => eprintln!("[server] FAIL: Stream ended"),
            Ok(Err(e)) => eprintln!("[server] FAIL: Read error: {}", e),
            Err(_) => eprintln!("[server] FAIL: Read timed out"),
        }

        // Keep stream alive briefly so client can read
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<_, anyhow::Error>(())
    });

    // Small delay to let server connect first
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = tokio::spawn(async move {
        eprintln!("[client] Connecting...");
        let mut stream = xoq::MoqStream::connect_to_insecure(&relay_client, TEST_PATH).await?;
        eprintln!("[client] Connected!");

        // Signal ready and wait for server
        let _ = client_ready_tx.send(());
        let _ = server_ready_rx.await;

        // Write after both connected
        eprintln!("[client] Writing 'hello-from-client'...");
        stream.write(bytes::Bytes::from("hello-from-client"));

        // Read
        eprintln!("[client] Reading...");
        match tokio::time::timeout(Duration::from_secs(10), stream.read()).await {
            Ok(Ok(Some(data))) => {
                eprintln!(
                    "[client] SUCCESS: Received '{}'",
                    String::from_utf8_lossy(&data)
                );
            }
            Ok(Ok(None)) => eprintln!("[client] FAIL: Stream ended"),
            Ok(Err(e)) => eprintln!("[client] FAIL: Read error: {}", e),
            Err(_) => eprintln!("[client] FAIL: Read timed out"),
        }
        Ok::<_, anyhow::Error>(())
    });

    let (server_res, client_res) = tokio::join!(server, client);
    if let Err(e) = server_res? {
        eprintln!("[server] Error: {}", e);
    }
    if let Err(e) = client_res? {
        eprintln!("[client] Error: {}", e);
    }

    Ok(())
}
