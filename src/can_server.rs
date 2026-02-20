//! CAN bridge server - bridges local CAN interface to remote clients over iroh P2P.
//!
//! Architecture: 2 persistent OS threads (reader + writer) communicate with the
//! BridgeServer via Vec<u8> channels. Wire encoding/decoding happens in the threads.

use anyhow::Result;
use socketcan::Socket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge_server::{BridgeServer, MoqConfig};
use crate::can::{CanFdFrame, CanFrame};
use crate::can_types::{wire, AnyCanFrame};

/// Read the CAN bus error state by parsing sysfs or `ip -details link show`.
/// Returns e.g. "ERROR-ACTIVE", "ERROR-PASSIVE", "BUS-OFF", or None if unknown.
fn can_bus_state(interface: &str) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(format!("/sys/class/net/{}/can_state", interface)) {
        return Some(s.trim().to_uppercase());
    }
    let output = std::process::Command::new("ip")
        .args(["-details", "link", "show", interface])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("can ") {
            if let Some(pos) = rest.find("state ") {
                let after = &rest[pos + 6..];
                let state = after.split_whitespace().next()?;
                return Some(state.to_string());
            }
        }
    }
    None
}

/// A server that bridges a local CAN interface to remote clients over iroh P2P.
/// Optionally broadcasts CAN state via MoQ for browser monitoring.
pub struct CanServer {
    interface: String,
    bridge: BridgeServer,
}

/// Reader thread for CAN FD sockets. Encodes frames to wire format Vec<u8>.
fn can_reader_thread_fd(
    socket: Arc<socketcan::CanFdSocket>,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    moq_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    write_count: Arc<AtomicU64>,
) {
    let mut last_read = Instant::now();
    let mut would_blocks: u32 = 0;
    let mut timed_outs: u32 = 0;
    let mut writes_at_gap_start: u64 = 0;
    loop {
        match socket.read_frame() {
            Ok(frame) => {
                let gap = last_read.elapsed();
                let writes_now = write_count.load(Ordering::Relaxed);
                let writes_during = writes_now - writes_at_gap_start;
                if gap > Duration::from_millis(50) && writes_during >= 3 {
                    tracing::debug!(
                        "CAN response delay: {:.1}ms ({} timed_out, {} would_block, {} writes during gap)",
                        gap.as_secs_f64() * 1000.0,
                        timed_outs,
                        would_blocks,
                        writes_during,
                    );
                }
                last_read = Instant::now();
                would_blocks = 0;
                timed_outs = 0;
                writes_at_gap_start = writes_now;
                let any_frame = match frame {
                    socketcan::CanAnyFrame::Normal(f) => match CanFrame::try_from(f) {
                        Ok(cf) => AnyCanFrame::Can(cf),
                        Err(e) => {
                            tracing::warn!("CAN frame conversion error: {}", e);
                            continue;
                        }
                    },
                    socketcan::CanAnyFrame::Fd(f) => match CanFdFrame::try_from(f) {
                        Ok(cf) => AnyCanFrame::CanFd(cf),
                        Err(e) => {
                            tracing::warn!("CAN FD frame conversion error: {}", e);
                            continue;
                        }
                    },
                    socketcan::CanAnyFrame::Remote(_) | socketcan::CanAnyFrame::Error(_) => {
                        continue;
                    }
                };
                let bytes = wire::encode(&any_frame);
                if let Some(ref moq) = moq_tx {
                    let _ = moq.try_send(bytes.clone());
                }
                if tx.blocking_send(bytes).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                would_blocks += 1;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                timed_outs += 1;
                continue;
            }
            Err(e) => {
                tracing::warn!("CAN read error (ignoring): {}", e);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Reader thread for standard CAN sockets. Encodes frames to wire format Vec<u8>.
fn can_reader_thread_std(
    socket: Arc<socketcan::CanSocket>,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    moq_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    write_count: Arc<AtomicU64>,
) {
    let mut last_read = Instant::now();
    let mut would_blocks: u32 = 0;
    let mut timed_outs: u32 = 0;
    let mut writes_at_gap_start: u64 = 0;
    loop {
        match socket.read_frame() {
            Ok(frame) => {
                let gap = last_read.elapsed();
                let writes_now = write_count.load(Ordering::Relaxed);
                let writes_during = writes_now - writes_at_gap_start;
                if gap > Duration::from_millis(50) && writes_during >= 3 {
                    tracing::debug!(
                        "CAN response delay: {:.1}ms ({} timed_out, {} would_block, {} writes during gap)",
                        gap.as_secs_f64() * 1000.0,
                        timed_outs,
                        would_blocks,
                        writes_during,
                    );
                }
                last_read = Instant::now();
                would_blocks = 0;
                timed_outs = 0;
                writes_at_gap_start = writes_now;
                let any_frame = match CanFrame::try_from(frame) {
                    Ok(cf) => AnyCanFrame::Can(cf),
                    Err(e) => {
                        tracing::warn!("CAN frame conversion error: {}", e);
                        continue;
                    }
                };
                let bytes = wire::encode(&any_frame);
                if let Some(ref moq) = moq_tx {
                    let _ = moq.try_send(bytes.clone());
                }
                if tx.blocking_send(bytes).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                would_blocks += 1;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                timed_outs += 1;
                continue;
            }
            Err(e) => {
                tracing::warn!("CAN read error (ignoring): {}", e);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Writer thread for CAN FD sockets. Decodes wire format Vec<u8> to frames.
fn can_writer_thread_fd(
    socket: Arc<socketcan::CanFdSocket>,
    mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    write_count: Arc<AtomicU64>,
) {
    let write_count_ref = &write_count;
    let write_fn = |frame: &AnyCanFrame| {
        for attempt in 0..4u32 {
            let result = match frame {
                AnyCanFrame::Can(f) => match socketcan::CanFrame::try_from(f) {
                    Ok(sf) => socket.write_frame(&sf).map(|_| ()),
                    Err(e) => {
                        tracing::warn!("CAN frame conversion error on write: {}", e);
                        return;
                    }
                },
                AnyCanFrame::CanFd(f) => match socketcan::CanFdFrame::try_from(f) {
                    Ok(sf) => socket.write_frame(&sf).map(|_| ()),
                    Err(e) => {
                        tracing::warn!("CAN FD frame conversion error on write: {}", e);
                        return;
                    }
                },
            };
            if result.is_ok() {
                write_count_ref.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let err = result.unwrap_err();
            if err.raw_os_error() == Some(105) && attempt < 3 {
                std::thread::sleep(Duration::from_micros(100));
                continue;
            }
            tracing::warn!("CAN write error (dropping frame): {}", err);
            return;
        }
    };

    let mut pending = Vec::new();
    let mut last_recv = Instant::now();
    while let Some(data) = rx.blocking_recv() {
        let recv_gap = last_recv.elapsed();
        last_recv = Instant::now();
        if recv_gap > Duration::from_millis(50) {
            tracing::debug!(
                "CAN writer: {:.1}ms gap between commands from network",
                recv_gap.as_secs_f64() * 1000.0,
            );
        }
        pending.extend_from_slice(&data);

        while pending.len() >= 6 {
            match wire::encoded_size(&pending) {
                Ok(frame_size) if pending.len() >= frame_size => match wire::decode(&pending) {
                    Ok((frame, consumed)) => {
                        let write_start = Instant::now();
                        write_fn(&frame);
                        let write_dur = write_start.elapsed();
                        if write_dur > Duration::from_millis(5) {
                            tracing::debug!(
                                "CAN writer: write_frame took {:.1}ms",
                                write_dur.as_secs_f64() * 1000.0,
                            );
                        }
                        pending.drain(..consumed);
                    }
                    Err(e) => {
                        tracing::error!("CAN wire decode error: {}", e);
                        pending.clear();
                        break;
                    }
                },
                Ok(_) => break,
                Err(e) => {
                    tracing::error!("CAN wire frame size error: {}", e);
                    pending.clear();
                    break;
                }
            }
        }
    }
}

/// Writer thread for standard CAN sockets. Decodes wire format Vec<u8> to frames.
fn can_writer_thread_std(
    socket: Arc<socketcan::CanSocket>,
    mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    write_count: Arc<AtomicU64>,
) {
    let write_count_ref = &write_count;
    let write_fn = |frame: &AnyCanFrame| {
        for attempt in 0..4u32 {
            let result = match frame {
                AnyCanFrame::Can(f) => match socketcan::CanFrame::try_from(f) {
                    Ok(sf) => socket.write_frame(&sf).map(|_| ()),
                    Err(e) => {
                        tracing::warn!("CAN frame conversion error on write: {}", e);
                        return;
                    }
                },
                AnyCanFrame::CanFd(_) => {
                    tracing::warn!("CAN FD frame on standard CAN socket, dropping");
                    return;
                }
            };
            if result.is_ok() {
                write_count_ref.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let err = result.unwrap_err();
            if err.raw_os_error() == Some(105) && attempt < 3 {
                std::thread::sleep(Duration::from_micros(100));
                continue;
            }
            tracing::warn!("CAN write error (dropping frame): {}", err);
            return;
        }
    };

    let mut pending = Vec::new();
    let mut last_recv = Instant::now();
    while let Some(data) = rx.blocking_recv() {
        let recv_gap = last_recv.elapsed();
        last_recv = Instant::now();
        if recv_gap > Duration::from_millis(50) {
            tracing::debug!(
                "CAN writer: {:.1}ms gap between commands from network",
                recv_gap.as_secs_f64() * 1000.0,
            );
        }
        pending.extend_from_slice(&data);

        while pending.len() >= 6 {
            match wire::encoded_size(&pending) {
                Ok(frame_size) if pending.len() >= frame_size => match wire::decode(&pending) {
                    Ok((frame, consumed)) => {
                        let write_start = Instant::now();
                        write_fn(&frame);
                        let write_dur = write_start.elapsed();
                        if write_dur > Duration::from_millis(5) {
                            tracing::debug!(
                                "CAN writer: write_frame took {:.1}ms",
                                write_dur.as_secs_f64() * 1000.0,
                            );
                        }
                        pending.drain(..consumed);
                    }
                    Err(e) => {
                        tracing::error!("CAN wire decode error: {}", e);
                        pending.clear();
                        break;
                    }
                },
                Ok(_) => break,
                Err(e) => {
                    tracing::error!("CAN wire frame size error: {}", e);
                    pending.clear();
                    break;
                }
            }
        }
    }
}

impl CanServer {
    /// Create a new CAN bridge server.
    ///
    /// If `moq_relay` is provided, CAN read frames are also fan-out to a MoQ
    /// publisher for browser monitoring.
    pub async fn new(
        interface: &str,
        enable_fd: bool,
        identity_path: Option<&str>,
        iroh_relay_url: Option<&str>,
        moq_relay: Option<&str>,
        moq_path: Option<&str>,
        moq_insecure: bool,
    ) -> Result<Self> {
        // Check CAN bus state on startup — bail early if not healthy
        match can_bus_state(interface) {
            Some(ref state) if state == "ERROR-ACTIVE" => {
                tracing::info!("[{}] CAN bus state: {}", interface, state);
            }
            Some(ref state) => {
                anyhow::bail!(
                    "[{}] CAN bus is {} — interface may be down or motors unpowered. \
                     Fix: sudo ip link set {} down && sudo ip link set {} up type can bitrate 1000000 dbitrate 5000000 fd on restart-ms 100",
                    interface, state, interface, interface,
                );
            }
            None => tracing::warn!(
                "[{}] Could not read CAN bus state (will continue anyway)",
                interface
            ),
        }

        // Channels between CAN threads and BridgeServer (Vec<u8> wire-encoded)
        let (read_tx, read_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        let (write_tx, write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let write_count = Arc::new(AtomicU64::new(0));

        let (moq_tx, moq_rx) = if moq_relay.is_some() {
            let (tx, rx) = tokio::sync::mpsc::channel(128);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        if enable_fd {
            let socket = socketcan::CanFdSocket::open(interface).map_err(|e| {
                anyhow::anyhow!("Failed to open CAN FD socket on {}: {}", interface, e)
            })?;
            socket
                .set_read_timeout(Duration::from_millis(10))
                .map_err(|e| anyhow::anyhow!("Failed to set read timeout: {}", e))?;
            tracing::info!("CAN FD socket opened on {}", interface);
            let socket = Arc::new(socket);

            let socket_reader = Arc::clone(&socket);
            let wc_reader = Arc::clone(&write_count);
            let moq_reader_tx = moq_tx.clone();
            std::thread::Builder::new()
                .name(format!("can-read-{}", interface))
                .spawn(move || {
                    can_reader_thread_fd(socket_reader, read_tx, moq_reader_tx, wc_reader)
                })?;

            let socket_writer = Arc::clone(&socket);
            let wc_writer = Arc::clone(&write_count);
            std::thread::Builder::new()
                .name(format!("can-write-{}", interface))
                .spawn(move || can_writer_thread_fd(socket_writer, write_rx, wc_writer))?;
        } else {
            let socket = socketcan::CanSocket::open(interface).map_err(|e| {
                anyhow::anyhow!("Failed to open CAN socket on {}: {}", interface, e)
            })?;
            socket
                .set_read_timeout(Duration::from_millis(10))
                .map_err(|e| anyhow::anyhow!("Failed to set read timeout: {}", e))?;
            tracing::info!("CAN socket opened on {}", interface);
            let socket = Arc::new(socket);

            let socket_reader = Arc::clone(&socket);
            let wc_reader = Arc::clone(&write_count);
            let moq_reader_tx = moq_tx.clone();
            std::thread::Builder::new()
                .name(format!("can-read-{}", interface))
                .spawn(move || {
                    can_reader_thread_std(socket_reader, read_tx, moq_reader_tx, wc_reader)
                })?;

            let socket_writer = Arc::clone(&socket);
            let wc_writer = Arc::clone(&write_count);
            std::thread::Builder::new()
                .name(format!("can-write-{}", interface))
                .spawn(move || can_writer_thread_std(socket_writer, write_rx, wc_writer))?;
        }

        let moq_path_str = moq_path
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("anon/xoq-can-{}", interface));

        let moq_config = moq_relay.map(|relay| MoqConfig {
            relay: relay.to_string(),
            path: moq_path_str.clone(),
            insecure: moq_insecure,
            state_subpath: "state".to_string(),
            command_subpath: "commands".to_string(),
            track_name: "can".to_string(),
        });

        let bridge = BridgeServer::new(
            identity_path,
            iroh_relay_url,
            write_tx,
            read_rx,
            moq_rx,
            moq_config,
        )
        .await?;

        Ok(Self {
            interface: interface.to_string(),
            bridge,
        })
    }

    /// Get the server's endpoint ID (share this with clients).
    pub fn id(&self) -> &str {
        self.bridge.id()
    }

    /// Run the bridge server (blocks forever, handling connections).
    pub async fn run(&self) -> Result<()> {
        tracing::info!(
            "[{}] CAN bridge server running. ID: {}",
            self.interface,
            self.bridge.id()
        );
        self.bridge.run().await
    }

    /// Run the bridge server for a single connection, then return.
    pub async fn run_once(&self) -> Result<()> {
        tracing::info!(
            "CAN bridge server waiting for connection. ID: {}",
            self.bridge.id()
        );
        self.bridge.run_once().await
    }
}
