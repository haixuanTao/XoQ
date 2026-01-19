//! Camera server example - streams local camera over P2P.
//!
//! Run with: cargo run --example camera_server --features "iroh,camera"

use anyhow::Result;
use xoq::CameraServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Create camera server with default settings (camera 0, 640x480, 30fps)
    let server = CameraServer::new(0, 640, 480, 30, None).await?;

    println!("Camera server started!");
    println!("Server ID: {}", server.id());
    println!("\nShare this ID with clients to connect.");
    println!("Press Ctrl+C to stop.\n");

    // Run forever, handling client connections
    server.run().await?;

    Ok(())
}
