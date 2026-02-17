//! Fake CAN server - simulates Damiao motors over iroh P2P + MoQ.
//!
//! Drop-in replacement for can-server that doesn't need CAN hardware.
//! Accepts iroh connections from clients (teleop, etc.), simulates motor
//! responses, and optionally publishes state to MoQ for browser monitoring.
//!
//! Usage:
//!   fake-can-server [options]
//!
//! Options:
//!   --moq-relay <url>    MoQ relay URL (default: https://cdn.1ms.ai)
//!   --moq-path <path>    MoQ base path (default: anon/xoq-can-can0)
//!   --moq-insecure       Disable TLS verification for MoQ
//!   --key-dir <path>     Directory for identity key files (default: current dir)

use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use xoq::iroh::{IrohConnection, IrohServerBuilder};
use xoq::{MoqBuilder, MoqPublisher, MoqTrackWriter};

// Damiao MIT protocol ranges
const POS_MIN: f64 = -12.5;
const POS_MAX: f64 = 12.5;
const VEL_MIN: f64 = -45.0;
const VEL_MAX: f64 = 45.0;
const TAU_MIN: f64 = -18.0;
const TAU_MAX: f64 = 18.0;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];

#[derive(Clone, Default)]
struct MotorState {
    enabled: bool,
    pos: f64,
    vel: f64,
    tau: f64,
}

type Motors = Arc<Mutex<[MotorState; 8]>>;

fn motor_index(can_id: u32) -> Option<usize> {
    if (0x11..=0x18).contains(&can_id) {
        Some((can_id - 0x11) as usize)
    } else {
        None
    }
}

/// Decode a Damiao MIT command (8 bytes) into (pos, vel, kp, kd, tau).
fn decode_damiao_cmd(data: &[u8]) -> (f64, f64, f64, f64, f64) {
    let pos_raw = ((data[0] as u16) << 8) | data[1] as u16;
    let vel_raw = ((data[2] as u16) << 4) | ((data[3] as u16) >> 4);
    let kp_raw = (((data[3] & 0x0F) as u16) << 8) | data[4] as u16;
    let kd_raw = ((data[5] as u16) << 4) | ((data[6] as u16) >> 4);
    let tau_raw = (((data[6] & 0x0F) as u16) << 8) | data[7] as u16;

    (
        pos_raw as f64 / 65535.0 * (POS_MAX - POS_MIN) + POS_MIN,
        vel_raw as f64 / 4095.0 * (VEL_MAX - VEL_MIN) + VEL_MIN,
        kp_raw as f64 / 4095.0 * 500.0,
        kd_raw as f64 / 4095.0 * 5.0,
        tau_raw as f64 / 4095.0 * (TAU_MAX - TAU_MIN) + TAU_MIN,
    )
}

/// Encode a Damiao MIT response frame into 8 bytes.
fn encode_damiao_response(
    motor_id: u8,
    pos: f64,
    vel: f64,
    tau: f64,
    temp_mos: u8,
    temp_rotor: u8,
) -> [u8; 8] {
    let pos_raw = (((pos - POS_MIN) / (POS_MAX - POS_MIN)) * 65535.0).clamp(0.0, 65535.0) as u16;
    let vel_raw = (((vel - VEL_MIN) / (VEL_MAX - VEL_MIN)) * 4095.0).clamp(0.0, 4095.0) as u16;
    let tau_raw = (((tau - TAU_MIN) / (TAU_MAX - TAU_MIN)) * 4095.0).clamp(0.0, 4095.0) as u16;

    [
        motor_id,
        (pos_raw >> 8) as u8,
        (pos_raw & 0xFF) as u8,
        (vel_raw >> 4) as u8,
        (((vel_raw & 0x0F) << 4) | ((tau_raw >> 8) & 0x0F)) as u8,
        (tau_raw & 0xFF) as u8,
        temp_mos,
        temp_rotor,
    ]
}

/// Encode a CAN frame in wire format: [1B flags][4B can_id LE][1B data_len][data...]
fn encode_wire_frame(can_id: u32, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6 + data.len());
    buf.push(0u8); // flags: standard CAN, no FD
    buf.extend_from_slice(&can_id.to_le_bytes());
    buf.push(data.len() as u8);
    buf.extend_from_slice(data);
    buf
}

/// Decode one wire frame: returns (can_id, data, bytes_consumed).
fn decode_wire_frame(buf: &[u8]) -> Option<(u32, Vec<u8>, usize)> {
    if buf.len() < 6 {
        return None;
    }
    let can_id = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
    let data_len = buf[5] as usize;
    if buf.len() < 6 + data_len {
        return None;
    }
    Some((can_id, buf[6..6 + data_len].to_vec(), 6 + data_len))
}

/// Process a CAN command and return the wire-encoded response (if any).
fn process_command(motors: &Motors, can_id: u32, data: &[u8]) -> Option<Vec<u8>> {
    let idx = motor_index(can_id)?;
    if data.len() != 8 {
        return None;
    }

    let mut motors = motors.lock().unwrap();

    if data == ENABLE_MIT {
        motors[idx].enabled = true;
        tracing::info!("Motor 0x{:02X} enabled", can_id);
        // Return a response at current position
        let resp = encode_damiao_response(can_id as u8, motors[idx].pos, 0.0, 0.0, 45, 50);
        return Some(encode_wire_frame(can_id, &resp));
    }
    if data == DISABLE_MIT {
        motors[idx].enabled = false;
        tracing::info!("Motor 0x{:02X} disabled", can_id);
        let resp = encode_damiao_response(can_id as u8, motors[idx].pos, 0.0, 0.0, 45, 50);
        return Some(encode_wire_frame(can_id, &resp));
    }

    if !motors[idx].enabled {
        return None;
    }

    let (cmd_pos, _cmd_vel, cmd_kp, _cmd_kd, cmd_tau) = decode_damiao_cmd(data);

    if cmd_kp > 0.0 {
        motors[idx].pos = cmd_pos;
    }
    motors[idx].vel = 0.0;
    motors[idx].tau = cmd_tau;

    let resp = encode_damiao_response(
        can_id as u8,
        motors[idx].pos,
        motors[idx].vel,
        motors[idx].tau,
        45,
        50,
    );
    Some(encode_wire_frame(can_id, &resp))
}

struct Args {
    moq_relay: Option<String>,
    moq_path: String,
    moq_insecure: bool,
    key_dir: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut result = Args {
        moq_relay: Some("https://cdn.1ms.ai".to_string()),
        moq_path: "anon/xoq-can-can0".to_string(),
        moq_insecure: false,
        key_dir: ".".to_string(),
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
            "--no-moq" => {
                result.moq_relay = None;
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
    println!("Fake CAN Server - simulates Damiao motors over iroh P2P + MoQ");
    println!();
    println!("Usage: fake-can-server [options]");
    println!();
    println!("Options:");
    println!("  --moq-relay <url>    MoQ relay URL (default: https://cdn.1ms.ai)");
    println!("  --moq-path <path>    MoQ base path (default: anon/xoq-can-can0)");
    println!("  --moq-insecure       Disable TLS verification for MoQ");
    println!("  --no-moq             Disable MoQ publishing (iroh only)");
    println!("  --key-dir <path>     Directory for identity key files (default: .)");
}

/// Handle a single iroh connection: read commands, simulate motors, send responses.
async fn handle_connection(
    conn: IrohConnection,
    motors: Motors,
    moq_writer: Option<Arc<Mutex<MoqTrackWriter>>>,
    cancel: CancellationToken,
) -> Result<()> {
    let stream = tokio::select! {
        result = conn.accept_stream() => result?,
        _ = cancel.cancelled() => return Ok(()),
    };

    let (mut send, mut recv) = stream.split();

    let mut buf = vec![0u8; 1024];
    let mut pending = Vec::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            read_result = recv.read(&mut buf) => {
                match read_result {
                    Ok(Some(n)) if n > 0 => {
                        pending.extend_from_slice(&buf[..n]);

                        let mut response_batch = Vec::new();

                        while let Some((can_id, data, consumed)) = decode_wire_frame(&pending) {
                            if let Some(resp) = process_command(&motors, can_id, &data) {
                                response_batch.extend_from_slice(&resp);
                            }
                            pending.drain(..consumed);
                        }

                        if !response_batch.is_empty() {
                            // Send response back over iroh stream
                            if send.write_all(&response_batch).await.is_err() {
                                break;
                            }
                            tokio::task::yield_now().await;

                            // Also publish to MoQ
                            if let Some(ref writer) = moq_writer {
                                writer.lock().unwrap().write(response_batch);
                            }
                        }
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(e) => return Err(anyhow::anyhow!("Read error: {}", e)),
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let args = parse_args();

    println!();
    println!("========================================");
    println!("Fake CAN Server");
    println!("========================================");
    if let Some(ref relay) = args.moq_relay {
        println!("MoQ relay: {}", relay);
        println!("MoQ path:  {}", args.moq_path);
        println!("MoQ state: {}/state", args.moq_path);
    } else {
        println!("MoQ:       disabled");
    }
    println!("Motors:    0x11–0x18 (8 Damiao MIT)");
    println!("========================================");
    println!();

    // Start iroh server
    let identity_path = format!("{}/.xoq_fake_can_server_key", args.key_dir);
    let server = IrohServerBuilder::new()
        .identity_path(&identity_path)
        .bind()
        .await?;

    let server_id = server.id().to_string();
    tracing::info!("Server ID: {}", server_id);
    println!("Server ID: {}", server_id);
    println!();

    let server = Arc::new(server);
    let motors: Motors = Arc::new(Mutex::new(Default::default()));

    // Connect MoQ state publisher
    // IMPORTANT: MoqPublisher holds the MoQ session — it must stay alive
    // for the writer to work. Dropping it closes the session.
    let (moq_writer, _moq_publisher): (Option<Arc<Mutex<MoqTrackWriter>>>, Option<MoqPublisher>) =
        if let Some(ref relay) = args.moq_relay {
            let mut builder = MoqBuilder::new().relay(relay);
            if args.moq_insecure {
                builder = builder.disable_tls_verify();
            }

            match builder
                .path(&format!("{}/state", args.moq_path))
                .connect_publisher_with_track("can")
                .await
            {
                Ok((publisher, writer)) => {
                    tracing::info!("MoQ state publisher connected on {}/state", args.moq_path);
                    (Some(Arc::new(Mutex::new(writer))), Some(publisher))
                }
                Err(e) => {
                    tracing::warn!("MoQ connect failed (continuing without): {}", e);
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

    // Accept iroh connections
    tracing::info!("Waiting for iroh connections...");

    let mut current_conn: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;

    loop {
        let conn = match server.accept().await? {
            Some(c) => c,
            None => continue,
        };

        tracing::info!("Client connected: {}", conn.remote_id());

        // Cancel previous connection
        if let Some((cancel, handle)) = current_conn.take() {
            tracing::info!("New client connected, closing previous connection");
            cancel.cancel();
            let _ = handle.await;
        }

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let motors = Arc::clone(&motors);
        let moq_writer = moq_writer.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = handle_connection(conn, motors, moq_writer, cancel_clone).await {
                tracing::error!("Connection error: {}", e);
            }
            tracing::info!("Client disconnected");
        });

        current_conn = Some((cancel, handle));
    }
}
