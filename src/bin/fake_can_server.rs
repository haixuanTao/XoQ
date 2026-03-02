//! Fake CAN server - simulates Damiao motors over iroh P2P + MoQ.
//!
//! Drop-in replacement for can-server that doesn't need CAN hardware.
//! Accepts iroh connections from clients (teleop, etc.), simulates motor
//! responses, and optionally publishes state to MoQ for browser monitoring.
//! MoQ commands are also received and processed (via BridgeServer).
//!
//! Usage:
//!   fake-can-server [options]
//!
//! Options:
//!   --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)
//!   --moq-path <path>    MoQ base path (default: anon/xoq-can-can0)
//!   --moq-insecure       Disable TLS verification for MoQ
//!   --key-dir <path>     Directory for identity key files (default: current dir)
//!   --gravity            Enable gravity simulation (rigid body dynamics)
//!   --arm <left|right>   Arm chain to simulate (default: left)

use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use xoq::arm_dynamics::{self, ArmModel};
use xoq::bridge_server::{BridgeServer, MoqConfig};

// Damiao MIT protocol ranges
const POS_MIN: f64 = -12.5;
const POS_MAX: f64 = 12.5;
const VEL_MIN: f64 = -45.0;
const VEL_MAX: f64 = 45.0;
const TAU_MIN: f64 = -18.0;
const TAU_MAX: f64 = 18.0;

const ENABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC];
const DISABLE_MIT: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD];

const PHYSICS_DT: f64 = 0.001; // 1kHz physics
const JOINT_DAMPING: f64 = 0.5; // Nm/(rad/s)

#[derive(Clone, Default)]
struct MotorState {
    enabled: bool,
    pos: f64,
    vel: f64,
    tau: f64,
}

type Motors = Arc<Mutex<[MotorState; 8]>>;

fn motor_index(can_id: u32) -> Option<usize> {
    if (0x01..=0x08).contains(&can_id) {
        Some((can_id - 0x01) as usize)
    } else if (0x11..=0x18).contains(&can_id) {
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

/// Encode a CAN frame as a 72-byte canfd_frame.
fn encode_wire_frame(can_id: u32, data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 72];
    buf[0..4].copy_from_slice(&can_id.to_le_bytes());
    buf[4] = data.len() as u8;
    // buf[5] = flags (0), buf[6..8] = reserved (0), already zeroed
    buf[8..8 + data.len()].copy_from_slice(data);
    buf
}

/// Decode one 72-byte canfd_frame: returns (can_id, data, bytes_consumed).
fn decode_wire_frame(buf: &[u8]) -> Option<(u32, Vec<u8>, usize)> {
    if buf.len() < 72 {
        return None;
    }
    let can_id = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = (buf[4] as usize).min(64);
    Some((can_id, buf[8..8 + len].to_vec(), 72))
}

/// Process a CAN command and return the wire-encoded response (if any).
/// Used when gravity is disabled (instant snap behavior).
fn process_command(motors: &Motors, can_id: u32, data: &[u8]) -> Option<Vec<u8>> {
    let idx = motor_index(can_id)?;
    if data.len() != 8 {
        return None;
    }

    let mut motors = motors.lock().unwrap();

    // Damiao response IDs are always 0x11 + motor_index, regardless of command ID range
    let resp_id = (0x11 + idx) as u32;

    if data == ENABLE_MIT {
        motors[idx].enabled = true;
        tracing::info!("Motor 0x{:02X} enabled", can_id);
        let resp = encode_damiao_response(resp_id as u8, motors[idx].pos, 0.0, 0.0, 45, 50);
        return Some(encode_wire_frame(resp_id, &resp));
    }
    if data == DISABLE_MIT {
        motors[idx].enabled = false;
        tracing::info!("Motor 0x{:02X} disabled", can_id);
        let resp = encode_damiao_response(resp_id as u8, motors[idx].pos, 0.0, 0.0, 45, 50);
        return Some(encode_wire_frame(resp_id, &resp));
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
        resp_id as u8,
        motors[idx].pos,
        motors[idx].vel,
        motors[idx].tau,
        45,
        50,
    );
    Some(encode_wire_frame(resp_id, &resp))
}

// ── Gravity simulation ──────────────────────────────────────────────────────

struct GravitySim {
    model: ArmModel,
    pos: [f64; 7],
    vel: [f64; 7],
    p_des: [f64; 7],
    v_des: [f64; 7],
    kp: [f64; 7],
    kd: [f64; 7],
    tau_ff: [f64; 7],
    tau_motor: [f64; 7],
    enabled: [bool; 8],
    gripper_pos: f64,
}

impl GravitySim {
    fn new(model: ArmModel) -> Self {
        Self {
            model,
            pos: [0.0; 7],
            vel: [0.0; 7],
            p_des: [0.0; 7],
            v_des: [0.0; 7],
            kp: [0.0; 7],
            kd: [0.0; 7],
            tau_ff: [0.0; 7],
            tau_motor: [0.0; 7],
            enabled: [false; 8],
            gripper_pos: 0.0,
        }
    }

    /// Process an enable/disable command. Returns a response frame if handled.
    fn process_enable_disable(&mut self, idx: usize, can_id: u32, data: &[u8]) -> Option<Vec<u8>> {
        let resp_id = (0x11 + idx) as u32;

        if data == ENABLE_MIT {
            self.enabled[idx] = true;
            tracing::info!("Motor 0x{:02X} enabled (gravity)", can_id);
            let pos = if idx < 7 {
                self.pos[idx]
            } else {
                self.gripper_pos
            };
            let resp = encode_damiao_response(resp_id as u8, pos, 0.0, 0.0, 45, 50);
            return Some(encode_wire_frame(resp_id, &resp));
        }
        if data == DISABLE_MIT {
            self.enabled[idx] = false;
            if idx < 7 {
                // Zero out setpoints so the joint is free-floating
                self.kp[idx] = 0.0;
                self.kd[idx] = 0.0;
                self.tau_ff[idx] = 0.0;
            }
            tracing::info!("Motor 0x{:02X} disabled (gravity)", can_id);
            let pos = if idx < 7 {
                self.pos[idx]
            } else {
                self.gripper_pos
            };
            let resp = encode_damiao_response(resp_id as u8, pos, 0.0, 0.0, 45, 50);
            return Some(encode_wire_frame(resp_id, &resp));
        }
        None
    }

    /// Process a MIT command for one motor. Updates setpoints for joints 0-6,
    /// snaps position for motor 7 (gripper). Returns a response frame.
    fn process_mit_command(&mut self, idx: usize, data: &[u8]) -> Option<Vec<u8>> {
        if !self.enabled[idx] {
            return None;
        }

        let resp_id = (0x11 + idx) as u32;
        let (cmd_pos, cmd_vel, cmd_kp, cmd_kd, cmd_tau) = decode_damiao_cmd(data);

        if idx < 7 {
            // Joint motor: update setpoints, physics runs continuously
            self.p_des[idx] = cmd_pos;
            self.v_des[idx] = cmd_vel;
            self.kp[idx] = cmd_kp;
            self.kd[idx] = cmd_kd;
            self.tau_ff[idx] = cmd_tau;

            let resp = encode_damiao_response(
                resp_id as u8,
                self.pos[idx],
                self.vel[idx],
                self.tau_motor[idx],
                45,
                50,
            );
            Some(encode_wire_frame(resp_id, &resp))
        } else {
            // Gripper (motor 8): no gravity, snap position
            if cmd_kp > 0.0 {
                self.gripper_pos = cmd_pos;
            }
            let resp =
                encode_damiao_response(resp_id as u8, self.gripper_pos, 0.0, cmd_tau, 45, 50);
            Some(encode_wire_frame(resp_id, &resp))
        }
    }

    /// Run one physics step at PHYSICS_DT.
    fn step(&mut self) {
        self.tau_motor = arm_dynamics::physics_step(
            &self.model,
            &mut self.pos,
            &mut self.vel,
            &self.p_des,
            &self.v_des,
            &self.kp,
            &self.kd,
            &self.tau_ff,
            PHYSICS_DT,
            JOINT_DAMPING,
        );
    }

    /// Build a response batch for all enabled motors (for MoQ publishing).
    fn build_state_batch(&self) -> Vec<u8> {
        let mut batch = Vec::new();
        for idx in 0..8 {
            if !self.enabled[idx] {
                continue;
            }
            let resp_id = (0x11 + idx) as u32;
            let (pos, vel, tau) = if idx < 7 {
                (self.pos[idx], self.vel[idx], self.tau_motor[idx])
            } else {
                (self.gripper_pos, 0.0, 0.0)
            };
            let resp = encode_damiao_response(resp_id as u8, pos, vel, tau, 45, 50);
            batch.extend_from_slice(&encode_wire_frame(resp_id, &resp));
        }
        batch
    }
}

// ── Args ────────────────────────────────────────────────────────────────────

struct Args {
    iroh_relay: Option<String>,
    moq_relay: Option<String>,
    moq_path: String,
    moq_insecure: bool,
    key_dir: String,
    gravity: bool,
    arm: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut result = Args {
        iroh_relay: None,
        moq_relay: None,
        moq_path: "anon/xoq-can-can0".to_string(),
        moq_insecure: false,
        key_dir: ".".to_string(),
        gravity: false,
        arm: "left".to_string(),
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
            "--gravity" => {
                result.gravity = true;
                i += 1;
            }
            "--arm" if i + 1 < args.len() => {
                result.arm = args[i + 1].clone();
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
    println!("  --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)");
    println!("  --moq-path <path>    MoQ base path (default: anon/xoq-can-can0)");
    println!("  --moq-insecure       Disable TLS verification for MoQ");
    println!("  --key-dir <path>     Directory for identity key files (default: .)");
    println!("  --gravity            Enable gravity simulation (rigid body dynamics)");
    println!("  --arm <left|right>   Arm chain to simulate (default: left)");
    println!();
    println!("Examples:");
    println!("  fake-can-server                                             # iroh only");
    println!("  fake-can-server --moq-relay https://cdn.1ms.ai              # iroh + MoQ");
    println!("  fake-can-server --moq-relay https://cdn.1ms.ai --gravity    # with gravity sim");
}

// ── Motor sim tasks ─────────────────────────────────────────────────────────

/// Motor simulation backend task (no gravity — original instant-snap behavior).
async fn motor_sim_task(
    motors: Motors,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    read_tx: mpsc::Sender<Vec<u8>>,
    moq_read_tx: Option<mpsc::Sender<Vec<u8>>>,
) {
    let mut pending = Vec::new();
    let mut last_moq_positions = [f64::NAN; 8];

    while let Some(data) = write_rx.recv().await {
        tracing::debug!("Motor sim received {} bytes", data.len());
        pending.extend_from_slice(&data);

        let mut response_batch = Vec::new();

        while let Some((can_id, frame_data, consumed)) = decode_wire_frame(&pending) {
            tracing::debug!(
                "Decoded CAN frame: id=0x{:X} data_len={}",
                can_id,
                frame_data.len()
            );
            if let Some(resp) = process_command(&motors, can_id, &frame_data) {
                response_batch.extend_from_slice(&resp);
            }
            pending.drain(..consumed);
        }

        if !response_batch.is_empty() {
            tracing::debug!("Motor sim sending {} bytes response", response_batch.len());
            // Send response to network (via BridgeServer)
            if read_tx.send(response_batch.clone()).await.is_err() {
                break;
            }

            // Send to MoQ only when motor positions changed
            if let Some(ref moq_tx) = moq_read_tx {
                let mg = motors.lock().unwrap();
                let changed = mg.iter().enumerate().any(|(i, m)| {
                    last_moq_positions[i].is_nan() || (m.pos - last_moq_positions[i]).abs() > 1e-10
                });
                if changed {
                    for (i, m) in mg.iter().enumerate() {
                        last_moq_positions[i] = m.pos;
                    }
                    drop(mg);
                    let _ = moq_tx.try_send(response_batch);
                }
            }
        }
    }
}

/// Motor simulation backend task with gravity (1kHz physics loop).
async fn motor_sim_task_gravity(
    model: ArmModel,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    read_tx: mpsc::Sender<Vec<u8>>,
    moq_read_tx: Option<mpsc::Sender<Vec<u8>>>,
) {
    let mut sim = GravitySim::new(model);
    let mut pending = Vec::new();
    let mut interval = tokio::time::interval(Duration::from_millis(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut last_moq_positions = [f64::NAN; 8];
    let mut moq_tick_counter = 0u32;

    loop {
        tokio::select! {
            Some(data) = write_rx.recv() => {
                tracing::debug!("Gravity sim received {} bytes", data.len());
                pending.extend_from_slice(&data);

                let mut response_batch = Vec::new();

                while let Some((can_id, frame_data, consumed)) = decode_wire_frame(&pending) {
                    let idx = match motor_index(can_id) {
                        Some(idx) => idx,
                        None => { pending.drain(..consumed); continue; }
                    };
                    if frame_data.len() != 8 {
                        pending.drain(..consumed);
                        continue;
                    }

                    // Try enable/disable first
                    if let Some(resp) = sim.process_enable_disable(idx, can_id, &frame_data) {
                        response_batch.extend_from_slice(&resp);
                    } else if let Some(resp) = sim.process_mit_command(idx, &frame_data) {
                        response_batch.extend_from_slice(&resp);
                    }
                    pending.drain(..consumed);
                }

                if !response_batch.is_empty() {
                    if read_tx.send(response_batch).await.is_err() {
                        break;
                    }
                }
            }
            _ = interval.tick() => {
                // Run physics at 1kHz
                sim.step();

                // Publish MoQ state at ~100Hz (every 10 ticks)
                moq_tick_counter += 1;
                if moq_tick_counter >= 10 {
                    moq_tick_counter = 0;
                    if let Some(ref moq_tx) = moq_read_tx {
                        let any_enabled = sim.enabled.iter().any(|e| *e);
                        if !any_enabled {
                            continue;
                        }
                        // Check if positions changed
                        let changed = (0..7).any(|i| {
                            last_moq_positions[i].is_nan()
                                || (sim.pos[i] - last_moq_positions[i]).abs() > 1e-6
                        }) || last_moq_positions[7].is_nan()
                            || (sim.gripper_pos - last_moq_positions[7]).abs() > 1e-6;

                        if changed {
                            for i in 0..7 {
                                last_moq_positions[i] = sim.pos[i];
                            }
                            last_moq_positions[7] = sim.gripper_pos;
                            let batch = sim.build_state_batch();
                            if !batch.is_empty() {
                                let _ = moq_tx.try_send(batch);
                            }
                        }
                    }
                }
            }
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
    println!("Fake CAN Server");
    println!("========================================");
    if let Some(ref relay) = args.moq_relay {
        println!("MoQ relay: {}", relay);
        println!("MoQ path:  {}", args.moq_path);
        println!("MoQ state: {}/state", args.moq_path);
        println!("MoQ cmds:  {}/commands", args.moq_path);
    } else {
        println!("MoQ:       disabled");
    }
    if args.gravity {
        println!("Gravity:   enabled ({} arm)", args.arm);
    } else {
        println!("Gravity:   disabled");
    }
    println!("Motors:    0x01–0x08 (8 Damiao MIT, respond on 0x11–0x18)");
    println!("========================================");
    println!();

    // Create channels between motor sim backend and BridgeServer
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(16);
    let (read_tx, read_rx) = mpsc::channel::<Vec<u8>>(16);

    let (moq_read_tx, moq_read_rx) = if args.moq_relay.is_some() {
        let (tx, rx) = mpsc::channel(128);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Spawn motor simulation backend task
    if args.gravity {
        let model = match args.arm.as_str() {
            "right" => arm_dynamics::right_arm_model(),
            _ => arm_dynamics::left_arm_model(),
        };
        tokio::spawn(async move {
            motor_sim_task_gravity(model, write_rx, read_tx, moq_read_tx).await;
        });
    } else {
        let motors: Motors = Arc::new(Mutex::new(Default::default()));
        tokio::spawn(async move {
            motor_sim_task(motors, write_rx, read_tx, moq_read_tx).await;
        });
    }

    // Create MoQ config
    let moq_config = args.moq_relay.as_ref().map(|relay| MoqConfig {
        relay: relay.clone(),
        path: args.moq_path.clone(),
        insecure: args.moq_insecure,
        state_subpath: "state".to_string(),
        command_subpath: "commands".to_string(),
        track_name: "can".to_string(),
    });

    // Create and run BridgeServer
    // Derive unique key filename from moq_path so each instance gets its own identity
    let key_suffix = args
        .moq_path
        .replace('/', "_")
        .replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "");
    let identity_path = format!("{}/.xoq_fake_can_server_key_{}", args.key_dir, key_suffix);
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
