//! CAN bridge benchmark client - measures latency and throughput
//!
//! Connects to a running can_server and benchmarks query/response performance.
//! Uses Damiao MIT zero-torque command by default to safely poll motor positions
//! without moving the arm. Useful for comparing performance with and without MoQ.
//!
//! Usage: can_benchmark <server-endpoint-id> [options]
//!
//! Options:
//!   --count <n>       Iterations (default: 100)
//!   --interval <ms>   Delay between iterations (default: 20)
//!   --timeout <ms>    Read timeout (default: 500)
//!   --can-id <hex>    CAN ID to send, 0 = cycle 1-8 (default: 0)
//!   --payload <hex>   Hex payload (default: MIT zero-torque query)
//!   --mode <mode>     rtt | write-only | read-only (default: rtt)
//!   --warmup <n>      Warmup iterations (default: 5)
//!   --motors <n>      Number of motors to cycle through (default: 8)
//!
//! Examples:
//!   can_benchmark <id>                          # Query all 8 motors, RTT benchmark
//!   can_benchmark <id> --can-id 0x01            # Query motor 1 only
//!   can_benchmark <id> --count 500 --interval 10

use anyhow::Result;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xoq::socketcan;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Rtt,
    WriteOnly,
    ReadOnly,
}

/// Damiao MIT protocol zero-torque command.
/// p_des=0, v_des=0, kp=0, kd=0, t_ff=0  →  queries position without torque.
const MIT_ZERO_TORQUE: [u8; 8] = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00];

struct Config {
    server_id: String,
    count: usize,
    interval: Duration,
    timeout: Duration,
    can_id: u32, // 0 = cycle through 1..=motors
    payload: Vec<u8>,
    mode: Mode,
    warmup: usize,
    motors: u32,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=debug".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let config = parse_args()?;

    // Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    println!("=== CAN Bridge Benchmark ===");
    println!("Server:   {}", config.server_id);
    println!("Mode:     {:?}", config.mode);
    println!("Count:    {}", config.count);
    println!("Interval: {}ms", config.interval.as_millis());
    println!("Timeout:  {}ms", config.timeout.as_millis());
    if config.can_id == 0 {
        println!(
            "CAN ID:   cycle 0x001-0x{:03x} ({} motors)",
            config.motors, config.motors
        );
    } else {
        println!("CAN ID:   0x{:03x}", config.can_id);
    }
    println!("Payload:  {:02x?} (MIT zero-torque query)", config.payload);
    println!("Warmup:   {}", config.warmup);
    println!();

    println!("Connecting to CAN bridge: {}", config.server_id);
    let mut socket = socketcan::new(&config.server_id)
        .timeout(config.timeout)
        .open()?;
    println!("Connected!");
    println!();

    match config.mode {
        Mode::Rtt => run_rtt(&mut socket, &config, &running),
        Mode::WriteOnly => run_write_only(&mut socket, &config, &running),
        Mode::ReadOnly => run_read_only(&mut socket, &config, &running),
    }
}

fn resolve_can_id(config: &Config, iteration: usize) -> u32 {
    if config.can_id != 0 {
        config.can_id
    } else {
        // Cycle through motors 1..=motors
        ((iteration - 1) % config.motors as usize) as u32 + 1
    }
}

fn run_rtt(
    socket: &mut socketcan::RemoteCanSocket,
    config: &Config,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Drain stale frames
    drain_stale_frames(socket)?;

    // Warmup
    if config.warmup > 0 {
        println!("Warming up ({} iterations)...", config.warmup);
        for w in 0..config.warmup {
            let id = resolve_can_id(config, w + 1);
            let frame = socketcan::CanFrame::new(id, &config.payload)?;
            socket.write_frame(&frame)?;
            let _ = socket.read_frame()?;
            std::thread::sleep(config.interval);
        }
        drain_stale_frames(socket)?;
        println!("Warmup complete.");
        println!();
    }

    println!("Starting RTT benchmark...");
    println!();

    let mut write_times: Vec<Duration> = Vec::with_capacity(config.count);
    let mut read_times: Vec<Duration> = Vec::with_capacity(config.count);
    let mut rtts: Vec<Duration> = Vec::with_capacity(config.count);
    let mut timeouts = 0u64;
    let mut write_errors = 0u64;
    let mut read_errors = 0u64;

    let test_start = Instant::now();

    for i in 1..=config.count {
        if !running.load(Ordering::SeqCst) {
            println!("\nInterrupted at iteration {}", i);
            break;
        }

        let can_id = resolve_can_id(config, i);
        let frame = socketcan::CanFrame::new(can_id, &config.payload)?;

        let t0 = Instant::now();

        // Write
        match socket.write_frame(&frame) {
            Ok(()) => {}
            Err(e) => {
                write_errors += 1;
                println!("  [{}] Write error: {}", i, e);
                std::thread::sleep(config.interval);
                continue;
            }
        }
        let t1 = Instant::now();

        // Read
        match socket.read_frame() {
            Ok(Some(_)) => {
                let t2 = Instant::now();
                let write_time = t1 - t0;
                let read_time = t2 - t1;
                let rtt = t2 - t0;

                write_times.push(write_time);
                read_times.push(read_time);
                rtts.push(rtt);

                // Print slow iterations and every 10th
                if rtt > Duration::from_millis(50) {
                    println!(
                        "  [{}] RTT={:.2}ms (write={:.2}ms, read={:.2}ms) *** SLOW",
                        i,
                        rtt.as_secs_f64() * 1000.0,
                        write_time.as_secs_f64() * 1000.0,
                        read_time.as_secs_f64() * 1000.0,
                    );
                } else if i % 10 == 0 || i == config.count {
                    println!(
                        "  [{}] RTT={:.2}ms (write={:.2}ms, read={:.2}ms)",
                        i,
                        rtt.as_secs_f64() * 1000.0,
                        write_time.as_secs_f64() * 1000.0,
                        read_time.as_secs_f64() * 1000.0,
                    );
                }
            }
            Ok(None) => {
                timeouts += 1;
                let write_time = t1 - t0;
                write_times.push(write_time);
                println!(
                    "  [{}] TIMEOUT (write={:.2}ms)",
                    i,
                    write_time.as_secs_f64() * 1000.0,
                );
            }
            Err(e) => {
                read_errors += 1;
                let write_time = t1 - t0;
                write_times.push(write_time);
                println!(
                    "  [{}] Read error: {} (write={:.2}ms)",
                    i,
                    e,
                    write_time.as_secs_f64() * 1000.0,
                );
            }
        }

        // Wait for interval
        let elapsed = t0.elapsed();
        if elapsed < config.interval {
            std::thread::sleep(config.interval - elapsed);
        }
    }

    let total_time = test_start.elapsed();

    println!();
    println!("{}", "=".repeat(60));
    println!("=== RTT Benchmark Results ===");
    println!();
    println!(
        "Iterations: {} sent, {} successful, {} timeouts, {} write errors, {} read errors",
        config.count,
        rtts.len(),
        timeouts,
        write_errors,
        read_errors
    );
    println!("Total time: {:.2}s", total_time.as_secs_f64());
    println!();

    if !rtts.is_empty() {
        println!("--- Round-trip latency ---");
        print_duration_stats(&rtts);
        println!();
        print_histogram(&rtts);
    }

    if !write_times.is_empty() {
        println!();
        println!("--- Write latency ---");
        print_duration_stats(&write_times);
    }

    if !read_times.is_empty() {
        println!();
        println!("--- Read latency ---");
        print_duration_stats(&read_times);
    }

    Ok(())
}

fn run_write_only(
    socket: &mut socketcan::RemoteCanSocket,
    config: &Config,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Warmup
    if config.warmup > 0 {
        println!("Warming up ({} iterations)...", config.warmup);
        for w in 0..config.warmup {
            let id = resolve_can_id(config, w + 1);
            let frame = socketcan::CanFrame::new(id, &config.payload)?;
            socket.write_frame(&frame)?;
            std::thread::sleep(config.interval);
        }
        println!("Warmup complete.");
        println!();
    }

    println!("Starting write-only benchmark...");
    println!();

    let mut write_times: Vec<Duration> = Vec::with_capacity(config.count);
    let mut write_errors = 0u64;

    let test_start = Instant::now();

    for i in 1..=config.count {
        if !running.load(Ordering::SeqCst) {
            println!("\nInterrupted at iteration {}", i);
            break;
        }

        let can_id = resolve_can_id(config, i);
        let frame = socketcan::CanFrame::new(can_id, &config.payload)?;

        let t0 = Instant::now();
        match socket.write_frame(&frame) {
            Ok(()) => {
                let write_time = t0.elapsed();
                write_times.push(write_time);

                if write_time > Duration::from_millis(50) {
                    println!(
                        "  [{}] write={:.2}ms *** SLOW",
                        i,
                        write_time.as_secs_f64() * 1000.0,
                    );
                } else if i % 10 == 0 || i == config.count {
                    println!("  [{}] write={:.2}ms", i, write_time.as_secs_f64() * 1000.0,);
                }
            }
            Err(e) => {
                write_errors += 1;
                println!("  [{}] Write error: {}", i, e);
            }
        }

        let elapsed = t0.elapsed();
        if elapsed < config.interval {
            std::thread::sleep(config.interval - elapsed);
        }
    }

    let total_time = test_start.elapsed();

    println!();
    println!("{}", "=".repeat(60));
    println!("=== Write-Only Benchmark Results ===");
    println!();
    println!(
        "Iterations: {} sent, {} successful, {} errors",
        config.count,
        write_times.len(),
        write_errors
    );
    println!("Total time: {:.2}s", total_time.as_secs_f64());
    println!();

    if !write_times.is_empty() {
        println!("--- Write latency ---");
        print_duration_stats(&write_times);
        println!();
        print_histogram(&write_times);
    }

    Ok(())
}

fn run_read_only(
    socket: &mut socketcan::RemoteCanSocket,
    config: &Config,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Drain stale frames
    drain_stale_frames(socket)?;

    println!(
        "Starting read-only benchmark (waiting for {} frames)...",
        config.count
    );
    println!();

    let mut read_times: Vec<Duration> = Vec::with_capacity(config.count);
    let mut timeouts = 0u64;
    let mut read_errors = 0u64;

    let test_start = Instant::now();
    let mut frames_received = 0usize;

    while frames_received < config.count {
        if !running.load(Ordering::SeqCst) {
            println!("\nInterrupted after {} frames", frames_received);
            break;
        }

        let t0 = Instant::now();
        match socket.read_frame() {
            Ok(Some(frame)) => {
                let read_time = t0.elapsed();
                frames_received += 1;
                read_times.push(read_time);

                if read_time > Duration::from_millis(50) {
                    println!(
                        "  [{}] read={:.2}ms ID=0x{:03x} len={} *** SLOW",
                        frames_received,
                        read_time.as_secs_f64() * 1000.0,
                        frame.id(),
                        frame.data().len(),
                    );
                } else if frames_received % 10 == 0 || frames_received == config.count {
                    println!(
                        "  [{}] read={:.2}ms ID=0x{:03x} len={}",
                        frames_received,
                        read_time.as_secs_f64() * 1000.0,
                        frame.id(),
                        frame.data().len(),
                    );
                }
            }
            Ok(None) => {
                timeouts += 1;
            }
            Err(e) => {
                read_errors += 1;
                println!("  Read error: {}", e);
            }
        }
    }

    let total_time = test_start.elapsed();

    println!();
    println!("{}", "=".repeat(60));
    println!("=== Read-Only Benchmark Results ===");
    println!();
    println!(
        "Frames: {} received, {} timeouts, {} errors",
        read_times.len(),
        timeouts,
        read_errors,
    );
    println!("Total time: {:.2}s", total_time.as_secs_f64());
    if !read_times.is_empty() {
        println!(
            "Throughput: {:.1} frames/sec",
            read_times.len() as f64 / total_time.as_secs_f64()
        );
    }
    println!();

    if !read_times.is_empty() {
        println!("--- Read latency ---");
        print_duration_stats(&read_times);
        println!();
        print_histogram(&read_times);
    }

    Ok(())
}

fn drain_stale_frames(socket: &mut socketcan::RemoteCanSocket) -> Result<()> {
    let old_timeout = socket.timeout();
    socket.set_timeout(Duration::from_millis(50))?;
    let mut drained = 0;
    loop {
        match socket.read_frame()? {
            Some(_) => drained += 1,
            None => break,
        }
    }
    socket.set_timeout(old_timeout)?;
    if drained > 0 {
        println!("Drained {} stale frames", drained);
    }
    Ok(())
}

fn print_duration_stats(times: &[Duration]) {
    let mut sorted: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let min = sorted[0];
    let max = sorted[sorted.len() - 1];
    let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let median = if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    let p95_idx = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    let p99_idx = ((sorted.len() as f64 * 0.99).ceil() as usize).saturating_sub(1);
    let p95 = sorted[p95_idx.min(sorted.len() - 1)];
    let p99 = sorted[p99_idx.min(sorted.len() - 1)];

    let variance = sorted.iter().map(|x| (x - avg).powi(2)).sum::<f64>() / sorted.len() as f64;
    let std_dev = variance.sqrt();

    println!("  Min:    {:>8.2}ms", min);
    println!("  Max:    {:>8.2}ms", max);
    println!("  Avg:    {:>8.2}ms", avg);
    println!("  Median: {:>8.2}ms", median);
    println!("  P95:    {:>8.2}ms", p95);
    println!("  P99:    {:>8.2}ms", p99);
    println!("  StdDev: {:>8.2}ms", std_dev);
}

fn print_histogram(times: &[Duration]) {
    let sorted: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    let n = sorted.len() as f64;

    let buckets: &[(f64, &str)] = &[
        (1.0, "<= 1ms"),
        (2.0, "<= 2ms"),
        (5.0, "<= 5ms"),
        (10.0, "<= 10ms"),
        (20.0, "<= 20ms"),
        (50.0, "<= 50ms"),
        (100.0, "<= 100ms"),
    ];

    println!("Histogram:");
    for &(limit, label) in buckets {
        let count = sorted.iter().filter(|&&x| x <= limit).count();
        println!(
            "  {:>10}: {:>5} ({:>5.1}%)",
            label,
            count,
            count as f64 / n * 100.0
        );
    }
    let over_100 = sorted.iter().filter(|&&x| x > 100.0).count();
    println!(
        "  {:>10}: {:>5} ({:>5.1}%)",
        "> 100ms",
        over_100,
        over_100 as f64 / n * 100.0
    );
}

fn parse_args() -> Result<Config> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: can_benchmark <server-endpoint-id> [options]");
        println!();
        println!("Options:");
        println!("  --count <n>       Iterations (default: 100)");
        println!("  --interval <ms>   Delay between iterations (default: 20)");
        println!("  --timeout <ms>    Read timeout (default: 500)");
        println!("  --can-id <hex>    CAN ID to send, 0 = cycle 1-N (default: 0)");
        println!("  --payload <hex>   Hex payload (default: MIT zero-torque query)");
        println!("  --mode <mode>     rtt | write-only | read-only (default: rtt)");
        println!("  --warmup <n>      Warmup iterations (default: 5)");
        println!("  --motors <n>      Number of motors to cycle (default: 8)");
        println!();
        println!("Examples:");
        println!("  can_benchmark <id>                          # Query all 8 motors");
        println!("  can_benchmark <id> --can-id 0x01            # Query motor 1 only");
        println!("  can_benchmark <id> --count 500 --interval 10");
        std::process::exit(0);
    }

    let server_id = args[1].clone();

    let count: usize = parse_arg(&args, "--count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let interval_ms: u64 = parse_arg(&args, "--interval")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let timeout_ms: u64 = parse_arg(&args, "--timeout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let can_id: u32 = parse_arg(&args, "--can-id")
        .map(|s| {
            if s.starts_with("0x") || s.starts_with("0X") {
                u32::from_str_radix(&s[2..], 16).unwrap_or(0)
            } else {
                s.parse().unwrap_or(0)
            }
        })
        .unwrap_or(0); // 0 = cycle through motors

    let payload: Vec<u8> = parse_arg(&args, "--payload")
        .map(|s| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
                .collect()
        })
        .unwrap_or_else(|| MIT_ZERO_TORQUE.to_vec());

    let mode = match parse_arg(&args, "--mode").as_deref() {
        Some("write-only") => Mode::WriteOnly,
        Some("read-only") => Mode::ReadOnly,
        Some("rtt") | None => Mode::Rtt,
        Some(other) => {
            eprintln!("Unknown mode: {}. Use: rtt, write-only, read-only", other);
            std::process::exit(1);
        }
    };

    let warmup: usize = parse_arg(&args, "--warmup")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let motors: u32 = parse_arg(&args, "--motors")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    Ok(Config {
        server_id,
        count,
        interval: Duration::from_millis(interval_ms),
        timeout: Duration::from_millis(timeout_ms),
        can_id,
        payload,
        mode,
        warmup,
        motors,
    })
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}
