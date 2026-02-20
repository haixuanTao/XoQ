//! Shared bridge server - unifies iroh P2P + MoQ plumbing across all bidirectional servers.
//!
//! Each server (CAN, serial, fake-CAN, fake-serial) provides only its backend logic
//! via channels. BridgeServer handles iroh connection management, MoQ state publishing,
//! and MoQ command subscription.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::iroh::{IrohConnection, IrohServer, IrohServerBuilder};

/// Configuration for MoQ integration.
pub struct MoqConfig {
    pub relay: String,
    pub path: String,
    pub insecure: bool,
    pub state_subpath: String,
    pub command_subpath: String,
    pub track_name: String,
}

/// A generic bridge server that connects iroh P2P + MoQ to backend channels.
///
/// All data flows through `Vec<u8>` channels — the server is format-agnostic.
/// Backends (CAN threads, serial threads, motor sim, echo) handle encoding/decoding.
pub struct BridgeServer {
    server_id: String,
    endpoint: Arc<IrohServer>,
    write_tx: mpsc::Sender<Vec<u8>>,
    read_rx: std::sync::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    moq_read_rx: std::sync::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    moq_config: Option<MoqConfig>,
}

impl BridgeServer {
    /// Create a new bridge server.
    ///
    /// - `identity_path`: path for iroh key persistence (None = ephemeral)
    /// - `write_tx`: send data to the backend
    /// - `read_rx`: receive data from the backend (for network)
    /// - `moq_read_rx`: receive data from the backend (for MoQ state publishing)
    /// - `moq_config`: MoQ configuration (None = disabled)
    pub async fn new(
        identity_path: Option<&str>,
        write_tx: mpsc::Sender<Vec<u8>>,
        read_rx: mpsc::Receiver<Vec<u8>>,
        moq_read_rx: Option<mpsc::Receiver<Vec<u8>>>,
        moq_config: Option<MoqConfig>,
    ) -> Result<Self> {
        let mut builder = IrohServerBuilder::new();
        if let Some(path) = identity_path {
            builder = builder.identity_path(path);
        }
        let server = builder.bind().await?;
        let server_id = server.id().to_string();

        Ok(Self {
            server_id,
            endpoint: Arc::new(server),
            write_tx,
            read_rx: std::sync::Mutex::new(Some(read_rx)),
            moq_read_rx: std::sync::Mutex::new(moq_read_rx),
            moq_config,
        })
    }

    /// Get the server's endpoint ID.
    pub fn id(&self) -> &str {
        &self.server_id
    }

    /// Run the bridge server (blocks forever, handling connections).
    ///
    /// Spawns MoQ tasks if configured, then loops accepting iroh connections.
    /// Only one connection is active at a time — new connections cancel the previous one.
    pub async fn run(&self) -> Result<()> {
        // Spawn MoQ state publisher if configured
        let _moq_pub_handle = if let Some(ref config) = self.moq_config {
            let rx = self.moq_read_rx.lock().unwrap().take();
            if let Some(rx) = rx {
                let relay = config.relay.clone();
                let path = config.path.clone();
                let insecure = config.insecure;
                let state_subpath = config.state_subpath.clone();
                let track_name = config.track_name.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = moq_state_publisher(
                        &relay,
                        &path,
                        &state_subpath,
                        &track_name,
                        insecure,
                        rx,
                    )
                    .await
                    {
                        tracing::error!("MoQ state publisher error: {}", e);
                    }
                }))
            } else {
                None
            }
        } else {
            None
        };

        // Spawn MoQ command subscriber if configured
        let _moq_cmd_handle = if let Some(ref config) = self.moq_config {
            let relay = config.relay.clone();
            let path = config.path.clone();
            let insecure = config.insecure;
            let command_subpath = config.command_subpath.clone();
            let track_name = config.track_name.clone();
            let write_tx = self.write_tx.clone();
            Some(tokio::spawn(async move {
                if let Err(e) = moq_command_subscriber(
                    &relay,
                    &path,
                    &command_subpath,
                    &track_name,
                    insecure,
                    write_tx,
                )
                .await
                {
                    tracing::error!("MoQ command subscriber error: {}", e);
                }
            }))
        } else {
            None
        };

        let mut current_conn: Option<(
            CancellationToken,
            tokio::task::JoinHandle<mpsc::Receiver<Vec<u8>>>,
        )> = None;

        loop {
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            if let Some((cancel, handle)) = current_conn.take() {
                tracing::info!("New client connected, closing previous connection");
                cancel.cancel();
                match handle.await {
                    Ok(rx) => {
                        self.read_rx.lock().unwrap().replace(rx);
                    }
                    Err(e) => {
                        tracing::error!("Connection task panicked: {}", e);
                    }
                }
            }

            let rx = self
                .read_rx
                .lock()
                .unwrap()
                .take()
                .expect("read receiver should be available");

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            let write_tx = self.write_tx.clone();

            let handle = tokio::spawn(async move {
                let (result, rx) = handle_connection(conn, rx, write_tx, cancel_clone).await;
                if let Err(e) = &result {
                    tracing::error!("Connection error: {}", e);
                }
                tracing::info!("Client disconnected");
                rx
            });

            current_conn = Some((cancel, handle));
        }
    }

    /// Run the bridge server for a single connection, then return.
    pub async fn run_once(&self) -> Result<()> {
        tracing::info!(
            "Bridge server waiting for connection. ID: {}",
            self.server_id
        );

        loop {
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            let rx = self
                .read_rx
                .lock()
                .unwrap()
                .take()
                .expect("read receiver should be available");

            let write_tx = self.write_tx.clone();
            let cancel = CancellationToken::new();

            let (result, rx) = handle_connection(conn, rx, write_tx, cancel).await;

            self.read_rx.lock().unwrap().replace(rx);

            tracing::info!("Client disconnected");

            if let Err(e) = result {
                tracing::error!("Connection error: {}", e);
            }

            return Ok(());
        }
    }
}

/// Core bridge logic for a single connection.
///
/// Bridges backend channels to an iroh stream:
/// - read_rx → network (with batching)
/// - network → write_tx
async fn handle_connection(
    conn: IrohConnection,
    mut read_rx: mpsc::Receiver<Vec<u8>>,
    write_tx: mpsc::Sender<Vec<u8>>,
    cancel: CancellationToken,
) -> (Result<()>, mpsc::Receiver<Vec<u8>>) {
    let stream = tokio::select! {
        result = conn.accept_stream() => {
            match result {
                Ok(s) => s,
                Err(e) => {
                    return (
                        Err(anyhow::anyhow!("Failed to accept stream: {}", e)),
                        read_rx,
                    );
                }
            }
        }
        _ = cancel.cancelled() => {
            tracing::info!("Connection cancelled while waiting for stream");
            return (Ok(()), read_rx);
        }
    };

    let (mut send, mut recv) = stream.split();

    // Drain stale data
    let mut drained = 0;
    while read_rx.try_recv().is_ok() {
        drained += 1;
    }
    if drained > 0 {
        tracing::info!("Drained {} stale messages from buffer", drained);
    }

    let conn_cancel = conn.cancellation_token();

    // Backend → Network task (with batching)
    let read_cancel = cancel.clone();
    let conn_cancel_clone = conn_cancel.clone();
    let read_to_net = tokio::spawn(async move {
        let mut batch_buf = Vec::with_capacity(1024);

        loop {
            batch_buf.clear();

            let first = tokio::select! {
                _ = read_cancel.cancelled() => break,
                _ = conn_cancel_clone.cancelled() => break,
                data = read_rx.recv() => match data {
                    Some(d) => d,
                    None => break,
                }
            };

            batch_buf.extend_from_slice(&first);

            // Greedily collect more ready data (up to 8 items)
            for _ in 1..8 {
                match read_rx.try_recv() {
                    Ok(data) => batch_buf.extend_from_slice(&data),
                    Err(_) => break,
                }
            }

            if send.write_all(&batch_buf).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }

        read_rx
    });

    // Network → Backend
    let mut buf = vec![0u8; 1024];
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break Ok(());
            }
            _ = conn_cancel.cancelled() => {
                break Ok(());
            }
            read_result = recv.read(&mut buf) => {
                match read_result {
                    Ok(Some(n)) if n > 0 => {
                        if write_tx.send(buf[..n].to_vec()).await.is_err() {
                            tracing::error!("Backend writer died");
                            break Ok(());
                        }
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        tracing::info!("Client disconnected (stream closed)");
                        break Ok(());
                    }
                    Err(e) => {
                        break Err(anyhow::anyhow!("Network read error: {}", e));
                    }
                }
            }
        }
    };

    cancel.cancel();
    let read_rx = match read_to_net.await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!("Read-to-net task panicked: {}", e);
            return (
                Err(anyhow::anyhow!("Read-to-net task panicked: {}", e)),
                mpsc::channel(1).1,
            );
        }
    };

    (result, read_rx)
}

/// Publish backend data to MoQ relay for browser monitoring.
/// Automatically reconnects with exponential backoff.
async fn moq_state_publisher(
    relay: &str,
    path: &str,
    state_subpath: &str,
    track_name: &str,
    insecure: bool,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    use crate::moq::MoqBuilder;

    let mut builder = MoqBuilder::new().relay(relay);
    if insecure {
        builder = builder.disable_tls_verify();
    }

    let pub_path = format!("{}/{}", path, state_subpath);

    loop {
        // Connect with backoff
        let mut delay = Duration::from_secs(1);
        let (publisher, mut writer) = loop {
            match builder
                .clone()
                .path(&pub_path)
                .connect_publisher_with_track(track_name)
                .await
            {
                Ok(result) => break result,
                Err(e) => {
                    tracing::warn!("MoQ connect failed: {}, retrying in {:?}...", e, delay);
                    while rx.try_recv().is_ok() {}
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        };
        tracing::info!("MoQ state publisher connected on {}", pub_path);

        let mut batch_buf = Vec::with_capacity(1024);
        let disconnected = loop {
            tokio::select! {
                result = publisher.closed() => {
                    tracing::warn!("MoQ session closed: {:?}, reconnecting...", result.err());
                    break false;
                }
                data = rx.recv() => {
                    let first = match data {
                        Some(d) => d,
                        None => break true,
                    };

                    batch_buf.clear();
                    batch_buf.extend_from_slice(&first);

                    while let Ok(d) = rx.try_recv() {
                        batch_buf.extend_from_slice(&d);
                    }

                    writer.write(batch_buf.clone());
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

/// Subscribe to MoQ commands and forward them to the backend.
/// Uses `try_send()` so iroh commands always have priority.
/// Automatically reconnects on disconnect.
///
/// Uses a simple connect-subscribe-read loop. Each iteration creates a fresh
/// connection to the relay, avoiding issues with reannounce-based track switching
/// where data doesn't flow from reannounced broadcasts (confirmed by testing).
async fn moq_command_subscriber(
    relay: &str,
    path: &str,
    command_subpath: &str,
    track_name: &str,
    insecure: bool,
    write_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    use crate::moq::MoqBuilder;

    let mut builder = MoqBuilder::new().relay(relay);
    if insecure {
        builder = builder.disable_tls_verify();
    }

    let cmd_path = format!("{}/{}", path, command_subpath);

    loop {
        tracing::info!("MoQ command subscriber connecting on {}...", cmd_path);

        // Fresh connection each iteration — this ensures we get the latest broadcast
        let mut subscriber = match tokio::time::timeout(
            Duration::from_secs(5),
            builder.clone().path(&cmd_path).connect_subscriber(),
        )
        .await
        {
            Ok(Ok(sub)) => sub,
            Ok(Err(e)) => {
                tracing::warn!("MoQ command subscriber connect error: {}, retrying...", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => {
                tracing::info!("MoQ command subscriber connect timeout, retrying...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        // Subscribe to the command track (waits for broadcast announcement)
        let mut reader = match tokio::time::timeout(
            Duration::from_secs(5),
            subscriber.subscribe_track(track_name),
        )
        .await
        {
            Ok(Ok(Some(r))) => {
                tracing::info!(
                    "MoQ command subscriber subscribed to track '{}' on {}",
                    track_name,
                    cmd_path
                );
                r
            }
            Ok(Ok(None)) => {
                tracing::info!("Command track subscribe returned None, retrying...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Ok(Err(e)) => {
                tracing::warn!("Command track subscribe error: {}, retrying...", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => {
                tracing::info!("No command publisher yet (announce timeout), retrying...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        // Read commands and forward to backend
        let backend_died = loop {
            match tokio::time::timeout(Duration::from_secs(10), reader.read()).await {
                Ok(Ok(Some(data))) => match write_tx.try_send(data.to_vec()) {
                    Ok(_) => {
                        tracing::debug!("MoQ command forwarded ({} bytes)", data.len());
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!("MoQ command dropped (iroh has priority)");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::error!("Backend writer died");
                        break true;
                    }
                },
                Ok(Ok(None)) => {
                    tracing::info!("MoQ command stream ended, will reconnect...");
                    break false;
                }
                Ok(Err(e)) => {
                    tracing::info!("MoQ command read error: {}, will reconnect...", e);
                    break false;
                }
                Err(_) => {
                    tracing::debug!("MoQ command read timeout (10s), will reconnect...");
                    break false;
                }
            }
        };

        if backend_died {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
