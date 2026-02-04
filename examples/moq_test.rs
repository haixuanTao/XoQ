//! MoQ relay diagnostic test
//!
//! Tests publish and subscribe through the relay to verify announcements work.
//!
//! Usage:
//!   # Publisher mode (run first):
//!   RUST_LOG=moq_lite=debug,info cargo run --example moq_test -- pub
//!
//!   # Subscriber mode (run second, in another terminal):
//!   RUST_LOG=moq_lite=debug,info cargo run --example moq_test -- sub
//!
//!   # Or run both in one process:
//!   RUST_LOG=moq_lite=debug,info cargo run --example moq_test -- both

use anyhow::Result;
use std::time::Duration;

const TEST_PATH: &str = "anon/xoq-diag-test";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let mode = std::env::args().nth(1).unwrap_or_default();

    match mode.as_str() {
        "pub" => run_publisher().await,
        "sub" => run_subscriber().await,
        "both" => run_both().await,
        _ => {
            eprintln!("Usage: moq_test <pub|sub|both>");
            eprintln!("  pub  - Run publisher only");
            eprintln!("  sub  - Run subscriber only");
            eprintln!("  both - Run publisher then subscriber in same process");
            Ok(())
        }
    }
}

async fn run_publisher() -> Result<()> {
    eprintln!(
        "[test] Connecting publisher to relay at path '{}'...",
        TEST_PATH
    );

    let mut publisher = xoq::MoqBuilder::new()
        .path(TEST_PATH)
        .connect_publisher()
        .await?;

    eprintln!("[test] Publisher connected! Creating track 'video'...");

    let mut track = publisher.create_track("video");

    eprintln!("[test] Track created. Sending frames every second...");

    for i in 0u64.. {
        let msg = format!("frame-{}", i);
        track.write_str(&msg);
        eprintln!("[test] Sent: {}", msg);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

async fn run_subscriber() -> Result<()> {
    eprintln!(
        "[test] Connecting subscriber to relay at path '{}'...",
        TEST_PATH
    );

    let mut subscriber = xoq::MoqBuilder::new()
        .path(TEST_PATH)
        .connect_subscriber()
        .await?;

    eprintln!("[test] Subscriber connected! Waiting for track 'video'...");

    match subscriber.subscribe_track("video").await {
        Ok(Some(mut reader)) => {
            eprintln!("[test] Subscribed to track! Reading frames...");
            for _ in 0..10 {
                match reader.read_string().await {
                    Ok(Some(data)) => eprintln!("[test] Received: {}", data),
                    Ok(None) => {
                        eprintln!("[test] Track ended (read returned None)");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[test] Read error: {}", e);
                        break;
                    }
                }
            }
        }
        Ok(None) => eprintln!("[test] No broadcast found (announced returned None)"),
        Err(e) => eprintln!("[test] Subscribe failed: {}", e),
    }

    Ok(())
}

async fn run_both() -> Result<()> {
    eprintln!("[test] === Running subscriber-first, then publisher ===");
    eprintln!("[test] Path: {}", TEST_PATH);

    // Start subscriber FIRST
    eprintln!("\n[test] --- Starting subscriber ---");
    let mut subscriber = xoq::MoqBuilder::new()
        .path(TEST_PATH)
        .connect_subscriber()
        .await?;
    eprintln!("[test] Subscriber connected! Waiting for announcements in background...");

    // Start publisher SECOND (after subscriber is waiting)
    eprintln!("\n[test] --- Starting publisher ---");
    let mut publisher = xoq::MoqBuilder::new()
        .path(TEST_PATH)
        .connect_publisher()
        .await?;

    eprintln!("[test] Publisher connected!");
    let mut track = publisher.create_track("video");
    track.write_str("hello-from-publisher");
    eprintln!("[test] Published 'hello-from-publisher' to track 'video'");

    // Now check if subscriber receives the announcement
    eprintln!("\n[test] --- Checking subscriber ---");
    match subscriber.subscribe_track("video").await {
        Ok(Some(mut reader)) => {
            eprintln!("[test] Subscribed! Reading...");
            match tokio::time::timeout(Duration::from_secs(5), reader.read_string()).await {
                Ok(Ok(Some(data))) => eprintln!("[test] SUCCESS - Received: {}", data),
                Ok(Ok(None)) => eprintln!("[test] FAIL - Track ended without data"),
                Ok(Err(e)) => eprintln!("[test] FAIL - Read error: {}", e),
                Err(_) => eprintln!("[test] FAIL - Read timed out after 5s"),
            }
        }
        Ok(None) => eprintln!("[test] FAIL - No broadcast announced"),
        Err(e) => eprintln!("[test] FAIL - Subscribe error: {}", e),
    }

    Ok(())
}
