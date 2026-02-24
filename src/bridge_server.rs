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
        iroh_relay_url: Option<&str>,
        write_tx: mpsc::Sender<Vec<u8>>,
        read_rx: mpsc::Receiver<Vec<u8>>,
        moq_read_rx: Option<mpsc::Receiver<Vec<u8>>>,
        moq_config: Option<MoqConfig>,
    ) -> Result<Self> {
        let mut builder = IrohServerBuilder::new();
        if let Some(path) = identity_path {
            builder = builder.identity_path(path);
        }
        if let Some(url) = iroh_relay_url {
            builder = builder.relay_url(url);
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
///
/// Creates a single MoQ client (QUIC endpoint) and reuses it across reconnects
/// to avoid leaking UDP sockets. Each `moq_native::Client` binds a UDP socket;
/// creating a new one per retry leaked ~1 socket/retry, hitting the 1024 FD limit
/// after ~20h.
async fn moq_state_publisher(
    relay: &str,
    path: &str,
    state_subpath: &str,
    track_name: &str,
    insecure: bool,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    use crate::moq::MoqBuilder;
    use moq_native::moq_lite::{Broadcast, Origin, Track};

    let builder = MoqBuilder::new().relay(relay);
    let builder = if insecure {
        builder.disable_tls_verify()
    } else {
        builder
    };

    let pub_path = format!("{}/{}", path, state_subpath);

    // Create client once — reuse across reconnects to avoid UDP socket leak.
    // Cloning shares the underlying quinn::Endpoint (Arc), no new socket.
    let client = builder.create_client_public()?;

    loop {
        // Connect with backoff
        let mut delay = Duration::from_secs(1);
        let (publisher, mut writer, _broadcast_producer) = loop {
            let url = builder.build_url_for_path(&pub_path)?;
            let origin = Origin::produce();
            let mut broadcast = Broadcast::produce();

            // Create track BEFORE connecting (same as connect_publisher_with_track)
            let track_producer = broadcast.producer.create_track(Track::new(track_name));

            origin
                .producer
                .publish_broadcast("", broadcast.consumer.clone());

            match tokio::time::timeout(Duration::from_secs(15), {
                let c = client.clone();
                async move {
                    let session = c.with_publish(origin.consumer).connect(url).await?;
                    Ok::<_, anyhow::Error>(session)
                }
            })
            .await
            {
                Ok(Ok(session)) => {
                    // Keep broadcast.producer alive — dropping it signals the relay
                    // that the broadcast is finished, making the track invisible.
                    break (
                        session,
                        crate::moq::MoqTrackWriter::from_producer(track_producer),
                        broadcast.producer,
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!("MoQ connect failed: {}, retrying in {:?}...", e, delay);
                    while rx.try_recv().is_ok() {}
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
                Err(_) => {
                    tracing::warn!("MoQ connect timed out (15s), retrying in {:?}...", delay);
                    while rx.try_recv().is_ok() {}
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        };
        tracing::info!("MoQ state publisher connected on {}", pub_path);

        let mut batch_buf = Vec::with_capacity(1024);
        let mut write_count = 0u64;
        let mut last_heartbeat = tokio::time::Instant::now();
        // Publish at a fixed 100Hz rate so the subscriber can always keep up.
        // All CAN frames arriving between ticks are batched into one group.
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut has_data = false;
        let disconnected = loop {
            tokio::select! {
                result = publisher.closed() => {
                    tracing::warn!("MoQ session closed after {} writes: {:?}, reconnecting...", write_count, result.err());
                    break false;
                }
                data = rx.recv() => {
                    match data {
                        Some(d) => {
                            batch_buf.extend_from_slice(&d);
                            has_data = true;
                            // Drain everything already queued
                            while let Ok(d) = rx.try_recv() {
                                batch_buf.extend_from_slice(&d);
                            }
                        }
                        None => break true,
                    };
                }
                _ = tick.tick() => {
                    if has_data {
                        // Count CAN frames in batch (wire format: 6+data_len per frame)
                        let mut n_frames = 0u32;
                        let mut off = 0usize;
                        while off + 6 <= batch_buf.len() {
                            let dlen = batch_buf[off + 5] as usize;
                            if off + 6 + dlen > batch_buf.len() { break; }
                            n_frames += 1;
                            off += 6 + dlen;
                        }
                        tracing::info!("MoQ publish: {} bytes, {} CAN frames", batch_buf.len(), n_frames);

                        writer.write(batch_buf.clone());
                        write_count += 1;
                        batch_buf.clear();
                        has_data = false;

                        if last_heartbeat.elapsed() >= Duration::from_secs(10) {
                            tracing::info!("MoQ state publisher: {} writes", write_count);
                            last_heartbeat = tokio::time::Instant::now();
                        }
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

/// Subscribe to MoQ commands and forward them to the backend.
/// Uses `try_send()` so iroh commands always have priority.
/// Automatically reconnects on disconnect.
///
/// Uses a `select!` loop to simultaneously read data AND watch for new
/// broadcast announcements (reannounce). When a publisher reconnects,
/// the reader switches to the new broadcast immediately — no need to
/// drain/detect stale data.
async fn moq_command_subscriber(
    relay: &str,
    path: &str,
    command_subpath: &str,
    track_name: &str,
    insecure: bool,
    write_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    use crate::moq::{MoqBuilder, MoqTrackReader};
    use moq_native::moq_lite::{Origin, Track};

    let builder = MoqBuilder::new().relay(relay);
    let builder = if insecure {
        builder.disable_tls_verify()
    } else {
        builder
    };

    let cmd_path = format!("{}/{}", path, command_subpath);

    // Create client once — reuse across reconnects to avoid UDP socket leak.
    let client = builder.create_client_public()?;

    loop {
        tracing::info!("MoQ command subscriber connecting on {}...", cmd_path);

        // Connect at low level, retaining origin_consumer for reannounce handling
        let (mut origin_consumer, _session) = match tokio::time::timeout(Duration::from_secs(5), {
            let url = builder.build_url_for_path(&cmd_path)?;
            let c = client.clone();
            async move {
                let origin = Origin::produce();
                let session = c.with_consume(origin.producer).connect(url).await?;
                Ok::<_, anyhow::Error>((origin.consumer, session))
            }
        })
        .await
        {
            Ok(Ok(r)) => r,
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

        // Wait for initial broadcast announcement
        let broadcast = match tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match origin_consumer.announced().await {
                    Some((_path, Some(bc))) => return Some(bc),
                    Some((_path, None)) => continue, // unannounce, skip
                    None => return None,
                }
            }
        })
        .await
        {
            Ok(Some(bc)) => bc,
            Ok(None) => {
                tracing::info!("MoQ command origin closed, reconnecting...");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(_) => {
                tracing::debug!("No command publisher yet (10s), reconnecting...");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };

        let mut current_broadcast = broadcast;
        let mut reader =
            MoqTrackReader::from_track(current_broadcast.subscribe_track(&Track::new(track_name)));
        let mut reader_alive = true;
        tracing::info!(
            "MoQ command subscriber active on track '{}' at {}",
            track_name,
            cmd_path
        );

        // select! loop: read data | watch reannouncements | idle timeout
        //
        // On Ok(None): track ended (publisher disconnected) — disable reader and
        //   wait for reannounce so we seamlessly switch when the browser reconnects.
        // On Err: re-subscribe to same broadcast (handles burst recovery). If the
        //   broadcast is dead, the new reader returns Ok(None) next iteration,
        //   which falls into the reannounce-wait path above.
        // Idle timeout (30s): no data or reannounce — reconnect to clear stale state.
        let timeout = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(timeout);

        let mut backend_died = false;
        let mut err_count = 0u32;
        loop {
            tokio::select! {
                // Read data from current broadcast (disabled when reader ended)
                read_result = reader.read(), if reader_alive => {
                    match read_result {
                        Ok(Some(data)) => {
                            err_count = 0;
                            timeout.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(30));
                            match write_tx.try_send(data.to_vec()) {
                                Ok(_) => {
                                    tracing::debug!("MoQ command forwarded ({} bytes)", data.len());
                                }
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    tracing::debug!("MoQ command dropped (iroh has priority)");
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    tracing::error!("Backend writer died");
                                    backend_died = true;
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            // Track ended gracefully (publisher disconnected).
                            // Stay in the session and wait for reannounce — the browser
                            // will reconnect and the relay will send a new announcement.
                            tracing::info!("MoQ command reader ended, waiting for reannounce...");
                            reader_alive = false;
                        }
                        Err(e) => {
                            err_count += 1;
                            if err_count <= 3 {
                                // Read error — try re-subscribing to same broadcast.
                                // Works for burst recovery (broadcast still alive).
                                tracing::info!("MoQ command read error: {}, re-subscribing ({}/3)...", e, err_count);
                                reader = MoqTrackReader::from_track(
                                    current_broadcast.subscribe_track(&Track::new(track_name)),
                                );
                            } else {
                                // Broadcast is dead (re-subscribe keeps failing).
                                // Disable reader and wait for reannounce.
                                tracing::info!("MoQ command read error: {}, waiting for reannounce...", e);
                                reader_alive = false;
                                err_count = 0;
                            }
                        }
                    }
                }
                // Watch for new broadcast announcements (publisher reconnected)
                announce = origin_consumer.announced() => {
                    match announce {
                        Some((_path, Some(bc))) => {
                            tracing::info!("MoQ command reannounce — switching to new broadcast");
                            current_broadcast = bc;
                            reader = MoqTrackReader::from_track(
                                current_broadcast.subscribe_track(&Track::new(track_name)),
                            );
                            reader_alive = true;
                            timeout.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(30));
                        }
                        Some((_path, None)) => {
                            // Unannounce — publisher went away
                            tracing::debug!("MoQ command unannounce received");
                        }
                        None => {
                            tracing::info!("MoQ command origin closed, reconnecting...");
                            break;
                        }
                    }
                }
                // Idle timeout: no data or reannounce for 30s — reconnect
                _ = &mut timeout => {
                    tracing::info!("MoQ command subscriber idle 30s, reconnecting...");
                    break;
                }
            }
        }

        if backend_died {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
