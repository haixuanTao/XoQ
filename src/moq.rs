//! MoQ transport builder
//!
//! Provides a builder API for creating MoQ clients and servers that communicate
//! via a relay server using the IETF MoQ Transport protocol (draft-14).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use moq_transport::coding::TrackNamespace;
use moq_transport::serve::{
    StreamWriter, SubgroupsReader, Track, TrackReaderMode, Tracks, TracksRequest, TracksWriter,
};
use moq_transport::session::{Session, Subscriber};
use url::Url;

/// Create a QUIC endpoint configured for WebTransport (HTTP/3).
fn create_quic_endpoint() -> Result<quinn::Endpoint> {
    // Install ring as the default crypto provider (required when both ring and
    // aws-lc-rs features are resolved from transitive dependencies).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![web_transport_quinn::ALPN.to_vec()];

    let crypto: quinn::crypto::rustls::QuicClientConfig = tls.try_into()?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
    transport.keep_alive_interval(Some(Duration::from_secs(4)));
    config.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

/// Builder for MoQ connections
pub struct MoqBuilder {
    relay_url: String,
    token: Option<String>,
    path: String,
}

impl MoqBuilder {
    /// Create a new builder with default relay
    pub fn new() -> Self {
        Self {
            relay_url: "https://cdn.moq.dev".to_string(),
            token: None,
            path: "anon/xoq".to_string(),
        }
    }

    /// Set the relay URL
    pub fn relay(mut self, url: &str) -> Self {
        self.relay_url = url.to_string();
        self
    }

    /// Set the path on the relay
    pub fn path(mut self, path: &str) -> Self {
        self.path = path.to_string();
        self
    }

    /// Set authentication token (JWT)
    pub fn token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    /// Build the full URL with optional token
    fn build_url(&self) -> Result<Url> {
        let url_str = match &self.token {
            Some(token) => format!("{}/{}?token={}", self.relay_url, self.path, token),
            None => format!("{}/{}", self.relay_url, self.path),
        };
        Ok(Url::parse(&url_str)?)
    }

    /// Connect as a duplex endpoint (can publish and subscribe)
    pub async fn connect_duplex(self) -> Result<MoqConnection> {
        let url = self.build_url()?;
        let path = self.path.clone();
        let endpoint = create_quic_endpoint()?;

        let wt = web_transport_quinn::connect(&endpoint, &url).await?;
        let wt_session = web_transport::Session::from(wt);

        let (session, publisher, subscriber) = Session::connect(wt_session, None).await?;

        // Session MUST run in background for control/data messages
        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                eprintln!("[xoq] session error: {}", e);
            }
        });

        // Create tracks registry for publishing
        let namespace = TrackNamespace::from_utf8_path(&path);
        let (tracks_writer, tracks_request, tracks_reader) =
            Tracks::new(namespace.clone()).produce();

        // Announce namespace in background (blocks while serving subscriptions)
        let mut pub_handle = publisher;
        let announce_task = tokio::spawn(async move {
            if let Err(e) = pub_handle.announce(tracks_reader).await {
                eprintln!("[xoq] announce error: {}", e);
            }
        });

        Ok(MoqConnection {
            tracks_writer,
            _tracks_request: tracks_request,
            subscriber,
            namespace,
            _session_task: session_task,
            _announce_task: announce_task,
        })
    }

    /// Connect as publisher only
    pub async fn connect_publisher(self) -> Result<MoqPublisher> {
        let url = self.build_url()?;
        let path = self.path.clone();
        let endpoint = create_quic_endpoint()?;

        let wt = web_transport_quinn::connect(&endpoint, &url).await?;
        let wt_session = web_transport::Session::from(wt);

        let (session, publisher, _subscriber) = Session::connect(wt_session, None).await?;

        // Session MUST run in background for control/data messages
        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                eprintln!("[xoq] session error: {}", e);
            }
        });

        // Create tracks registry
        let namespace = TrackNamespace::from_utf8_path(&path);
        let (tracks_writer, tracks_request, tracks_reader) = Tracks::new(namespace).produce();

        // Announce namespace in background (blocks while serving subscriptions)
        let mut pub_handle = publisher;
        let announce_task = tokio::spawn(async move {
            if let Err(e) = pub_handle.announce(tracks_reader).await {
                eprintln!("[xoq] announce error: {}", e);
            }
        });

        Ok(MoqPublisher {
            tracks_writer,
            _tracks_request: tracks_request,
            _session_task: session_task,
            _announce_task: announce_task,
        })
    }

    /// Connect as subscriber only
    pub async fn connect_subscriber(self) -> Result<MoqSubscriber> {
        let url = self.build_url()?;
        let path = self.path.clone();
        eprintln!("[xoq] MoQ subscriber connecting to {}...", url);

        let endpoint = create_quic_endpoint()?;

        let wt = web_transport_quinn::connect(&endpoint, &url).await?;
        let wt_session = web_transport::Session::from(wt);

        let (session, _publisher, subscriber) = Session::connect(wt_session, None).await?;

        // Session MUST run in background for control/data messages
        let session_task = tokio::spawn(async move {
            if let Err(e) = session.run().await {
                eprintln!("[xoq] session error: {}", e);
            }
        });

        let namespace = TrackNamespace::from_utf8_path(&path);

        eprintln!("[xoq] MoQ subscriber connected to relay");

        Ok(MoqSubscriber {
            subscriber,
            namespace,
            _session_task: session_task,
        })
    }
}

impl Default for MoqBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A duplex MoQ connection that can publish and subscribe
pub struct MoqConnection {
    tracks_writer: TracksWriter,
    _tracks_request: TracksRequest,
    subscriber: Subscriber,
    namespace: TrackNamespace,
    _session_task: tokio::task::JoinHandle<()>,
    _announce_task: tokio::task::JoinHandle<()>,
}

impl MoqConnection {
    /// Create a track for publishing
    pub fn create_track(&mut self, name: &str) -> MoqTrackWriter {
        let track_writer = self
            .tracks_writer
            .create(name)
            .expect("tracks reader dropped");
        let stream_writer = track_writer.stream(0).expect("track already has a mode");
        MoqTrackWriter { stream_writer }
    }

    /// Subscribe to a track by name (no announcement handshake needed)
    pub async fn subscribe_track(&mut self, track_name: &str) -> Result<Option<MoqTrackReader>> {
        subscribe_track_impl(&mut self.subscriber, &self.namespace, track_name).await
    }
}

/// A publish-only MoQ connection
pub struct MoqPublisher {
    tracks_writer: TracksWriter,
    _tracks_request: TracksRequest,
    _session_task: tokio::task::JoinHandle<()>,
    _announce_task: tokio::task::JoinHandle<()>,
}

impl MoqPublisher {
    /// Create a track for publishing
    pub fn create_track(&mut self, name: &str) -> MoqTrackWriter {
        let track_writer = self
            .tracks_writer
            .create(name)
            .expect("tracks reader dropped");
        let stream_writer = track_writer.stream(0).expect("track already has a mode");
        MoqTrackWriter { stream_writer }
    }
}

/// A subscribe-only MoQ connection
pub struct MoqSubscriber {
    subscriber: Subscriber,
    namespace: TrackNamespace,
    _session_task: tokio::task::JoinHandle<()>,
}

impl MoqSubscriber {
    /// Subscribe to a track by name (no announcement handshake needed)
    pub async fn subscribe_track(&mut self, track_name: &str) -> Result<Option<MoqTrackReader>> {
        subscribe_track_impl(&mut self.subscriber, &self.namespace, track_name).await
    }
}

/// Shared subscribe logic for both MoqConnection and MoqSubscriber.
async fn subscribe_track_impl(
    subscriber: &mut Subscriber,
    namespace: &TrackNamespace,
    track_name: &str,
) -> Result<Option<MoqTrackReader>> {
    eprintln!(
        "[xoq] Subscribing to track '{}' in namespace '{}'...",
        track_name,
        namespace.to_utf8_path()
    );

    let (track_writer, track_reader) =
        Track::new(namespace.clone(), track_name.to_string()).produce();

    // Subscribe in background (blocks until subscription ends)
    let mut sub = subscriber.clone();
    tokio::spawn(async move {
        if let Err(e) = sub.subscribe(track_writer).await {
            eprintln!("[xoq] subscribe error: {}", e);
        }
    });

    // Wait for the track mode to be set (data must arrive first)
    let mode = tokio::time::timeout(Duration::from_secs(10), track_reader.mode())
        .await
        .map_err(|_| anyhow::anyhow!("Timed out waiting for track data after 10s"))?
        .map_err(|e| anyhow::anyhow!("Track error: {}", e))?;

    eprintln!("[xoq] Track '{}' mode received, creating reader", track_name);

    match mode {
        TrackReaderMode::Stream(stream_reader) => Ok(Some(MoqTrackReader {
            inner: ReaderInner::Stream(stream_reader),
        })),
        TrackReaderMode::Subgroups(subgroups_reader) => Ok(Some(MoqTrackReader {
            inner: ReaderInner::Subgroups {
                subgroups: subgroups_reader,
                current: None,
            },
        })),
        TrackReaderMode::Datagrams(_) => {
            anyhow::bail!("Datagram mode not supported");
        }
    }
}

/// A track writer for publishing data
pub struct MoqTrackWriter {
    stream_writer: StreamWriter,
}

impl MoqTrackWriter {
    /// Write a frame of data. Each write creates a new group with one object.
    pub fn write(&mut self, data: impl Into<Bytes>) {
        let data = data.into();
        if let Ok(mut group) = self.stream_writer.append() {
            let _ = group.write(data);
        }
    }

    /// Write string data
    pub fn write_str(&mut self, data: &str) {
        self.write(Bytes::from(data.to_string()));
    }
}

/// Internal reader state that handles both Stream and Subgroups wire formats.
enum ReaderInner {
    Stream(moq_transport::serve::StreamReader),
    Subgroups {
        subgroups: SubgroupsReader,
        current: Option<moq_transport::serve::SubgroupReader>,
    },
}

/// A track reader for receiving data
pub struct MoqTrackReader {
    inner: ReaderInner,
}

impl MoqTrackReader {
    /// Read the next frame
    pub async fn read(&mut self) -> Result<Option<Bytes>> {
        match &mut self.inner {
            ReaderInner::Stream(stream_reader) => {
                // Get next group, then read the first object from it
                match stream_reader.next().await {
                    Ok(Some(mut group)) => match group.read_next().await {
                        Ok(data) => Ok(data),
                        Err(e) => Err(anyhow::anyhow!("Stream group read error: {}", e)),
                    },
                    Ok(None) => Ok(None),
                    Err(e) => Err(anyhow::anyhow!("Stream read error: {}", e)),
                }
            }
            ReaderInner::Subgroups {
                subgroups,
                current,
            } => {
                loop {
                    // Try to read from current subgroup first
                    if let Some(ref mut reader) = current {
                        match reader.read_next().await {
                            Ok(Some(data)) => return Ok(Some(data)),
                            Ok(None) => {
                                // Current subgroup exhausted, get next one
                                *current = None;
                            }
                            Err(e) => {
                                // Current subgroup errored, try next one
                                eprintln!("[xoq] subgroup read error: {}", e);
                                *current = None;
                            }
                        }
                    }

                    // Get next subgroup
                    match subgroups.next().await {
                        Ok(Some(reader)) => {
                            *current = Some(reader);
                            // Loop back to read from the new subgroup
                        }
                        Ok(None) => return Ok(None),
                        Err(e) => return Err(anyhow::anyhow!("Subgroups read error: {}", e)),
                    }
                }
            }
        }
    }

    /// Read the next frame as string
    pub async fn read_string(&mut self) -> Result<Option<String>> {
        if let Some(bytes) = self.read().await? {
            return Ok(Some(String::from_utf8_lossy(&bytes).to_string()));
        }
        Ok(None)
    }
}
