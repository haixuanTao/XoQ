//! Remote RealSense client via MoQ.
//!
//! Subscribes to "video", "depth", and "metadata" tracks published by
//! `realsense_server`, decodes AV1 frames with NVDEC, and provides
//! synced color+depth pairs with intrinsics.

use anyhow::Result;

use crate::moq::{MoqBuilder, MoqSubscriber, MoqTrackReader};
use crate::nvdec_av1_decoder::{self, NvdecAv1Decoder};

/// Camera intrinsics received from the metadata track.
#[derive(Debug, Clone, Copy)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub ppx: f32,
    pub ppy: f32,
    pub width: u32,
    pub height: u32,
    pub depth_shift: u32,
}

/// A synced pair of color + depth frames.
pub struct RealSenseFrames {
    /// RGB color data (width * height * 3 bytes).
    pub color_rgb: Vec<u8>,
    /// Depth in millimeters (width * height u16 values).
    pub depth_mm: Vec<u16>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Wall-clock timestamp in milliseconds.
    pub timestamp_ms: u64,
}

/// Async RealSense client that subscribes to MoQ tracks.
pub struct RealSenseClient {
    video_reader: MoqTrackReader,
    depth_reader: MoqTrackReader,
    metadata_reader: Option<MoqTrackReader>,
    video_decoder: Box<NvdecAv1Decoder>,
    depth_decoder: Box<NvdecAv1Decoder>,
    intrinsics: Option<Intrinsics>,
    _subscriber: MoqSubscriber,
}

impl RealSenseClient {
    /// Connect to a remote RealSense camera via MoQ relay.
    ///
    /// `path` is the MoQ broadcast path (e.g. "anon/realsense").
    pub async fn connect_moq(path: &str) -> Result<Self> {
        // Try :4443 first, fall back to default
        let relay = if path.contains("://") {
            path.to_string()
        } else {
            format!("https://cdn.1ms.ai/{}", path)
        };

        let (relay_url, moq_path) = if relay.contains("://") {
            let url = url::Url::parse(&relay)?;
            let base = format!(
                "{}://{}:{}",
                url.scheme(),
                url.host_str().unwrap_or("localhost"),
                url.port().unwrap_or(443)
            );
            let p = url.path().trim_start_matches('/').to_string();
            (base, if p.is_empty() { path.to_string() } else { p })
        } else {
            ("https://cdn.1ms.ai".to_string(), path.to_string())
        };

        let mut subscriber = MoqBuilder::new()
            .relay(&relay_url)
            .path(&moq_path)
            .disable_tls_verify()
            .connect_subscriber()
            .await?;

        let video_reader = subscriber
            .subscribe_track("video")
            .await?
            .ok_or_else(|| anyhow::anyhow!("Failed to subscribe to video track"))?;

        let depth_reader = subscriber
            .subscribe_track("depth")
            .await?
            .ok_or_else(|| anyhow::anyhow!("Failed to subscribe to depth track"))?;

        // metadata track is optional (server may not have it yet)
        let metadata_reader = subscriber.subscribe_track("metadata").await.ok().flatten();

        let video_decoder = Box::new(NvdecAv1Decoder::new(false)?); // 8-bit NV12 for color
        let depth_decoder = Box::new(NvdecAv1Decoder::new(true)?); // 10-bit P016 for depth

        Ok(Self {
            video_reader,
            depth_reader,
            metadata_reader,
            video_decoder,
            depth_decoder,
            intrinsics: None,
            _subscriber: subscriber,
        })
    }

    /// Read one synced color+depth frame pair.
    pub async fn read_frames(&mut self) -> Result<RealSenseFrames> {
        // Try to read metadata if available and we don't have intrinsics yet
        if self.intrinsics.is_none() {
            if let Some(ref mut meta_reader) = self.metadata_reader {
                tokio::select! {
                    biased;
                    result = meta_reader.read() => {
                        if let Ok(Some(data)) = result {
                            self.parse_metadata(&data);
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::ZERO) => {}
                }
            }
        }

        // Read color frame
        let color_frame = loop {
            let data = self
                .video_reader
                .read()
                .await?
                .ok_or_else(|| anyhow::anyhow!("Video track ended"))?;

            let (timestamp_ms, payload) = parse_stamped_data(&data);

            let obus = extract_av1_from_cmaf(payload);
            if obus.is_empty() {
                continue;
            }

            match self.video_decoder.decode(&obus)? {
                Some(frame) => break (frame, timestamp_ms),
                None => continue,
            }
        };

        // Read depth frame
        let depth_frame = loop {
            let data = self
                .depth_reader
                .read()
                .await?
                .ok_or_else(|| anyhow::anyhow!("Depth track ended"))?;

            let (_timestamp_ms, payload) = parse_stamped_data(&data);

            let obus = extract_av1_from_cmaf(payload);
            if obus.is_empty() {
                continue;
            }

            match self.depth_decoder.decode(&obus)? {
                Some(frame) => break frame,
                None => continue,
            }
        };

        let (color_decoded, timestamp_ms) = color_frame;

        // Convert depth: P016 Y-plane → u16 mm
        let depth_shift = self.intrinsics.map(|i| i.depth_shift).unwrap_or(0);
        let depth_mm = nvdec_av1_decoder::p016_y_to_depth_mm(&depth_frame.data, depth_shift);

        Ok(RealSenseFrames {
            color_rgb: color_decoded.data,
            depth_mm,
            width: color_decoded.width,
            height: color_decoded.height,
            timestamp_ms,
        })
    }

    /// Get camera intrinsics (from metadata track).
    pub fn intrinsics(&self) -> Option<Intrinsics> {
        self.intrinsics
    }

    fn parse_metadata(&mut self, data: &[u8]) {
        if let Ok(text) = std::str::from_utf8(data) {
            if let Some(intr) = parse_intrinsics_json(text) {
                self.intrinsics = Some(intr);
            }
        }
    }
}

/// Parse the 8-byte LE timestamp prefix from stamped data.
fn parse_stamped_data(data: &[u8]) -> (u64, &[u8]) {
    if data.len() < 8 {
        return (0, data);
    }
    let ms = u64::from_le_bytes(data[..8].try_into().unwrap());
    (ms, &data[8..])
}

/// Extract raw AV1 OBU data from CMAF fMP4 (init + media segments).
fn extract_av1_from_cmaf(data: &[u8]) -> Vec<u8> {
    let mut pos = 0;
    let mut result = Vec::new();

    while pos + 8 <= data.len() {
        let box_size =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let box_type = &data[pos + 4..pos + 8];

        if box_size == 0 || pos + box_size > data.len() {
            break;
        }

        if box_type == b"mdat" {
            result.extend_from_slice(&data[pos + 8..pos + box_size]);
        }

        pos += box_size;
    }

    result
}

/// Parse intrinsics JSON without serde.
fn parse_intrinsics_json(json: &str) -> Option<Intrinsics> {
    fn extract_f32(json: &str, key: &str) -> Option<f32> {
        let pattern = format!("\"{}\":", key);
        let start = json.find(&pattern)? + pattern.len();
        let rest = json[start..].trim_start();
        let end = rest.find(|c: char| c == ',' || c == '}' || c == ' ')?;
        rest[..end].parse().ok()
    }
    fn extract_u32(json: &str, key: &str) -> Option<u32> {
        extract_f32(json, key).map(|v| v as u32)
    }

    Some(Intrinsics {
        fx: extract_f32(json, "fx")?,
        fy: extract_f32(json, "fy")?,
        ppx: extract_f32(json, "ppx")?,
        ppy: extract_f32(json, "ppy")?,
        width: extract_u32(json, "width")?,
        height: extract_u32(json, "height")?,
        depth_shift: extract_u32(json, "depth_shift").unwrap_or(0),
    })
}

/// Blocking wrapper around `RealSenseClient` with its own tokio runtime.
pub struct SyncRealSenseClient {
    inner: RealSenseClient,
    runtime: tokio::runtime::Runtime,
    source: String,
    has_read: bool,
}

impl SyncRealSenseClient {
    /// Connect to a remote RealSense via MoQ path.
    pub fn connect_moq(path: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = runtime.block_on(RealSenseClient::connect_moq(path))?;
        Ok(Self {
            inner: client,
            runtime,
            source: path.to_string(),
            has_read: false,
        })
    }

    /// Auto-detect transport and connect.
    pub fn connect_auto(source: &str) -> Result<Self> {
        Self::connect_moq(source)
    }

    fn reconnect(&mut self) -> Result<()> {
        let client = self
            .runtime
            .block_on(RealSenseClient::connect_moq(&self.source))?;
        self.inner = client;
        Ok(())
    }

    /// Read a synced color+depth frame pair.
    pub fn read_frames(&mut self) -> Result<RealSenseFrames> {
        match self.runtime.block_on(self.inner.read_frames()) {
            Ok(frames) => {
                self.has_read = true;
                Ok(frames)
            }
            Err(e) if !self.has_read => {
                eprintln!("[xoq] First read failed ({e}), reconnecting...");
                self.reconnect()?;
                self.has_read = true;
                self.runtime.block_on(self.inner.read_frames())
            }
            Err(e) => Err(e),
        }
    }

    /// Get camera intrinsics.
    pub fn intrinsics(&self) -> Option<Intrinsics> {
        self.inner.intrinsics()
    }
}
