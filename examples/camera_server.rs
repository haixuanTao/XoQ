//! Camera server example - streams local camera over P2P or relay.
//!
//! Run with iroh (P2P):
//!     cargo run --example camera_server --features "iroh,camera"
//!
//! Run with MoQ (relay):
//!     cargo run --example camera_server --features "iroh,camera" -- --moq anon/my-camera

use anyhow::Result;
use xoq::CameraServerBuilder;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    let mut builder = CameraServerBuilder::new()
        .camera_index(2)
        .resolution(640, 480)
        .fps(30);

    // Check for --moq flag
    if let Some(pos) = args.iter().position(|a| a == "--moq") {
        let path = args
            .get(pos + 1)
            .expect("Usage: camera_server --moq <path>");
        builder = builder.moq(path);
        println!("Using MoQ relay transport");
    } else {
        builder = builder.iroh();
        println!("Using iroh P2P transport");
    }

    let mut server = builder.build().await?;

    println!("Camera server started!");
    println!("Server ID: {}", server.id());
    println!("\nShare this ID with clients to connect.");
    println!("Press Ctrl+C to stop.\n");

    server.run().await?;

    Ok(())
}
