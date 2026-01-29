//! Encoded camera server example - streams H.264 encoded video over P2P.
//!
//! This example demonstrates hardware-accelerated video encoding using NVENC
//! for low-latency camera streaming.
//!
//! Run with:
//!     cargo run --example camera_server_encoded --features "iroh,camera"
//!
//! Options:
//!     --camera <index>   Camera index (default: 0)
//!     --width <pixels>   Frame width (default: 1280)
//!     --height <pixels>  Frame height (default: 720)
//!     --fps <rate>       Framerate (default: 30)

use anyhow::Result;
use xoq::camera::Camera;
use xoq::iroh::IrohServerBuilder;
use xoq_codec::{Codec, EncoderConfig, NvencEncoder, VideoEncoder, VideoFrame};

const CAMERA_H264_ALPN: &[u8] = b"xoq/camera-h264/0";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // Parse arguments
    let camera_index = args
        .iter()
        .position(|a| a == "--camera")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0u32);

    let width = args
        .iter()
        .position(|a| a == "--width")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1280u32);

    let height = args
        .iter()
        .position(|a| a == "--height")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(720u32);

    let fps = args
        .iter()
        .position(|a| a == "--fps")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(30u32);

    // Open camera
    println!(
        "Opening camera {} at {}x{} @ {} fps...",
        camera_index, width, height, fps
    );
    let mut camera = Camera::open(camera_index, width, height, fps)?;
    let actual_width = camera.width();
    let actual_height = camera.height();
    let actual_fps = camera.fps();
    println!(
        "Camera opened: {}x{} @ {} fps (requested {}x{} @ {})",
        actual_width, actual_height, actual_fps, width, height, fps
    );

    // Create NVENC encoder
    println!("Initializing NVENC encoder...");
    let config = EncoderConfig::new(actual_width, actual_height)
        .codec(Codec::H264)
        .framerate(actual_fps, 1)
        .for_low_latency();

    let mut encoder = NvencEncoder::new(config)?;
    println!(
        "Encoder initialized: {:?} {}x{}",
        encoder.codec(),
        actual_width,
        actual_height
    );

    // Create iroh server
    let server = IrohServerBuilder::new()
        .alpn(CAMERA_H264_ALPN)
        .bind()
        .await?;

    println!("\nEncoded camera server started!");
    println!("Server ID: {}", server.id());
    println!("\nShare this ID with clients to connect.");
    println!("Press Ctrl+C to stop.\n");

    // Accept and handle connections one at a time
    loop {
        let conn = match server.accept().await? {
            Some(c) => c,
            None => break,
        };

        println!("Client connected: {}", conn.remote_id());

        if let Err(e) = handle_client(conn, &mut camera, &mut encoder).await {
            tracing::error!("Client error: {}", e);
        }
        println!("Client disconnected\n");
    }

    Ok(())
}

async fn handle_client(
    conn: xoq::iroh::IrohConnection,
    camera: &mut Camera,
    encoder: &mut NvencEncoder,
) -> Result<()> {
    let stream = conn.accept_stream().await?;
    let (mut send, _recv) = stream.split();

    // Send stream header: codec (1) + width (4) + height (4) = 9 bytes
    let (width, height) = encoder.dimensions();

    let mut header = Vec::with_capacity(9);
    header.push(0x01); // Codec: 0x01 = H.264
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    send.write_all(&header).await?;

    let mut frame_count = 0u64;

    loop {
        // Capture frame
        let frame = camera.capture()?;

        // Convert to VideoFrame for encoder
        let video_frame =
            VideoFrame::from_rgb(frame.width, frame.height, frame.data, frame.timestamp_us);

        // Encode
        let packet = encoder.encode(&video_frame)?;

        // Send packet: flags (1) + pts (8) + length (4) + data
        // flags: bit 0 = keyframe
        let flags: u8 = if packet.is_keyframe { 0x01 } else { 0x00 };

        let mut packet_header = Vec::with_capacity(13);
        packet_header.push(flags);
        packet_header.extend_from_slice(&packet.pts_us.to_le_bytes());
        packet_header.extend_from_slice(&(packet.data.len() as u32).to_le_bytes());

        if let Err(e) = send.write_all(&packet_header).await {
            tracing::debug!("Write error: {}", e);
            break;
        }
        if let Err(e) = send.write_all(&packet.data).await {
            tracing::debug!("Write error: {}", e);
            break;
        }

        frame_count += 1;
        if frame_count % 100 == 0 {
            println!(
                "Sent {} frames, latest: {} bytes, keyframe: {}",
                frame_count,
                packet.data.len(),
                packet.is_keyframe
            );
        }
    }

    Ok(())
}
