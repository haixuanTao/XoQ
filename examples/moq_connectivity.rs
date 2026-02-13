//! MoQ connectivity checker
//!
//! Subscribes to MoQ broadcast paths and verifies data arrives.
//! Used in CI to check that hardware servers are publishing.
//!
//! Usage:
//!   cargo run --example moq_connectivity -- \
//!     anon/xoq-can-can0/state:can \
//!     anon/xoq-can-can1/state:can \
//!     anon/realsense:video
//!
//! Each argument is `path:track`. Default relay is https://cdn.1ms.ai.
//! Override with --relay <url>.

use std::time::Duration;

use anyhow::Result;

const DEFAULT_RELAY: &str = "https://cdn.1ms.ai";
const TIMEOUT_SECS: u64 = 30;

struct CheckResult {
    path: String,
    track: String,
    ok: bool,
    detail: String,
}

async fn check_path(relay: &str, path: &str, track_name: &str) -> CheckResult {
    let label = format!("{}/{}", path, track_name);
    eprintln!("[check] {} — connecting...", label);

    let result = tokio::time::timeout(
        Duration::from_secs(TIMEOUT_SECS),
        check_inner(relay, path, track_name),
    )
    .await;

    match result {
        Ok(Ok(nbytes)) => {
            eprintln!("[check] {} — OK ({} bytes)", label, nbytes);
            CheckResult {
                path: path.to_string(),
                track: track_name.to_string(),
                ok: true,
                detail: format!("{} bytes", nbytes),
            }
        }
        Ok(Err(e)) => {
            eprintln!("[check] {} — FAIL: {}", label, e);
            CheckResult {
                path: path.to_string(),
                track: track_name.to_string(),
                ok: false,
                detail: format!("{}", e),
            }
        }
        Err(_) => {
            eprintln!("[check] {} — FAIL: timeout ({}s)", label, TIMEOUT_SECS);
            CheckResult {
                path: path.to_string(),
                track: track_name.to_string(),
                ok: false,
                detail: format!("timeout ({}s)", TIMEOUT_SECS),
            }
        }
    }
}

async fn check_inner(relay: &str, path: &str, track_name: &str) -> Result<usize> {
    let mut sub = xoq::MoqBuilder::new()
        .relay(relay)
        .path(path)
        .connect_subscriber()
        .await?;

    let mut reader = sub
        .subscribe_track(track_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no broadcast announced"))?;

    let data = reader
        .read()
        .await?
        .ok_or_else(|| anyhow::anyhow!("track ended without data"))?;

    Ok(data.len())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("warn".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut relay = DEFAULT_RELAY.to_string();
    let mut targets: Vec<(String, String)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--relay" {
            i += 1;
            relay = args.get(i).cloned().unwrap_or_else(|| {
                eprintln!("--relay requires a URL");
                std::process::exit(1);
            });
        } else if let Some((path, track)) = args[i].rsplit_once(':') {
            targets.push((path.to_string(), track.to_string()));
        } else {
            eprintln!("Invalid target '{}' — expected path:track", args[i]);
            std::process::exit(1);
        }
        i += 1;
    }

    if targets.is_empty() {
        eprintln!("Usage: moq_connectivity [--relay URL] <path:track> [path:track ...]");
        eprintln!();
        eprintln!("Example:");
        eprintln!("  moq_connectivity anon/xoq-can-can0/state:can anon/realsense:video");
        std::process::exit(1);
    }

    eprintln!("Relay: {}", relay);
    eprintln!("Checking {} target(s)...\n", targets.len());

    // Run all checks concurrently
    let mut handles = Vec::new();
    for (path, track) in &targets {
        let relay = relay.clone();
        let path = path.clone();
        let track = track.clone();
        handles.push(tokio::spawn(async move {
            check_path(&relay, &path, &track).await
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await?);
    }

    // Print summary table
    eprintln!();
    eprintln!("{:<40} {:<8} {}", "PATH", "STATUS", "DETAIL");
    eprintln!("{}", "-".repeat(70));
    let mut any_failed = false;
    for r in &results {
        let status = if r.ok { "OK" } else { "FAIL" };
        if !r.ok {
            any_failed = true;
        }
        eprintln!(
            "{:<40} {:<8} {}",
            format!("{}:{}", r.path, r.track),
            status,
            r.detail
        );
    }

    let passed = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    eprintln!();
    eprintln!("{}/{} checks passed", passed, total);

    if any_failed {
        std::process::exit(1);
    }

    Ok(())
}
