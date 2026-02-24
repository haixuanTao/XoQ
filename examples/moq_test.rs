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
        "burst" => {
            let relay = args.get(2).map(|s| s.as_str()).unwrap_or(DEFAULT_CMD_RELAY);
            let count: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
            run_burst_test(relay, count).await
        }
        "watch" => {
            let base_path = match args.get(2) {
                Some(p) => p.as_str(),
                None => {
                    eprintln!("Usage: moq_test watch <base-path> [relay]");
                    eprintln!();
                    eprintln!("  base-path: CAN server MoQ path (e.g. anon/NODE_ID/xoq-can-can0)");
                    eprintln!("  relay:     Relay URL (default: {})", DEFAULT_CMD_RELAY);
                    eprintln!();
                    eprintln!("Read-only monitor: subscribes to <base-path>/state track 'can'");
                    eprintln!("and prints decoded motor positions. Runs until Ctrl-C.");
                    return Ok(());
                }
            };
            let relay = args.get(3).map(|s| s.as_str()).unwrap_or(DEFAULT_CMD_RELAY);
            run_watch(relay, base_path).await
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
            eprintln!("Usage: moq_test <pub|sub|stream|watch|cmd|burst> [args...]");
            eprintln!();
            eprintln!("  pub                    - Run publisher (one-way test)");
            eprintln!("  sub                    - Run subscriber (one-way test)");
            eprintln!("  stream                 - Bidirectional MoqStream test (server + client)");
            eprintln!(
                "  watch <path> [url]     - Monitor CAN state (read-only, prints motor positions)"
            );
            eprintln!(
                "  cmd <path> [url]       - CAN command test (simulates browser motor query)"
            );
            eprintln!(
                "  burst [relay] [count]  - Burst test: send N msgs fast, then verify still alive"
            );
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

// ─── Watch mode (read-only CAN state monitor) ─────────────

async fn run_watch(relay: &str, base_path: &str) -> Result<()> {
    let state_path = format!("{}/state", base_path);
    eprintln!("=== CAN State Watch ===");
    eprintln!("Relay: {}", relay);
    eprintln!("Path:  {}", state_path);
    eprintln!();

    eprintln!("[watch] Connecting subscriber...");
    let mut sub = xoq::MoqBuilder::new()
        .relay(relay)
        .path(&state_path)
        .connect_subscriber()
        .await?;

    eprintln!("[watch] Subscribing to track 'can' (10s timeout)...");
    let mut reader =
        match tokio::time::timeout(Duration::from_secs(10), sub.subscribe_track("can")).await {
            Ok(Ok(Some(r))) => {
                eprintln!("ASSERT PASS: subscribed to track 'can'");
                r
            }
            Ok(Ok(None)) => {
                eprintln!("ASSERT FAIL: subscribe returned None (no broadcast found)");
                return Ok(());
            }
            Ok(Err(e)) => {
                eprintln!("ASSERT FAIL: subscribe error: {}", e);
                return Ok(());
            }
            Err(_) => {
                eprintln!("ASSERT FAIL: no broadcast within 10s — CAN server not publishing?");
                return Ok(());
            }
        };

    eprintln!("[watch] Reading frames (Ctrl-C to stop)...");
    eprintln!();

    let mut frame_count = 0u64;
    let mut last_stats = std::time::Instant::now();
    let mut frames_since_stats = 0u64;
    let first_frame_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut got_first_frame = false;

    loop {
        let timeout = if got_first_frame {
            tokio::time::Instant::now() + Duration::from_secs(30)
        } else {
            first_frame_deadline
        };

        match tokio::time::timeout_at(timeout, reader.read()).await {
            Ok(Ok(Some(data))) => {
                frame_count += 1;
                frames_since_stats += 1;

                if !got_first_frame {
                    eprintln!("ASSERT PASS: first frame received ({} bytes)", data.len());
                    got_first_frame = true;
                }

                // Decode and print motor positions
                let now = {
                    let d = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap();
                    let secs = d.as_secs() % 86400; // time-of-day
                    let ms = d.subsec_millis();
                    format!(
                        "{:02}:{:02}:{:02}.{:03}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60,
                        ms
                    )
                };
                let mut motors = Vec::new();
                let mut offset = 0;
                while offset < data.len() {
                    if let Some((_can_id, frame_data, consumed)) =
                        decode_can_wire_frame(&data[offset..])
                    {
                        if frame_data.len() >= 8 {
                            let motor_id = frame_data[0];
                            let p_raw = ((frame_data[1] as u16) << 8) | (frame_data[2] as u16);
                            let v_raw =
                                ((frame_data[3] as u16) << 4) | ((frame_data[4] as u16) >> 4);
                            let t_raw =
                                (((frame_data[4] & 0x0F) as u16) << 8) | (frame_data[5] as u16);
                            let pos = -12.5 + (p_raw as f64 / 65535.0) * 25.0;
                            let vel = -45.0 + (v_raw as f64 / 4095.0) * 90.0;
                            let torque = -18.0 + (t_raw as f64 / 4095.0) * 36.0;
                            motors.push((motor_id, pos, vel, torque));
                        }
                        offset += consumed;
                    } else {
                        break;
                    }
                }

                eprint!("[{}] #{} ({} bytes)", now, frame_count, data.len());
                if motors.is_empty() {
                    eprintln!(" (no decodable motor frames)");
                } else {
                    for (id, pos, vel, torque) in &motors {
                        eprint!("  m{}: {:.3}rad {:.2}v {:.2}t", id, pos, vel, torque);
                    }
                    eprintln!();
                }

                // Print stats every 5s
                if last_stats.elapsed() >= Duration::from_secs(5) {
                    let fps = frames_since_stats as f64 / last_stats.elapsed().as_secs_f64();
                    eprintln!("[stats] {:.1} frames/sec, {} total", fps, frame_count);
                    last_stats = std::time::Instant::now();
                    frames_since_stats = 0;
                }
            }
            Ok(Ok(None)) => {
                eprintln!("[watch] Track ended after {} frames", frame_count);
                break;
            }
            Ok(Err(e)) => {
                eprintln!("[watch] Read error after {} frames: {}", frame_count, e);
                break;
            }
            Err(_) => {
                if !got_first_frame {
                    eprintln!(
                        "ASSERT FAIL: no data within 10s — CAN server publishing but track empty?"
                    );
                } else {
                    eprintln!(
                        "[watch] No data for 30s, stopping. {} frames total",
                        frame_count
                    );
                }
                break;
            }
        }
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

/// Run one round of CAN command testing: publish zero-torque queries,
/// read motor state responses, return the set of (motor_id, position) seen.
async fn run_cmd_round(
    relay: &str,
    base_path: &str,
    duration_secs: u64,
) -> Vec<(u8, f64, f64, f64)> {
    use std::collections::HashMap;

    let state_path = format!("{}/state", base_path);
    let cmd_path = format!("{}/commands", base_path);
    let relay_cmd = relay.to_string();
    let relay_state = relay.to_string();

    // Collect motor responses: motor_id → (position, velocity, torque)
    let responses: std::sync::Arc<std::sync::Mutex<HashMap<u8, (f64, f64, f64)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    let responses_writer = responses.clone();

    // Publisher: send zero-torque queries
    let pub_handle = tokio::spawn(async move {
        let (_publisher, mut track) = match xoq::MoqBuilder::new()
            .relay(&relay_cmd)
            .path(&cmd_path)
            .connect_publisher_with_track("can")
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [cmd] Connect error: {}", e);
                return;
            }
        };
        let mit_cmd = mit_zero_torque_cmd();
        let rounds = duration_secs * 2; // 500ms intervals
        for _ in 0..rounds {
            for motor_id in 1u32..=8 {
                let frame = encode_can_wire_frame(motor_id, &mit_cmd);
                track.write(bytes::Bytes::from(frame));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // Subscriber: read motor state
    let sub_handle = tokio::spawn(async move {
        let mut sub = match xoq::MoqBuilder::new()
            .relay(&relay_state)
            .path(&state_path)
            .connect_subscriber()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  [state] Connect error: {}", e);
                return;
            }
        };

        let mut reader =
            match tokio::time::timeout(Duration::from_secs(10), sub.subscribe_track("can")).await {
                Ok(Ok(Some(r))) => r,
                Ok(Ok(None)) => {
                    eprintln!("  [state] Subscribe returned None");
                    return;
                }
                Ok(Err(e)) => {
                    eprintln!("  [state] Subscribe error: {}", e);
                    return;
                }
                Err(_) => {
                    eprintln!("  [state] No broadcast (10s timeout)");
                    return;
                }
            };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs + 2);
        loop {
            match tokio::time::timeout_at(deadline, reader.read()).await {
                Ok(Ok(Some(data))) => {
                    let mut offset = 0;
                    while offset < data.len() {
                        if let Some((_can_id, frame_data, consumed)) =
                            decode_can_wire_frame(&data[offset..])
                        {
                            if frame_data.len() >= 8 {
                                let motor_id = frame_data[0];
                                let p_raw = ((frame_data[1] as u16) << 8) | (frame_data[2] as u16);
                                let v_raw =
                                    ((frame_data[3] as u16) << 4) | ((frame_data[4] as u16) >> 4);
                                let t_raw =
                                    (((frame_data[4] & 0x0F) as u16) << 8) | (frame_data[5] as u16);
                                let pos = -12.5 + (p_raw as f64 / 65535.0) * 25.0;
                                let vel = -45.0 + (v_raw as f64 / 4095.0) * 90.0;
                                let torque = -18.0 + (t_raw as f64 / 4095.0) * 36.0;
                                responses_writer
                                    .lock()
                                    .unwrap()
                                    .insert(motor_id, (pos, vel, torque));
                            }
                            offset += consumed;
                        } else {
                            break;
                        }
                    }
                }
                Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
            }
        }
    });

    let _ = tokio::join!(pub_handle, sub_handle);

    let map = responses.lock().unwrap();
    let mut result: Vec<(u8, f64, f64, f64)> =
        map.iter().map(|(&id, &(p, v, t))| (id, p, v, t)).collect();
    result.sort_by_key(|(id, _, _, _)| *id);
    result
}

fn check_motor_responses(motors: &[(u8, f64, f64, f64)]) -> bool {
    if motors.is_empty() {
        eprintln!("  FAIL: No motor responses received");
        return false;
    }
    // Check that at least one motor has a non-default position
    // Default/zero would be exactly 0.0 (p_raw=0x8000) — real motors have some offset
    let has_real_position = motors.iter().any(|(_, pos, _, _)| pos.abs() > 0.001);
    if !has_real_position {
        eprintln!("  WARN: All motor positions are ~0.0 (motors may not be enabled)");
    }
    for (id, pos, vel, torque) in motors {
        eprintln!(
            "  motor {:2}: pos={:+.3}rad  vel={:+.2}  tau={:+.2}",
            id, pos, vel, torque
        );
    }
    true
}

async fn run_cmd_test(relay: &str, base_path: &str) -> Result<()> {
    eprintln!("=== CAN Command Reconnection Test ===");
    eprintln!("Relay: {}", relay);
    eprintln!("Path:  {}", base_path);
    eprintln!();

    // Round 1
    eprintln!("--- Round 1: initial connection ---");
    let motors1 = run_cmd_round(relay, base_path, 5).await;
    let ok1 = check_motor_responses(&motors1);
    eprintln!(
        "  Result: {} motors responded {}",
        motors1.len(),
        if ok1 { "OK" } else { "FAIL" }
    );
    eprintln!();

    // Disconnect gap
    eprintln!("--- Disconnected. Waiting 5s... ---");
    tokio::time::sleep(Duration::from_secs(5)).await;
    eprintln!();

    // Round 2 — reconnection
    eprintln!("--- Round 2: reconnection ---");
    let motors2 = run_cmd_round(relay, base_path, 5).await;
    let ok2 = check_motor_responses(&motors2);
    eprintln!(
        "  Result: {} motors responded {}",
        motors2.len(),
        if ok2 { "OK" } else { "FAIL" }
    );
    eprintln!();

    // Summary
    eprintln!("=== Summary ===");
    eprintln!(
        "Round 1: {} motors  {}",
        motors1.len(),
        if ok1 { "PASS" } else { "FAIL" }
    );
    eprintln!(
        "Round 2: {} motors  {}",
        motors2.len(),
        if ok2 { "PASS" } else { "FAIL" }
    );
    if ok1 && ok2 {
        eprintln!("RESULT: Reconnection works! Both rounds received motor data.");
    } else if ok1 && !ok2 {
        eprintln!("RESULT: RECONNECTION BROKEN — Round 1 worked but Round 2 failed!");
    } else {
        eprintln!("RESULT: FAIL — check CAN server and motor connections.");
    }

    Ok(())
}

/// Burst test: publisher sends N messages as fast as possible, subscriber counts them,
/// then publisher sends slow follow-up messages to verify connection is still alive.
async fn run_burst_test(relay: &str, count: usize) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let test_path = "anon/xoq-burst-test";
    eprintln!("=== Burst Test ===");
    eprintln!("Relay: {}", relay);
    eprintln!("Path:  {}", test_path);
    eprintln!("Burst: {} messages", count);
    eprintln!();

    let burst_received = Arc::new(AtomicU64::new(0));
    let post_burst_received = Arc::new(AtomicU64::new(0));
    let burst_received_sub = burst_received.clone();
    let post_burst_received_sub = post_burst_received.clone();

    // Signal from pub → sub that publisher is connected
    let (pub_ready_tx, pub_ready_rx) = tokio::sync::oneshot::channel::<()>();
    // Signal from sub → pub that subscriber is subscribed
    let (sub_ready_tx, sub_ready_rx) = tokio::sync::oneshot::channel::<()>();

    let relay_pub = relay.to_string();
    let relay_sub = relay.to_string();

    // 1. Start publisher FIRST so relay has a broadcast to announce
    let burst_count = count;
    let pub_handle = tokio::spawn(async move {
        eprintln!("[pub] Connecting to {}...", relay_pub);
        let (_publisher, mut track) = xoq::MoqBuilder::new()
            .relay(&relay_pub)
            .path(test_path)
            .disable_tls_verify()
            .connect_publisher_with_track("data")
            .await
            .unwrap();
        eprintln!("[pub] Connected! Signaling ready for subscriber...");
        let _ = pub_ready_tx.send(());

        // Wait for subscriber to be ready
        let _ = sub_ready_rx.await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- BURST PHASE ---
        eprintln!("[pub] Starting burst of {} messages...", burst_count);
        let burst_start = std::time::Instant::now();
        for i in 0..burst_count {
            let msg = format!("burst-{}", i);
            track.write(bytes::Bytes::from(msg));
        }
        let burst_elapsed = burst_start.elapsed();
        eprintln!(
            "[pub] Burst complete: {} msgs in {:.1}ms ({:.0} msg/s)",
            burst_count,
            burst_elapsed.as_secs_f64() * 1000.0,
            burst_count as f64 / burst_elapsed.as_secs_f64()
        );

        // Give subscriber time to drain
        tokio::time::sleep(Duration::from_secs(3)).await;

        // --- POST-BURST PHASE: verify connection still alive ---
        eprintln!("[pub] Post-burst: sending 10 slow messages at 1/s...");
        for i in 0..10 {
            let msg = format!("post-burst-{}", i);
            eprintln!("[pub] Sending: {}", msg);
            track.write(bytes::Bytes::from(msg));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        eprintln!("[pub] All done, keeping alive 3s...");
        tokio::time::sleep(Duration::from_secs(3)).await;
    });

    // 2. Start subscriber after publisher is ready
    let sub_handle = tokio::spawn(async move {
        // Wait for publisher to connect first
        let _ = pub_ready_rx.await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        eprintln!("[sub] Connecting to {}...", relay_sub);
        let mut subscriber = match xoq::MoqBuilder::new()
            .relay(&relay_sub)
            .path(test_path)
            .disable_tls_verify()
            .connect_subscriber()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[sub] FAIL: Connect error: {}", e);
                return 0u64;
            }
        };

        let mut reader = match subscriber.subscribe_track("data").await {
            Ok(Some(r)) => r,
            Ok(None) => {
                eprintln!("[sub] FAIL: Subscribe returned None");
                return 0;
            }
            Err(e) => {
                eprintln!("[sub] FAIL: Subscribe error: {}", e);
                return 0;
            }
        };
        eprintln!("[sub] Subscribed! Signaling ready for burst...");
        let _ = sub_ready_tx.send(());

        let mut total = 0u64;

        loop {
            match tokio::time::timeout(Duration::from_secs(15), reader.read()).await {
                Ok(Ok(Some(data))) => {
                    total += 1;
                    let msg = String::from_utf8_lossy(&data);
                    if msg.starts_with("post-burst") {
                        post_burst_received_sub.fetch_add(1, Ordering::Relaxed);
                        eprintln!("[sub] #{} POST-BURST: {}", total, msg);
                    } else {
                        burst_received_sub.fetch_add(1, Ordering::Relaxed);
                        if total <= 5 || total % 200 == 0 {
                            eprintln!("[sub] #{} BURST: {}", total, msg);
                        }
                    }
                }
                Ok(Ok(None)) => {
                    eprintln!("[sub] Track ended after {} reads", total);
                    break;
                }
                Ok(Err(e)) => {
                    eprintln!("[sub] Read error after {} reads: {}", total, e);
                    break;
                }
                Err(_) => {
                    eprintln!("[sub] Timeout (15s idle). total={}", total);
                    break;
                }
            }
        }
        total
    });

    let (sub_result, _pub_result) = tokio::join!(sub_handle, pub_handle);
    let total_received = sub_result.unwrap_or(0);

    eprintln!();
    eprintln!("=== Burst Test Results ===");
    eprintln!(
        "Burst received:      {} / {} ({:.1}%)",
        burst_received.load(Ordering::Relaxed),
        count,
        burst_received.load(Ordering::Relaxed) as f64 / count as f64 * 100.0
    );
    eprintln!(
        "Post-burst received: {} / 10",
        post_burst_received.load(Ordering::Relaxed)
    );
    eprintln!("Total received:      {}", total_received);

    if post_burst_received.load(Ordering::Relaxed) > 0 {
        eprintln!("RESULT: Connection survived the burst!");
    } else {
        eprintln!("RESULT: CONNECTION DEAD after burst — no post-burst messages received!");
    }

    Ok(())
}
