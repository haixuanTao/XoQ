//! Camera client example - receives frames from remote camera server.
//!
//! Run with iroh (P2P):
//!     cargo run --example camera_client --features "iroh,camera" -- <server-id>
//!
//! Run with MoQ (relay):
//!     cargo run --example camera_client --features "iroh,camera" -- --moq anon/my-camera

use anyhow::Result;
use xoq::CameraClientBuilder;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    let mut client = if let Some(pos) = args.iter().position(|a| a == "--moq") {
        let path = args
            .get(pos + 1)
            .expect("Usage: camera_client --moq <path>");
        println!("Connecting via MoQ relay: {}", path);
        CameraClientBuilder::new().moq(path).connect().await?
    } else {
        let server_id = args
            .get(1)
            .expect("Usage: camera_client <server-id> or camera_client --moq <path>");
        println!("Connecting via iroh P2P: {}", server_id);
        CameraClientBuilder::new().iroh(server_id).connect().await?
    };

    println!("Connected! Reading frames...\n");

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
