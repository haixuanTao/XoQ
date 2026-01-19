//! Camera client example - receives frames from remote camera server.
//!
//! Run with: cargo run --example camera_client --features "iroh,camera" -- <server-id>

use anyhow::Result;
use xoq::CameraClient;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let server_id = std::env::args()
        .nth(1)
        .expect("Usage: camera_client <server-id>");

    println!("Connecting to camera server: {}", server_id);

    let mut client = CameraClient::connect(&server_id).await?;

    println!("Connected! Reading frames...\n");

    // Read frames in a loop
    loop {
        let frame = client.read_frame().await?;
        println!(
            "Frame: {}x{}, {} bytes, timestamp: {}us",
            frame.width,
            frame.height,
            frame.data.len(),
            frame.timestamp_us
        );
    }
}
