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
/// Connects as a duplex MoQ endpoint: publishes own messages and discovers/subscribes
/// to other users' broadcasts automatically.
pub struct ChatClient {
    runtime: tokio::runtime::Runtime,
    writer: Mutex<MoqTrackWriter>,
    rx: Mutex<mpsc::UnboundedReceiver<ChatMessage>>,
    username: String,
    _session: moq_lite::Session,
}

impl ChatClient {
    /// Connect to a MoQ relay and join a chat channel.
    ///
    /// - `relay`: MoQ relay URL (e.g., "https://cdn.1ms.ai")
    /// - `path`: Chat channel path (e.g., "anon/openarm-chat")
    /// - `username`: Display name for outgoing messages
    pub fn connect(relay: &str, path: &str, username: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let (writer, rx, session, username) =
            runtime.block_on(Self::connect_async(relay, path, username))?;
        Ok(Self {
            runtime,
            writer: Mutex::new(writer),
            rx: Mutex::new(rx),
            username,
            _session: session,
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
        String,
    )> {
        let session_id = format!("{:06x}", rand::rng().random_range(0u32..0x1000000));
        let publish_path = format!("{}_{}", username, session_id);

        let url = url::Url::parse(&format!("{}/{}", relay, path))?;

        let mut config = moq_native::ClientConfig::default();
        config.tls.disable_verify = Some(true);
        let client = config.init()?;

        let origin = Origin::produce();
        let mut broadcast = Broadcast::produce();

        // Create "messages" track before connecting (avoid race with subscribers)
        let track_producer = broadcast.producer.create_track(Track::new("messages"));
        let writer = MoqTrackWriter::from_producer(track_producer);

        // Publish our broadcast at <username>_<sessionId>
        origin
            .producer
            .publish_broadcast(&publish_path, broadcast.consumer);

        let session = client
            .with_publish(origin.consumer.clone())
            .with_consume(origin.producer.clone())
            .connect(url)
            .await?;

        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn discovery task to find and subscribe to other users
        let our_path = publish_path;
        let origin_consumer = origin.consumer.clone();
        tokio::spawn(async move {
            Self::discovery_loop(origin_consumer, our_path, tx).await;
        });

        Ok((writer, rx, session, username.to_string()))
    }

    async fn discovery_loop(
        mut origin_consumer: moq_lite::OriginConsumer,
        our_path: String,
        tx: mpsc::UnboundedSender<ChatMessage>,
    ) {
        loop {
            match origin_consumer.announced().await {
                Some((path, Some(broadcast_consumer))) => {
                    let path_str = path.as_str().to_string();
                    // Skip our own broadcast
                    if path_str == our_path {
                        continue;
                    }
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::subscribe_user(broadcast_consumer, tx).await {
                            tracing::debug!("Chat subscriber error for {}: {}", path_str, e);
                        }
                    });
                }
                Some((_path, None)) => continue, // unannounce
                None => break,                   // origin closed
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
                    if tx.send(chat_msg).is_err() {
                        break; // receiver dropped
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
    ///
    /// Returns `None` if timeout expires or channel closes.
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
