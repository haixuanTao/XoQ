//! Sync camera client for Python bindings.
//!
//! This module provides a blocking API for remote cameras,
//! managing its own tokio runtime internally.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::frame::Frame;
use crate::iroh::IrohClientBuilder;

const CAMERA_ALPN: &[u8] = b"xoq/camera/0";

/// A synchronous client for remote cameras.
///
/// This client manages its own tokio runtime internally,
/// providing a simple blocking API.
pub struct SyncCameraClient {
    recv: Arc<Mutex<iroh::endpoint::RecvStream>>,
    runtime: tokio::runtime::Runtime,
    _conn: crate::iroh::IrohConnection,
}

impl SyncCameraClient {
    /// Connect to a remote camera server via iroh P2P.
    pub fn connect(server_id: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;

        let (recv, conn) = runtime.block_on(async {
            let conn = IrohClientBuilder::new()
                .alpn(CAMERA_ALPN)
                .connect_str(server_id)
                .await?;
            let stream = conn.open_stream().await?;
            let (_send, recv) = stream.split();
            Ok::<_, anyhow::Error>((recv, conn))
        })?;

        Ok(Self {
            recv: Arc::new(Mutex::new(recv)),
            runtime,
            _conn: conn,
        })
    }

    /// Read a frame from the remote camera.
    pub fn read_frame(&self) -> Result<Frame> {
        self.runtime.block_on(async {
            let (width, height, timestamp, jpeg_data) = {
                let mut recv = self.recv.lock().await;

                let mut header = [0u8; 20];
                recv.read_exact(&mut header).await?;

                let width = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let height = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                let timestamp = u64::from_le_bytes([
                    header[8], header[9], header[10], header[11], header[12], header[13],
                    header[14], header[15],
                ]);
                let length =
                    u32::from_le_bytes([header[16], header[17], header[18], header[19]]);

                let mut jpeg_data = vec![0u8; length as usize];
                recv.read_exact(&mut jpeg_data).await?;

                (width, height, timestamp, jpeg_data)
            };

            // Decode JPEG to RGB
            let mut frame = Frame::from_jpeg(&jpeg_data)?;
            frame.timestamp_us = timestamp;

            // Verify dimensions match
            if frame.width != width || frame.height != height {
                tracing::warn!(
                    "Frame dimension mismatch: expected {}x{}, got {}x{}",
                    width,
                    height,
                    frame.width,
                    frame.height
                );
            }

            Ok(frame)
        })
    }
}
