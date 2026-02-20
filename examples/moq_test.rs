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
//!
//!   # CAN command test (simulates browser querying motors):
//!   cargo run --example moq_test -- cmd <base-path> [relay]
//!   cargo run --example moq_test -- cmd anon/a13af1d39199/xoq-can-can0
//!   cargo run --example moq_test -- cmd anon/a13af1d39199/xoq-can-can0 https://cdn.1ms.ai

use anyhow::Result;
use std::time::Duration;

const TEST_PATH: &str = "anon/xoq-test";
const DEFAULT_RELAY: &str = "https://172.18.133.111:4443";
const DEFAULT_CMD_RELAY: &str = "https://cdn.1ms.ai";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match mode {
        "pub" => {
            let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_RELAY);
            run_publisher(relay).await
        }
        "sub" => {
            let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_RELAY);
            run_subscriber(relay).await
        }
        "stream" => {
            let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_RELAY);
            run_stream_test(relay).await
        }
        "reannounce" => {
            let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_CMD_RELAY);
            run_reannounce_test(relay).await
        }
        "cmd" => {
            let base_path = match args.get(2) {
                Some(p) => p.as_str(),
                None => {
                    eprintln!("Usage: moq_test cmd <base-path> [relay]");
                    eprintln!();
                    eprintln!("  base-path: CAN server MoQ path (e.g. anon/NODE_ID/xoq-can-can0)");
                    eprintln!("  relay:     Relay URL (default: {})", DEFAULT_CMD_RELAY);
                    eprintln!();
                    eprintln!("This simulates what openarm.html does:");
                    eprintln!("  1. Subscribe to <base-path>/state track 'can' (read motor state)");
                    eprintln!("  2. Publish on <base-path>/commands track 'can' (send zero-torque queries)");
                    return Ok(());
                }
            };
            let relay = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_CMD_RELAY);
            run_cmd_test(relay, base_path).await
        }
        _ => {
            eprintln!("Usage: moq_test <pub|sub|stream|cmd> [args...]");
            eprintln!();
            eprintln!("  pub              - Run publisher (one-way test)");
            eprintln!("  sub              - Run subscriber (one-way test)");
            eprintln!("  stream           - Bidirectional MoqStream test (server + client)");
            eprintln!("  cmd <path> [url] - CAN command test (simulates browser motor query)");
            eprintln!();
            eprintln!("Default relay: {}", DEFAULT_RELAY);
            eprintln!("Default cmd relay: {}", DEFAULT_CMD_RELAY);
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

// ─── CAN command test ──────────────────────────────────────

/// Encode a CAN wire frame: [flags(1), canId(4 LE), len(1), data(len)]
/// Matches the wire format in xoq::can_types::wire and JS encodeCanFrame()
fn encode_can_wire_frame(can_id: u32, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6 + data.len());
    buf.push(0x00); // flags: standard CAN
    buf.extend_from_slice(&can_id.to_le_bytes());
    buf.push(data.len() as u8);
    buf.extend_from_slice(data);
    buf
}

/// Decode a CAN wire frame, returns (can_id, data, bytes_consumed)
fn decode_can_wire_frame(buf: &[u8]) -> Option<(u32, Vec<u8>, usize)> {
    if buf.len() < 6 {
        return None;
    }
    let _flags = buf[0];
    let can_id = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
    let data_len = buf[5] as usize;
    if buf.len() < 6 + data_len {
        return None;
    }
    let data = buf[6..6 + data_len].to_vec();
    Some((can_id, data, 6 + data_len))
}

/// MIT zero-torque query command (p=0, v=0, kp=0, kd=0, t=0)
/// Same encoding as JS encodeMitZeroTorque()
fn mit_zero_torque_cmd() -> [u8; 8] {
    let p: u16 = 0x8000;
    let v: u16 = 0x0800;
    let kp: u16 = 0;
    let kd: u16 = 0;
    let t: u16 = 0x0800;
    [
        (p >> 8) as u8,
        (p & 0xFF) as u8,
        (v >> 4) as u8,
        (((v & 0xF) << 4) | (kp >> 8)) as u8,
        (kp & 0xFF) as u8,
        (kd >> 4) as u8,
        (((kd & 0xF) << 4) | (t >> 8)) as u8,
        (t & 0xFF) as u8,
    ]
}

/// Parse a Damiao motor response (8 bytes) into human-readable state
fn parse_damiao_response(data: &[u8]) -> Option<String> {
    if data.len() < 8 {
        return None;
    }
    let motor_id = data[0];
    let p_raw = ((data[1] as u16) << 8) | (data[2] as u16);
    let v_raw = ((data[3] as u16) << 4) | ((data[4] as u16) >> 4);
    let t_raw = (((data[4] & 0x0F) as u16) << 8) | (data[5] as u16);

    // Convert from raw to physical units
    let p_min: f64 = -12.5;
    let p_max: f64 = 12.5;
    let v_min: f64 = -45.0;
    let v_max: f64 = 45.0;
    let t_min: f64 = -18.0;
    let t_max: f64 = 18.0;

    let position = p_min + (p_raw as f64 / 65535.0) * (p_max - p_min);
    let velocity = v_min + (v_raw as f64 / 4095.0) * (v_max - v_min);
    let torque = t_min + (t_raw as f64 / 4095.0) * (t_max - t_min);

    Some(format!(
        "motor={} pos={:.3}rad vel={:.2} tau={:.2}",
        motor_id, position, velocity, torque
    ))
}

/// Test the reannounce flow with a STALE broadcast.
/// 1. First publisher connects briefly then disconnects (creates stale broadcast on relay)
/// 2. Subscriber connects and gets the stale broadcast
/// 3. Second publisher connects (triggers reannounce)
/// 4. Subscriber should switch and read data from the new publisher
///
/// This mimics exactly the CAN server's scenario where the relay caches
/// old broadcasts from previous sessions.
async fn run_reannounce_test(relay: &str) -> Result<()> {
    use moq_native::moq_lite::{Origin, Track};

    let test_path = "anon/xoq-reannounce-test/commands";
    eprintln!("=== Reannounce Test ===");
    eprintln!("Relay: {}", relay);
    eprintln!("Path:  {}", test_path);
    eprintln!();

    let relay_stale = relay.to_string();
    let relay_sub = relay.to_string();
    let relay_pub = relay.to_string();

    // Use a channel to coordinate: subscriber signals when it's subscribed to stale broadcast
    let (sub_ready_tx, sub_ready_rx) = tokio::sync::oneshot::channel::<()>();

    // 0. Create a stale broadcast by publishing briefly, then disconnecting
    eprintln!("[stale] Creating stale broadcast...");
    {
        let (_publisher, mut track) = xoq::MoqBuilder::new()
            .relay(&relay_stale)
            .path(test_path)
            .connect_publisher_with_track("can")
            .await?;
        track.write(bytes::Bytes::from("stale-data"));
        eprintln!("[stale] Published 1 frame, waiting 2s then disconnecting...");
        tokio::time::sleep(Duration::from_secs(2)).await;
        // publisher drops here, creating a "stale" broadcast on the relay
    }
    eprintln!("[stale] Disconnected. Relay should cache the stale broadcast.");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 1. Start subscriber (like CAN server — connects and finds stale broadcast)
    let sub_handle = tokio::spawn(async move {
        let builder = xoq::MoqBuilder::new().relay(&relay_sub);
        let url = builder.build_url_for_path(test_path).unwrap();
        let client = builder.create_client_public().unwrap();
        let origin = Origin::produce();
        let _session = client
            .with_consume(origin.producer)
            .connect(url)
            .await
            .unwrap();
        let mut origin_consumer = origin.consumer;

        eprintln!("[sub] Connected, waiting for initial announcement...");

        // Wait for initial broadcast (may be stale or real)
        let broadcast = match tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match origin_consumer.announced().await {
                    Some((_p, Some(bc))) => return Some(bc),
                    Some((_p, None)) => continue,
                    None => return None,
                }
            }
        })
        .await
        {
            Ok(Some(bc)) => {
                eprintln!("[sub] Got initial broadcast (may be stale)");
                bc
            }
            Ok(None) => {
                eprintln!("[sub] Origin closed");
                return;
            }
            Err(_) => {
                eprintln!("[sub] No initial broadcast (timeout). Waiting for publisher...");
                match tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        match origin_consumer.announced().await {
                            Some((_p, Some(bc))) => return Some(bc),
                            Some((_p, None)) => continue,
                            None => return None,
                        }
                    }
                })
                .await
                {
                    Ok(Some(bc)) => {
                        eprintln!("[sub] Got broadcast after waiting");
                        bc
                    }
                    _ => {
                        eprintln!("[sub] FAIL: never got broadcast");
                        return;
                    }
                }
            }
        };

        let track_consumer = broadcast.subscribe_track(&Track::new("can"));
        let mut reader = xoq::MoqTrackReader::from_track(track_consumer);
        eprintln!("[sub] Subscribed to track 'can', entering select! loop...");
        let _ = sub_ready_tx.send(()); // signal that we're subscribed

        let mut msg_count = 0u32;
        loop {
            tokio::select! {
                read_result = reader.read() => {
                    match read_result {
                        Ok(Some(data)) => {
                            msg_count += 1;
                            eprintln!("[sub] READ #{}: {} bytes: {:?}", msg_count, data.len(),
                                String::from_utf8_lossy(&data));
                        }
                        Ok(None) => {
                            eprintln!("[sub] Track ended after {} reads", msg_count);
                            break;
                        }
                        Err(e) => {
                            eprintln!("[sub] Read error after {} reads: {}", msg_count, e);
                            break;
                        }
                    }
                }
                announce = origin_consumer.announced() => {
                    match announce {
                        Some((_path, Some(new_bc))) => {
                            eprintln!("[sub] REANNOUNCE: new broadcast! Switching...");
                            let new_track = new_bc.subscribe_track(&Track::new("can"));
                            reader = xoq::MoqTrackReader::from_track(new_track);
                            eprintln!("[sub] Switched to new track, continuing read loop...");
                        }
                        Some((_path, None)) => {
                            eprintln!("[sub] UNANNOUNCE (broadcast going away)");
                        }
                        None => {
                            eprintln!("[sub] Origin closed");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(15)) => {
                    eprintln!("[sub] Timeout (15s), no data received. msg_count={}", msg_count);
                    break;
                }
            }
        }
    });

    // 2. Wait for subscriber to be subscribed to stale broadcast, then start publisher
    let pub_handle = tokio::spawn(async move {
        eprintln!("[pub] Waiting for subscriber to be subscribed to stale broadcast...");
        let _ = sub_ready_rx.await;
        eprintln!("[pub] Subscriber is ready. Waiting 2s then connecting...");
        tokio::time::sleep(Duration::from_secs(2)).await;
        eprintln!("[pub] Publishing on {}...", test_path);
        let (_publisher, mut track) = xoq::MoqBuilder::new()
            .relay(&relay_pub)
            .path(test_path)
            .connect_publisher_with_track("can")
            .await
            .unwrap();
        eprintln!("[pub] Connected! Waiting 2s for subscriber to detect...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        for i in 0..5 {
            let msg = format!("cmd-{}", i);
            eprintln!("[pub] Sending: {}", msg);
            track.write(bytes::Bytes::from(msg));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        eprintln!("[pub] All sent, keeping alive 5s...");
        tokio::time::sleep(Duration::from_secs(5)).await;
        eprintln!("[pub] Done.");
    });

    let _ = tokio::join!(sub_handle, pub_handle);
    eprintln!();
    eprintln!("=== Test complete ===");
    Ok(())
}

async fn run_cmd_test(relay: &str, base_path: &str) -> Result<()> {
    eprintln!("=== CAN Command Test ===");
    eprintln!("Relay: {}", relay);
    eprintln!("Base path: {}", base_path);
    eprintln!("State:    {}/state (track 'can')", base_path);
    eprintln!("Commands: {}/commands (track 'can')", base_path);
    eprintln!();

    let state_path = format!("{}/state", base_path);
    let cmd_path = format!("{}/commands", base_path);

    let relay_state = relay.to_string();
    let relay_cmd = relay.to_string();

    // 1. Start state subscriber in background
    let state_handle = tokio::spawn(async move {
        eprintln!("[state] Subscribing to {}...", state_path);
        let mut sub = match xoq::MoqBuilder::new()
            .relay(&relay_state)
            .path(&state_path)
            .connect_subscriber()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[state] FAIL: Connect error: {}", e);
                return;
            }
        };
        eprintln!("[state] Connected, waiting for track 'can'...");

        match tokio::time::timeout(Duration::from_secs(10), sub.subscribe_track("can")).await {
            Ok(Ok(Some(mut reader))) => {
                eprintln!("[state] Subscribed to 'can' track, reading...");
                let mut count = 0u64;
                loop {
                    match tokio::time::timeout(Duration::from_secs(10), reader.read()).await {
                        Ok(Ok(Some(data))) => {
                            count += 1;
                            // Decode all wire frames in this chunk
                            let mut offset = 0;
                            while offset < data.len() {
                                if let Some((can_id, frame_data, consumed)) =
                                    decode_can_wire_frame(&data[offset..])
                                {
                                    if let Some(state) = parse_damiao_response(&frame_data) {
                                        eprintln!(
                                            "[state] #{} CAN 0x{:03X}: {}",
                                            count, can_id, state
                                        );
                                    } else {
                                        eprintln!(
                                            "[state] #{} CAN 0x{:03X}: {} bytes: {:02X?}",
                                            count,
                                            can_id,
                                            frame_data.len(),
                                            &frame_data
                                        );
                                    }
                                    offset += consumed;
                                } else {
                                    eprintln!(
                                        "[state] #{} raw: {} bytes (partial frame?)",
                                        count,
                                        data.len() - offset
                                    );
                                    break;
                                }
                            }
                            // Print a few then switch to summary
                            if count > 20 {
                                eprintln!("[state] (suppressing further output, still reading...)");
                                // Just drain
                                while reader.read().await.ok().flatten().is_some() {}
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            eprintln!("[state] Track ended after {} reads", count);
                            break;
                        }
                        Ok(Err(e)) => {
                            eprintln!("[state] Read error after {} reads: {}", count, e);
                            break;
                        }
                        Err(_) => {
                            eprintln!(
                                "[state] No data for 10s (after {} reads). Motors may not be responding.",
                                count
                            );
                            break;
                        }
                    }
                }
            }
            Ok(Ok(None)) => eprintln!("[state] Subscribe returned None"),
            Ok(Err(e)) => eprintln!("[state] Subscribe error: {}", e),
            Err(_) => eprintln!(
                "[state] No broadcast on state path (timeout 10s). Is CAN server publishing?"
            ),
        }
    });

    // 2. Small delay, then connect command publisher + a verification subscriber
    tokio::time::sleep(Duration::from_millis(500)).await;

    let relay_verify = relay.to_string();
    let cmd_path_verify = cmd_path.clone();

    // Verification subscriber: independent subscriber on the same commands path
    // to check if data from our publisher actually reaches the relay
    let verify_handle = tokio::spawn(async move {
        eprintln!(
            "[verify] Subscribing to {} (independent check)...",
            cmd_path_verify
        );
        let mut sub = match xoq::MoqBuilder::new()
            .relay(&relay_verify)
            .path(&cmd_path_verify)
            .connect_subscriber()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[verify] Connect error: {}", e);
                return;
            }
        };
        match tokio::time::timeout(Duration::from_secs(15), sub.subscribe_track("can")).await {
            Ok(Ok(Some(mut reader))) => {
                eprintln!("[verify] Subscribed! Waiting for data...");
                let mut count = 0;
                loop {
                    match tokio::time::timeout(Duration::from_secs(10), reader.read()).await {
                        Ok(Ok(Some(data))) => {
                            count += 1;
                            eprintln!("[verify] GOT DATA #{}: {} bytes", count, data.len());
                            if count >= 3 {
                                eprintln!("[verify] Data flowing OK!");
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            eprintln!("[verify] Track ended after {} reads", count);
                            break;
                        }
                        Ok(Err(e)) => {
                            eprintln!("[verify] Read error: {} (after {} reads)", e, count);
                            break;
                        }
                        Err(_) => {
                            eprintln!("[verify] NO DATA for 10s (after {} reads)", count);
                            break;
                        }
                    }
                }
            }
            Ok(Ok(None)) => eprintln!("[verify] Subscribe returned None"),
            Ok(Err(e)) => eprintln!("[verify] Subscribe error: {}", e),
            Err(_) => eprintln!("[verify] No broadcast on commands path (15s timeout)"),
        }
    });

    let cmd_handle = tokio::spawn(async move {
        eprintln!("[cmd] Publishing on {}...", cmd_path);
        let (_publisher, mut track) = match xoq::MoqBuilder::new()
            .relay(&relay_cmd)
            .path(&cmd_path)
            .connect_publisher_with_track("can")
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[cmd] FAIL: Connect error: {}", e);
                return;
            }
        };
        eprintln!("[cmd] Connected! Publisher is live on {}", cmd_path);
        eprintln!("[cmd] Waiting 3s for CAN server to detect new broadcast...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Send zero-torque queries for motors 1-8
        let mit_cmd = mit_zero_torque_cmd();
        eprintln!(
            "[cmd] Sending zero-torque queries for motors 1-8 (MIT cmd: {:02X?})",
            mit_cmd
        );

        for round in 0..3 {
            for motor_id in 1u32..=8 {
                let frame = encode_can_wire_frame(motor_id, &mit_cmd);
                track.write(bytes::Bytes::from(frame));
            }
            eprintln!("[cmd] Round {} sent (8 motors)", round + 1);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        eprintln!("[cmd] All commands sent. Keeping publisher alive for 15s...");
        tokio::time::sleep(Duration::from_secs(15)).await;
        eprintln!("[cmd] Done.");
    });

    // Wait for all with overall timeout
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(35)) => {
            eprintln!();
            eprintln!("=== Test timeout after 35s ===");
        }
        _ = async {
            let _ = tokio::join!(state_handle, cmd_handle, verify_handle);
        } => {
            eprintln!();
            eprintln!("=== Test complete ===");
        }
    }

    Ok(())
}
