//! Serial server - bridges local serial port to remote clients over iroh P2P.
//! Optionally broadcasts serial data via MoQ for browser monitoring.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::iroh::{IrohConnection, IrohServerBuilder};
use crate::serial::SerialPort;

/// A server that bridges a local serial port to remote clients over iroh P2P.
/// Optionally publishes serial data via MoQ for browser access.
pub struct Server {
    server_id: String,
    /// Sender for data to write to serial port (tokio mpsc for async + try_send)
    serial_write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Receiver for data read from serial port
    serial_read_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
    endpoint: Arc<crate::iroh::IrohServer>,
    // MoQ fields
    moq_read_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
    moq_relay: Option<String>,
    moq_path: String,
    moq_insecure: bool,
}

impl Server {
    /// Create a new serial bridge server
    ///
    /// Args:
    ///     port: Serial port name (e.g., "/dev/ttyUSB0" or "COM3")
    ///     baud_rate: Baud rate (e.g., 115200)
    ///     identity_path: Optional path to save/load server identity
    ///     moq_relay: Optional MoQ relay URL (enables MoQ alongside iroh)
    ///     moq_path: Optional MoQ broadcast path (default: anon/xoq-serial)
    ///     moq_insecure: Disable TLS verification for MoQ
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

        // Create channels for serial I/O
        // tokio channel for serial->network (async receiver)
        let (serial_read_tx, serial_read_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);
        // tokio channel for network->serial (capacity 1 for backpressure)
        let (serial_write_tx, mut serial_write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);

        // Optional MoQ fan-out channel
        let (moq_tx, moq_rx) = if moq_relay.is_some() {
            let (tx, rx) = tokio::sync::mpsc::channel(128);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        // Spawn dedicated reader thread that continuously polls serial
        let read_tx = serial_read_tx;
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
                            // 0 bytes - timeout, yield to prevent busy spin
                            tokio::task::yield_now().await;
                        }
                        Ok(n) => {
                            tracing::debug!("Serial read {} bytes", n);
                            let data = buf[..n].to_vec();
                            // Fan out to MoQ (non-blocking)
                            if let Some(ref moq) = moq_tx {
                                let _ = moq.try_send(data.clone());
                            }
                            if read_tx.send(data).await.is_err() {
                                break; // Channel closed
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
                while let Some(data) = serial_write_rx.recv().await {
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::error!("Serial write error: {}", e);
                        break;
                    }
                    tracing::debug!("Wrote {} bytes to serial", data.len());
                }
            });
        });

        // Start iroh server
        let mut builder = IrohServerBuilder::new();
        if let Some(path) = identity_path {
            builder = builder.identity_path(path);
        }
        let server = builder.bind().await?;
        let server_id = server.id().to_string();

        let moq_path = moq_path
            .map(|p| p.to_string())
            .unwrap_or_else(|| "anon/xoq-serial".to_string());

        Ok(Self {
            server_id,
            serial_write_tx,
            serial_read_rx: Arc::new(Mutex::new(serial_read_rx)),
            endpoint: Arc::new(server),
            moq_read_rx: std::sync::Mutex::new(moq_rx),
            moq_relay: moq_relay.map(|s| s.to_string()),
            moq_path,
            moq_insecure,
        })
    }

    /// Get the server's endpoint ID (share this with clients)
    pub fn id(&self) -> &str {
        &self.server_id
    }

    /// Run the bridge server (blocks forever, handling connections)
    pub async fn run(&self) -> Result<()> {
        tracing::info!("Serial bridge server running. ID: {}", self.server_id);

        // Spawn MoQ s2c publisher if configured
        let _moq_pub_handle = if self.moq_relay.is_some() {
            let rx = self.moq_read_rx.lock().unwrap().take().unwrap();
            let relay = self.moq_relay.clone().unwrap();
            let path = self.moq_path.clone();
            let insecure = self.moq_insecure;
            Some(tokio::spawn(async move {
                if let Err(e) = moq_s2c_publisher(&relay, &path, insecure, rx).await {
                    tracing::error!("MoQ s2c publisher error: {}", e);
                }
            }))
        } else {
            None
        };

        // Spawn MoQ c2s subscriber if configured
        let _moq_sub_handle = if self.moq_relay.is_some() {
            let relay = self.moq_relay.clone().unwrap();
            let path = self.moq_path.clone();
            let insecure = self.moq_insecure;
            let write_tx = self.serial_write_tx.clone();
            Some(tokio::spawn(async move {
                if let Err(e) = moq_c2s_subscriber(&relay, &path, insecure, write_tx).await {
                    tracing::error!("MoQ c2s subscriber error: {}", e);
                }
            }))
        } else {
            None
        };

        let mut active_cancel: Option<tokio_util::sync::CancellationToken> = None;
        let mut active_task: Option<tokio::task::JoinHandle<()>> = None;

        loop {
            // Accept connection
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            // Cancel previous connection if still active
            if let Some(cancel) = active_cancel.take() {
                tracing::info!("Disconnecting previous client");
                cancel.cancel();
            }
            if let Some(task) = active_task.take() {
                let _ = task.await;
            }

            // Create a cancel token for this connection
            let cancel = tokio_util::sync::CancellationToken::new();
            active_cancel = Some(cancel.clone());

            // Spawn connection handler so we can accept new connections immediately
            let serial_read_rx = self.serial_read_rx.clone();
            let serial_write_tx = self.serial_write_tx.clone();
            active_task = Some(tokio::spawn(async move {
                if let Err(e) =
                    Self::handle_connection_inner(conn, serial_read_rx, serial_write_tx, cancel)
                        .await
                {
                    tracing::error!("Connection error: {}", e);
                }
                tracing::info!("Client disconnected");
            }));
        }
    }

    /// Run the bridge server for a single connection, then return
    pub async fn run_once(&self) -> Result<()> {
        tracing::info!(
            "Serial bridge server waiting for connection. ID: {}",
            self.server_id
        );

        loop {
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            let cancel = tokio_util::sync::CancellationToken::new();
            if let Err(e) = Self::handle_connection_inner(
                conn,
                self.serial_read_rx.clone(),
                self.serial_write_tx.clone(),
                cancel,
            )
            .await
            {
                tracing::error!("Connection error: {}", e);
            }

            tracing::info!("Client disconnected");
            return Ok(());
        }
    }

    async fn handle_connection_inner(
        conn: IrohConnection,
        serial_read_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
        serial_write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        external_cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        tracing::info!(
            "max_datagram_size={:?} (None means datagrams unsupported)",
            conn.max_datagram_size()
        );
        tracing::debug!("Waiting for client to open stream...");
        let stream = tokio::select! {
            result = conn.accept_stream() => {
                result.map_err(|e| anyhow::anyhow!("Failed to accept stream: {}", e))?
            }
            _ = external_cancel.cancelled() => {
                tracing::info!("Connection cancelled while waiting for stream");
                return Ok(());
            }
        };
        tracing::debug!("Stream accepted, starting bridge");
        // Split the stream so reads and writes don't block each other
        let (mut send, mut recv) = stream.split();

        // Drain any stale data from the channel before starting
        {
            let mut rx = serial_read_rx.lock().await;
            let mut drained = 0;
            while rx.try_recv().is_ok() {
                drained += 1;
            }
            if drained > 0 {
                tracing::info!("Drained {} stale serial messages from buffer", drained);
            }
        }

        // Merge connection's own cancel token with the external one
        let cancel_token = conn.cancellation_token();

        // Spawn task: serial -> network (event-driven via channel)
        let cancel_clone = cancel_token.clone();
        let external_clone = external_cancel.clone();
        let serial_to_net = tokio::spawn(async move {
            tracing::debug!("Serial->Network bridge task started");
            let mut rx = serial_read_rx.lock().await;
            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => {
                        tracing::debug!("Serial->Network task cancelled (conn dropped)");
                        break;
                    }
                    _ = external_clone.cancelled() => {
                        tracing::debug!("Serial->Network task cancelled (new client)");
                        break;
                    }
                    data = rx.recv() => {
                        match data {
                            Some(data) => {
                                tracing::debug!("Serial -> Network: {} bytes", data.len());
                                if let Err(e) = send.write_all(&data).await {
                                    tracing::debug!("Network write error: {}", e);
                                    break;
                                }
                                // quinn's flush() is a no-op — yield to let
                                // connection task send the data immediately
                                tokio::task::yield_now().await;
                            }
                            None => break,
                        }
                    }
                }
            }
            tracing::debug!("Serial->Network bridge task ended");
        });

        // Main task: network -> serial
        tracing::debug!("Entering network->serial bridge loop (stream + datagram)");
        let mut buf = vec![0u8; 1024];
        loop {
            tokio::select! {
                _ = external_cancel.cancelled() => {
                    tracing::info!("Connection cancelled (new client connecting)");
                    break;
                }
                // Stream data (reliable, backward-compatible path)
                result = recv.read(&mut buf) => {
                    match result {
                        Ok(Some(n)) if n > 0 => {
                            tracing::debug!(
                                "Network(stream) -> Serial: {} bytes",
                                n,
                            );
                            // Async send — backpressures naturally, gives iroh priority over MoQ's try_send
                            if serial_write_tx.send(buf[..n].to_vec()).await.is_err() {
                                tracing::error!("Serial writer thread died");
                                break;
                            }
                        }
                        Ok(Some(_)) => {
                            // 0 bytes from network - keep waiting
                            continue;
                        }
                        Ok(None) => {
                            tracing::info!("Client disconnected (stream closed)");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Network read error: {}", e);
                            break;
                        }
                    }
                }
                // Datagram data (low-latency path, each datagram is a separate message)
                result = conn.recv_datagram() => {
                    match result {
                        Ok(data) => {
                            tracing::debug!(
                                "Network(datagram) -> Serial: {} bytes",
                                data.len(),
                            );
                            if serial_write_tx.send(data.to_vec()).await.is_err() {
                                tracing::error!("Serial writer thread died");
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Datagram recv error: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        // Signal graceful shutdown instead of abort (allows lock to be released)
        cancel_token.cancel();
        // Wait for task to finish and release the lock
        let _ = serial_to_net.await;
        Ok(())
    }
}

/// Publish serial s2c (server-to-client) data to MoQ relay.
/// Automatically reconnects when the relay session drops.
async fn moq_s2c_publisher(
    relay: &str,
    path: &str,
    insecure: bool,
    mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    use crate::moq::MoqBuilder;

    let mut builder = MoqBuilder::new().relay(relay);
    if insecure {
        builder = builder.disable_tls_verify();
    }

    loop {
        // Connect with backoff
        let mut delay = Duration::from_secs(1);
        let (publisher, mut writer) = loop {
            match builder
                .clone()
                .path(&format!("{}/s2c", path))
                .connect_publisher_with_track("data")
                .await
            {
                Ok(result) => break result,
                Err(e) => {
                    tracing::warn!("MoQ s2c connect failed: {}, retrying in {:?}...", e, delay);
                    while rx.try_recv().is_ok() {}
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        };
        tracing::info!("MoQ s2c publisher connected on {}/s2c", path);

        let disconnected = loop {
            tokio::select! {
                result = publisher.closed() => {
                    tracing::warn!("MoQ s2c session closed: {:?}, reconnecting...", result.err());
                    break false;
                }
                data = rx.recv() => {
                    match data {
                        Some(bytes) => {
                            writer.write(bytes);
                        }
                        None => break true,
                    }
                }
            }
        };

        if disconnected {
            break;
        }

        // Drain stale data before reconnecting
        while rx.try_recv().is_ok() {}
    }
    Ok(())
}

/// Subscribe to MoQ c2s (client-to-server) data and forward to serial writer.
/// Uses `try_send()` so iroh commands always have priority — MoQ commands are
/// dropped if the channel is full (iroh command pending).
/// Automatically reconnects on disconnect.
async fn moq_c2s_subscriber(
    relay: &str,
    path: &str,
    insecure: bool,
    serial_write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    use crate::moq::MoqBuilder;

    let mut builder = MoqBuilder::new().relay(relay);
    if insecure {
        builder = builder.disable_tls_verify();
    }

    let c2s_path = format!("{}/c2s", path);

    loop {
        tracing::info!("MoQ c2s subscriber connecting on {}...", c2s_path);

        let c2s_sub = match builder.clone().path(&c2s_path).connect_subscriber().await {
            Ok(sub) => sub,
            Err(e) => {
                tracing::warn!("MoQ c2s subscriber connect error: {}, retrying...", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let (c2s_reader, c2s_sub) = match tokio::time::timeout(Duration::from_secs(5), async {
            let mut sub = c2s_sub;
            let result = sub.subscribe_track("data").await;
            (result, sub)
        })
        .await
        {
            Ok((Ok(Some(reader)), sub)) => {
                tracing::info!("MoQ c2s subscriber connected on {}", c2s_path);
                (reader, sub)
            }
            Ok((Ok(None), sub)) => {
                tracing::debug!("c2s broadcast ended, retrying...");
                drop(sub);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Ok((Err(e), sub)) => {
                tracing::debug!("c2s subscribe error: {}, retrying...", e);
                drop(sub);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => {
                tracing::debug!("No c2s publisher yet, retrying...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut c2s_reader = c2s_reader;
        let _c2s_sub = c2s_sub; // keep alive while reading

        loop {
            match c2s_reader.read().await {
                Ok(Some(data)) => match serial_write_tx.try_send(data.to_vec()) {
                    Ok(_) => {
                        tracing::debug!("MoQ c2s data forwarded to serial");
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!("MoQ c2s data dropped (iroh has priority)");
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        tracing::error!("Serial writer thread died");
                        return Ok(());
                    }
                },
                Ok(None) => {
                    tracing::info!("MoQ c2s stream ended, will reconnect...");
                    break;
                }
                Err(e) => {
                    tracing::warn!("MoQ c2s read error: {}, will reconnect...", e);
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
