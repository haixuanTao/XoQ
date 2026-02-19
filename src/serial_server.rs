//! Serial server - bridges local serial port to remote clients over iroh P2P.
//! Optionally broadcasts serial data via MoQ for browser monitoring.

use anyhow::Result;

use crate::bridge_server::{BridgeServer, MoqConfig};
use crate::serial::SerialPort;

/// A server that bridges a local serial port to remote clients over iroh P2P.
/// Optionally publishes serial data via MoQ for browser access.
pub struct Server {
    bridge: BridgeServer,
}

impl Server {
    /// Create a new serial bridge server
    pub async fn new(
        port: &str,
        baud_rate: u32,
        identity_path: Option<&str>,
        moq_relay: Option<&str>,
        moq_path: Option<&str>,
        moq_insecure: bool,
    ) -> Result<Self> {
        // Open serial port and split
        let serial = SerialPort::open_simple(port, baud_rate)?;
        let (mut reader, mut writer) = serial.split();

        // Channels between serial threads and BridgeServer (raw bytes)
        let (read_tx, read_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);

        // Optional MoQ fan-out channel
        let (moq_tx, moq_rx) = if moq_relay.is_some() {
            let (tx, rx) = tokio::sync::mpsc::channel(128);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        // Spawn dedicated reader thread that continuously polls serial
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => {
                            tokio::task::yield_now().await;
                        }
                        Ok(n) => {
                            tracing::debug!("Serial read {} bytes", n);
                            let data = buf[..n].to_vec();
                            if let Some(ref moq) = moq_tx {
                                let _ = moq.try_send(data.clone());
                            }
                            if read_tx.send(data).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Serial read error: {}", e);
                            break;
                        }
                    }
                }
            });
        });

        // Spawn dedicated writer thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                while let Some(data) = write_rx.recv().await {
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::error!("Serial write error: {}", e);
                        break;
                    }
                    tracing::debug!("Wrote {} bytes to serial", data.len());
                }
            });
        });

        let moq_path_str = moq_path
            .map(|p| p.to_string())
            .unwrap_or_else(|| "anon/xoq-serial".to_string());

        let moq_config = moq_relay.map(|relay| MoqConfig {
            relay: relay.to_string(),
            path: moq_path_str,
            insecure: moq_insecure,
            state_subpath: "s2c".to_string(),
            command_subpath: "c2s".to_string(),
            track_name: "data".to_string(),
        });

        let bridge =
            BridgeServer::new(identity_path, write_tx, read_rx, moq_rx, moq_config).await?;

        Ok(Self { bridge })
    }

    /// Get the server's endpoint ID (share this with clients)
    pub fn id(&self) -> &str {
        self.bridge.id()
    }

    /// Run the bridge server (blocks forever, handling connections)
    pub async fn run(&self) -> Result<()> {
        tracing::info!("Serial bridge server running. ID: {}", self.bridge.id());
        self.bridge.run().await
    }

    /// Run the bridge server for a single connection, then return
    pub async fn run_once(&self) -> Result<()> {
        tracing::info!(
            "Serial bridge server waiting for connection. ID: {}",
            self.bridge.id()
        );
        self.bridge.run_once().await
    }
}
