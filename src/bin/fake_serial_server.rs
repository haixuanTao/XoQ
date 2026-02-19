//! Fake serial server - simulates a serial echo device over iroh P2P + optional MoQ.
//!
//! Drop-in replacement for serial-server that doesn't need serial hardware.
//! Accepts iroh connections from clients, echoes received data back, and
//! optionally publishes echo data to MoQ for browser monitoring.
//!
//! Usage:
//!   fake-serial-server [OPTIONS]
//!
//! Options:
//!   --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)
//!   --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)
//!   --moq-insecure       Disable TLS verification for MoQ
//!   --key-dir <path>     Directory for identity key files (default: current dir)
//!
//! Examples:
//!   fake-serial-server                                             # iroh only
//!   fake-serial-server --moq-relay https://cdn.1ms.ai              # iroh + MoQ

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use xoq::iroh::{IrohConnection, IrohServerBuilder};
use xoq::{MoqBuilder, MoqPublisher, MoqTrackWriter};

struct Args {
    moq_relay: Option<String>,
    moq_path: String,
    moq_insecure: bool,
    key_dir: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut result = Args {
        moq_relay: None,
        moq_path: "anon/xoq-serial".to_string(),
        moq_insecure: false,
        key_dir: ".".to_string(),
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--moq-relay" if i + 1 < args.len() => {
                result.moq_relay = Some(args[i + 1].clone());
                i += 2;
            }
            "--moq-path" if i + 1 < args.len() => {
                result.moq_path = args[i + 1].clone();
                i += 2;
            }
            "--moq-insecure" => {
                result.moq_insecure = true;
                i += 1;
            }
            "--key-dir" if i + 1 < args.len() => {
                result.key_dir = args[i + 1].clone();
                i += 2;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                i += 1;
            }
        }
    }

    result
}

fn print_usage() {
    println!("Fake Serial Server - simulates serial echo device over iroh P2P + MoQ");
    println!();
    println!("Usage: fake-serial-server [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --moq-relay <url>    MoQ relay URL (enables MoQ alongside iroh)");
    println!("  --moq-path <path>    MoQ broadcast path (default: anon/xoq-serial)");
    println!("  --moq-insecure       Disable TLS verification for MoQ");
    println!("  --key-dir <path>     Directory for identity key files (default: .)");
    println!();
    println!("Examples:");
    println!("  fake-serial-server                                             # iroh only");
    println!("  fake-serial-server --moq-relay https://cdn.1ms.ai              # iroh + MoQ");
}

/// Handle a single iroh connection: echo received data back.
async fn handle_connection(
    conn: IrohConnection,
    moq_writer: Option<Arc<std::sync::Mutex<MoqTrackWriter>>>,
    cancel: CancellationToken,
) -> Result<()> {
    let stream = tokio::select! {
        result = conn.accept_stream() => result?,
        _ = cancel.cancelled() => return Ok(()),
    };

    let (mut send, mut recv) = stream.split();

    let mut buf = vec![0u8; 1024];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            read_result = recv.read(&mut buf) => {
                match read_result {
                    Ok(Some(n)) if n > 0 => {
                        tracing::debug!("Echo: {} bytes", n);
                        let data = buf[..n].to_vec();

                        // Echo back over iroh stream
                        if send.write_all(&data).await.is_err() {
                            break;
                        }
                        tokio::task::yield_now().await;

                        // Publish to MoQ if configured
                        if let Some(ref writer) = moq_writer {
                            writer.lock().unwrap().write(data);
                        }
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(e) => return Err(anyhow::anyhow!("Read error: {}", e)),
                }
            }
        }
    }

    Ok(())
}

/// Subscribe to MoQ c2s data and forward as echo responses via MoQ s2c.
/// This allows browser clients to send data and see the echo response.
async fn moq_echo_bridge(
    relay: &str,
    path: &str,
    insecure: bool,
    moq_writer: Arc<std::sync::Mutex<MoqTrackWriter>>,
) -> Result<()> {
    let mut builder = MoqBuilder::new().relay(relay);
    if insecure {
        builder = builder.disable_tls_verify();
    }

    let c2s_path = format!("{}/c2s", path);

    loop {
        tracing::info!("MoQ c2s subscriber connecting on {}...", c2s_path);

        let c2s_sub = match builder.clone().path(&c2s_path).connect_subscriber().await {
            Ok(sub) => sub,
            Err(e) => {
                tracing::warn!("MoQ c2s connect error: {}, retrying...", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let (c2s_reader, c2s_sub) = match tokio::time::timeout(Duration::from_secs(5), async {
            let mut sub = c2s_sub;
            let result = sub.subscribe_track("data").await;
            (result, sub)
        })
        .await
        {
            Ok((Ok(Some(reader)), sub)) => {
                tracing::info!("MoQ c2s subscriber connected on {}", c2s_path);
                (reader, sub)
            }
            Ok((Ok(None), sub)) => {
                tracing::debug!("c2s broadcast ended, retrying...");
                drop(sub);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Ok((Err(e), sub)) => {
                tracing::debug!("c2s subscribe error: {}, retrying...", e);
                drop(sub);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => {
                tracing::debug!("No c2s publisher yet, retrying...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut c2s_reader = c2s_reader;
        let _c2s_sub = c2s_sub;

        loop {
            match c2s_reader.read().await {
                Ok(Some(data)) => {
                    tracing::debug!("MoQ echo: {} bytes", data.len());
                    moq_writer.lock().unwrap().write(data);
                }
                Ok(None) => {
                    tracing::info!("MoQ c2s stream ended, will reconnect...");
                    break;
                }
                Err(e) => {
                    tracing::warn!("MoQ c2s read error: {}, will reconnect...", e);
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("warn".parse()?),
        )
        .init();

    let args = parse_args();

    println!();
    println!("========================================");
    println!("Fake Serial Server (echo)");
    println!("========================================");
    if let Some(ref relay) = args.moq_relay {
        println!("MoQ relay: {}", relay);
        println!("MoQ path:  {}", args.moq_path);
        println!("MoQ s2c:   {}/s2c", args.moq_path);
        println!("MoQ c2s:   {}/c2s", args.moq_path);
    } else {
        println!("MoQ:       disabled");
    }
    println!("Mode:      echo (returns received data)");
    println!("========================================");
    println!();

    // Start iroh server
    let identity_path = format!("{}/.xoq_fake_serial_server_key", args.key_dir);
    let server = IrohServerBuilder::new()
        .identity_path(&identity_path)
        .bind()
        .await?;

    let server_id = server.id().to_string();
    tracing::info!("Server ID: {}", server_id);
    println!("Server ID: {}", server_id);
    println!();

    let server = Arc::new(server);

    // Connect MoQ s2c publisher (publishes echo responses)
    let (moq_writer, _moq_publisher): (
        Option<Arc<std::sync::Mutex<MoqTrackWriter>>>,
        Option<MoqPublisher>,
    ) = if let Some(ref relay) = args.moq_relay {
        let mut builder = MoqBuilder::new().relay(relay);
        if args.moq_insecure {
            builder = builder.disable_tls_verify();
        }

        match builder
            .path(&format!("{}/s2c", args.moq_path))
            .connect_publisher_with_track("data")
            .await
        {
            Ok((publisher, writer)) => {
                tracing::info!("MoQ s2c publisher connected on {}/s2c", args.moq_path);
                (
                    Some(Arc::new(std::sync::Mutex::new(writer))),
                    Some(publisher),
                )
            }
            Err(e) => {
                tracing::warn!("MoQ connect failed (continuing without): {}", e);
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Spawn MoQ echo bridge (c2s -> s2c) if MoQ is configured
    if let (Some(ref relay), Some(ref writer)) = (&args.moq_relay, &moq_writer) {
        let relay = relay.clone();
        let path = args.moq_path.clone();
        let insecure = args.moq_insecure;
        let writer = Arc::clone(writer);
        tokio::spawn(async move {
            if let Err(e) = moq_echo_bridge(&relay, &path, insecure, writer).await {
                tracing::error!("MoQ echo bridge error: {}", e);
            }
        });
    }

    // Accept iroh connections
    tracing::info!("Waiting for iroh connections...");

    let mut current_conn: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;

    loop {
        let conn = match server.accept().await? {
            Some(c) => c,
            None => continue,
        };

        tracing::info!("Client connected: {}", conn.remote_id());

        // Cancel previous connection
        if let Some((cancel, handle)) = current_conn.take() {
            tracing::info!("New client connected, closing previous connection");
            cancel.cancel();
            let _ = handle.await;
        }

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let moq_writer = moq_writer.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = handle_connection(conn, moq_writer, cancel_clone).await {
                tracing::error!("Connection error: {}", e);
            }
            tracing::info!("Client disconnected");
        });

        current_conn = Some((cancel, handle));
    }
}
