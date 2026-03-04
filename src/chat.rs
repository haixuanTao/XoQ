//! MoQ Chat — multi-user chat via MoQ relay.
//!
//! Each user publishes a broadcast at `<username>_<sessionId>` with a "messages" track.
//! Users discover each other via relay announcements and subscribe to each other's tracks.
//! Messages are JSON: `{"name": "str", "text": "str", "ts": millis}`.
//!
//! Protocol is interoperable with the browser chat in `js/examples/openarm-chat.js`.

use anyhow::Result;
use bytes::Bytes;
use moq_native::moq_lite::{self, Broadcast, Origin, Track};
use rand::Rng;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::moq::{MoqTrackReader, MoqTrackWriter};

/// A chat message received from another user.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub name: String,
    pub text: String,
    pub ts: u64,
}

/// Blocking chat client that connects to a MoQ relay for multi-user chat.
///
/// Uses a single duplex MoQ connection with separate origins for publish
/// and consume to avoid announce/unannounce feedback loops.
pub struct ChatClient {
    runtime: tokio::runtime::Runtime,
    writer: Mutex<MoqTrackWriter>,
    rx: Mutex<mpsc::UnboundedReceiver<ChatMessage>>,
    username: String,
    _session: moq_lite::Session,
    _broadcast: moq_lite::BroadcastProducer,
}

impl ChatClient {
    /// Connect to a MoQ relay and join a chat channel.
    pub fn connect(relay: &str, path: &str, username: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let (writer, rx, session, broadcast_producer, username) =
            runtime.block_on(Self::connect_async(relay, path, username))?;
        Ok(Self {
            runtime,
            writer: Mutex::new(writer),
            rx: Mutex::new(rx),
            username,
            _session: session,
            _broadcast: broadcast_producer,
        })
    }

    async fn connect_async(
        relay: &str,
        path: &str,
        username: &str,
    ) -> Result<(
        MoqTrackWriter,
        mpsc::UnboundedReceiver<ChatMessage>,
        moq_lite::Session,
        moq_lite::BroadcastProducer,
        String,
    )> {
        let session_id = format!("{:06x}", rand::rng().random_range(0u32..0x1000000));
        let publish_path = format!("{}_{}", username, session_id);

        let url = url::Url::parse(&format!("{}/{}", relay, path))?;

        let mut config = moq_native::ClientConfig::default();
        config.tls.disable_verify = Some(true);
        let client = config.init()?;

        // Use SEPARATE origins for publish and consume on the SAME connection.
        // A shared origin causes infinite announce/unannounce loops because
        // relay echoes feed back into the origin, triggering reannounce.
        let pub_origin = Origin::produce();
        let sub_origin = Origin::produce();
        let mut broadcast = Broadcast::produce();

        // Create "messages" track before connecting (avoid race with subscribers)
        let track_producer = broadcast.producer.create_track(Track::new("messages"));
        let writer = MoqTrackWriter::from_producer(track_producer);

        // Publish our broadcast
        pub_origin
            .producer
            .publish_broadcast(&publish_path, broadcast.consumer);

        // Single duplex connection with separate origins
        let session = client
            .with_publish(pub_origin.consumer)
            .with_consume(sub_origin.producer)
            .connect(url)
            .await?;

        let (tx, rx) = mpsc::unbounded_channel();

        let our_path = publish_path;
        let origin_consumer = sub_origin.consumer;
        tokio::spawn(async move {
            Self::discovery_loop(origin_consumer, our_path, tx).await;
        });

        Ok((
            writer,
            rx,
            session,
            broadcast.producer,
            username.to_string(),
        ))
    }

    async fn discovery_loop(
        mut origin_consumer: moq_lite::OriginConsumer,
        our_path: String,
        msg_tx: mpsc::UnboundedSender<ChatMessage>,
    ) {
        tracing::debug!("[chat-discovery] started, our_path={}", our_path);

        // Track active subscriptions. When a subscription fails, it signals
        // through retry_tx so we can retry on the next announce.
        let mut subscribed: HashSet<String> = HashSet::new();
        let (retry_tx, mut retry_rx) = mpsc::unbounded_channel::<String>();

        loop {
            // Drain retry signals — mark failed subscriptions as retryable
            while let Ok(path) = retry_rx.try_recv() {
                subscribed.remove(&path);
            }

            tokio::select! {
                announcement = origin_consumer.announced() => {
                    match announcement {
                        Some((path, Some(broadcast_consumer))) => {
                            let path_str = path.as_str().to_string();
                            if path_str == our_path {
                                continue;
                            }
                            if subscribed.contains(&path_str) {
                                continue;
                            }
                            tracing::debug!("[chat-discovery] subscribing to {}", path_str);
                            subscribed.insert(path_str.clone());

                            let msg_tx = msg_tx.clone();
                            let retry_tx = retry_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::subscribe_user(broadcast_consumer, msg_tx).await {
                                    tracing::debug!("[chat-subscribe] error for {}: {}, will retry", path_str, e);
                                    let _ = retry_tx.send(path_str);
                                }
                            });
                        }
                        Some((_path, None)) => continue,
                        None => {
                            tracing::debug!("[chat-discovery] origin closed");
                            break;
                        }
                    }
                }
                // Also check retries when no announcement is pending
                retry = retry_rx.recv() => {
                    if let Some(path) = retry {
                        subscribed.remove(&path);
                    }
                }
            }
        }
    }

    async fn subscribe_user(
        broadcast: moq_lite::BroadcastConsumer,
        tx: mpsc::UnboundedSender<ChatMessage>,
    ) -> Result<()> {
        let track = broadcast.subscribe_track(&Track::new("messages"));
        let mut reader = MoqTrackReader::from_track(track);

        while let Some(data) = reader.read().await? {
            if let Ok(text) = std::str::from_utf8(&data) {
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) {
                    let chat_msg = ChatMessage {
                        name: msg["name"].as_str().unwrap_or("").to_string(),
                        text: msg["text"].as_str().unwrap_or("").to_string(),
                        ts: msg["ts"].as_u64().unwrap_or(0),
                    };
                    tracing::debug!("[chat-subscribe] {}: {}", chat_msg.name, chat_msg.text);
                    if tx.send(chat_msg).is_err() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Send a chat message.
    pub fn send(&self, text: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let msg = serde_json::json!({
            "name": self.username,
            "text": text,
            "ts": now,
        });
        let json_bytes = serde_json::to_vec(&msg)?;
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        writer.write(Bytes::from(json_bytes));
        Ok(())
    }

    /// Receive the next chat message, with optional timeout.
    pub fn recv(&self, timeout: Option<Duration>) -> Result<Option<ChatMessage>> {
        let mut rx = self.rx.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        self.runtime.block_on(async {
            match timeout {
                Some(t) => match tokio::time::timeout(t, rx.recv()).await {
                    Ok(msg) => Ok(msg),
                    Err(_) => Ok(None),
                },
                None => Ok(rx.recv().await),
            }
        })
    }

    /// Get the username.
    pub fn username(&self) -> &str {
        &self.username
    }
}
