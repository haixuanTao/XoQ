// Allow Arc with non-Send types since camera access is serialized through Mutex
#![allow(clippy::arc_with_non_send_sync)]

//! Camera server - streams local camera to remote clients.
//!
//! Supports both iroh P2P and MoQ relay transports.

use crate::camera::Camera;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Transport type for camera streaming.
#[derive(Clone)]
pub enum Transport {
    /// Iroh P2P (direct connection, may have NAT issues)
    Iroh {
        /// Optional path to save/load server identity
        identity_path: Option<String>,
    },
    /// MoQ relay (uses relay server, works through NAT)
    Moq {
        /// MoQ path for the stream (e.g., "anon/my-camera")
        path: String,
        /// Optional custom relay URL (default: <https://cdn.1ms.ai>)
        relay_url: Option<String>,
    },
}

impl Default for Transport {
    fn default() -> Self {
        Transport::Iroh {
            identity_path: None,
        }
    }
}

/// Builder for creating a camera server.
pub struct CameraServerBuilder {
    camera_index: u32,
    width: u32,
    height: u32,
    fps: u32,
    jpeg_quality: u8,
    transport: Transport,
}

impl CameraServerBuilder {
    /// Create a new camera server builder with defaults.
    pub fn new() -> Self {
        Self {
            camera_index: 0,
            width: 640,
            height: 480,
            fps: 30,
            jpeg_quality: 80,
            transport: Transport::default(),
        }
    }

    /// Set camera index.
    pub fn camera_index(mut self, index: u32) -> Self {
        self.camera_index = index;
        self
    }

    /// Set resolution.
    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set FPS.
    pub fn fps(mut self, fps: u32) -> Self {
        self.fps = fps;
        self
    }

    /// Set JPEG quality (1-100).
    pub fn jpeg_quality(mut self, quality: u8) -> Self {
        self.jpeg_quality = quality.clamp(1, 100);
        self
    }

    /// Use iroh P2P transport.
    pub fn iroh(mut self) -> Self {
        self.transport = Transport::Iroh {
            identity_path: None,
        };
        self
    }

    /// Use iroh P2P transport with persistent identity.
    pub fn iroh_with_identity(mut self, path: &str) -> Self {
        self.transport = Transport::Iroh {
            identity_path: Some(path.to_string()),
        };
        self
    }

    /// Use MoQ relay transport.
    pub fn moq(mut self, path: &str) -> Self {
        self.transport = Transport::Moq {
            path: path.to_string(),
            relay_url: None,
        };
        self
    }

    /// Use MoQ relay transport with custom relay URL.
    pub fn moq_with_relay(mut self, path: &str, relay_url: &str) -> Self {
        self.transport = Transport::Moq {
            path: path.to_string(),
            relay_url: Some(relay_url.to_string()),
        };
        self
    }

    /// Build the camera server.
    pub async fn build(self) -> Result<CameraServer> {
        let camera = Camera::open(self.camera_index, self.width, self.height, self.fps)?;

        let inner = match self.transport {
            Transport::Iroh { identity_path } => {
                use crate::iroh::IrohServerBuilder;
                const CAMERA_ALPN: &[u8] = b"xoq/camera/0";

                let mut builder = IrohServerBuilder::new().alpn(CAMERA_ALPN);
                if let Some(path) = identity_path {
                    builder = builder.identity_path(&path);
                }
                let iroh = builder.bind().await?;
                let id = iroh.id().to_string();

                CameraServerInner::Iroh {
                    server: Arc::new(iroh),
                    id,
                }
            }
            Transport::Moq { path, relay_url } => {
                use crate::moq::MoqBuilder;

                let mut builder = MoqBuilder::new().path(&path);
                if let Some(url) = &relay_url {
                    builder = builder.relay(url);
                }
                let mut conn = builder.connect_publisher().await?;
                let track = conn.create_track("camera");

                CameraServerInner::Moq {
                    track,
                    path,
                    _conn: conn,
                }
            }
        };

        Ok(CameraServer {
            camera: Arc::new(Mutex::new(camera)),
            jpeg_quality: self.jpeg_quality,
            inner,
        })
    }
}

impl Default for CameraServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

enum CameraServerInner {
    Iroh {
        server: Arc<crate::iroh::IrohServer>,
        id: String,
    },
    Moq {
        track: crate::moq::MoqTrackWriter,
        path: String,
        _conn: crate::moq::MoqPublisher,
    },
}

/// A server that streams camera frames to remote clients.
pub struct CameraServer {
    camera: Arc<Mutex<Camera>>,
    jpeg_quality: u8,
    inner: CameraServerInner,
}

impl CameraServer {
    /// Create a new camera server with iroh transport (legacy API).
    pub async fn new(
        camera_index: u32,
        width: u32,
        height: u32,
        fps: u32,
        identity_path: Option<&str>,
    ) -> Result<Self> {
        let mut builder = CameraServerBuilder::new()
            .camera_index(camera_index)
            .resolution(width, height)
            .fps(fps);

        if let Some(path) = identity_path {
            builder = builder.iroh_with_identity(path);
        } else {
            builder = builder.iroh();
        }

        builder.build().await
    }

    /// Set JPEG compression quality (1-100, default 80).
    pub fn set_quality(&mut self, quality: u8) {
        self.jpeg_quality = quality.clamp(1, 100);
    }

    /// Get the server's ID (iroh endpoint ID or MoQ path).
    pub fn id(&self) -> String {
        match &self.inner {
            CameraServerInner::Iroh { id, .. } => id.clone(),
            CameraServerInner::Moq { path, .. } => path.clone(),
        }
    }

    /// Run the camera server.
    pub async fn run(&mut self) -> Result<()> {
        match &mut self.inner {
            CameraServerInner::Iroh { server, .. } => {
                let server = server.clone();
                let camera = self.camera.clone();
                let quality = self.jpeg_quality;

                let mut active_cancel: Option<tokio_util::sync::CancellationToken> = None;
                let mut active_task: Option<tokio::task::JoinHandle<()>> = None;

                loop {
                    let conn = server
                        .accept()
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("Server closed"))?;

                    tracing::info!("Client connected: {}", conn.remote_id());

                    // Cancel previous connection if still active
                    if let Some(cancel) = active_cancel.take() {
                        tracing::info!("Disconnecting previous client");
                        cancel.cancel();
                    }
                    if let Some(task) = active_task.take() {
                        let _ = task.await;
                    }

                    let cancel = tokio_util::sync::CancellationToken::new();
                    active_cancel = Some(cancel.clone());

                    let cam = camera.clone();
                    active_task = Some(tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_iroh_connection(conn, &cam, quality, cancel).await
                        {
                            tracing::error!("Connection error: {}", e);
                        }
                        tracing::info!("Client disconnected");
                    }));
                }
            }
            CameraServerInner::Moq { track, .. } => {
                loop {
                    let frame = {
                        let mut cam = self.camera.lock().await;
                        cam.capture()?
                    };

                    let jpeg = frame.to_jpeg(self.jpeg_quality)?;

                    // Frame format: width (4) + height (4) + timestamp (4) + jpeg data
                    let mut data = Vec::with_capacity(12 + jpeg.len());
                    data.extend_from_slice(&frame.width.to_le_bytes());
                    data.extend_from_slice(&frame.height.to_le_bytes());
                    data.extend_from_slice(&(frame.timestamp_us as u32).to_le_bytes());
                    data.extend_from_slice(&jpeg);

                    track.write(data);
                }
            }
        }
    }

    /// Handle a single client connection (iroh only).
    pub async fn run_once(&mut self) -> Result<()> {
        match &self.inner {
            CameraServerInner::Iroh { server, .. } => {
                let conn = server
                    .accept()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Server closed"))?;

                tracing::info!("Client connected: {}", conn.remote_id());
                let cancel = tokio_util::sync::CancellationToken::new();
                Self::handle_iroh_connection(conn, &self.camera, self.jpeg_quality, cancel).await
            }
            CameraServerInner::Moq { .. } => {
                anyhow::bail!("run_once not supported for MoQ transport (use run instead)")
            }
        }
    }

    async fn handle_iroh_connection(
        conn: crate::iroh::IrohConnection,
        camera: &Arc<Mutex<Camera>>,
        quality: u8,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let stream = tokio::select! {
            result = conn.accept_stream() => {
                result.map_err(|e| anyhow::anyhow!("Failed to accept stream: {}", e))?
            }
            _ = cancel.cancelled() => {
                tracing::info!("Connection cancelled while waiting for stream");
                return Ok(());
            }
        };
        let (mut send, _recv) = stream.split();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Connection cancelled (new client connecting)");
                    break;
                }
                frame_result = async {
                    let mut cam = camera.lock().await;
                    cam.capture()
                } => {
                    let frame = frame_result?;
                    let jpeg = frame.to_jpeg(quality)?;

                    // Header: width (4) + height (4) + timestamp (8) + length (4) = 20 bytes
                    let mut header = Vec::with_capacity(20);
                    header.extend_from_slice(&frame.width.to_le_bytes());
                    header.extend_from_slice(&frame.height.to_le_bytes());
                    header.extend_from_slice(&frame.timestamp_us.to_le_bytes());
                    header.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());

                    if let Err(e) = send.write_all(&header).await {
                        tracing::debug!("Write error: {}", e);
                        break;
                    }
                    if let Err(e) = send.write_all(&jpeg).await {
                        tracing::debug!("Write error: {}", e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}
