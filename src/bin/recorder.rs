//! MoQ Data Recorder — subscribes to live MoQ broadcasts and saves to a single multi-track fMP4.
//!
//! Subscribes to video (AV1 8-bit), depth (AV1 10-bit), and CAN (raw binary) tracks,
//! then re-muxes everything into one fMP4 file with independent per-track timestamps.
//!
//! Usage:
//!   recorder [options]
//!
//! Options:
//!   --config <file>         Load config JSON (OpenArm v3 format)
//!   --relay <url>           MoQ relay URL (default: https://cdn.1ms.ai)
//!   --video-path <path>     Video broadcast path (repeatable, overrides config)
//!   --can-path <path>       CAN broadcast path (repeatable, overrides config)
//!   --prefix <name>         Output file prefix (default: recording_YYYYMMDD_HHMMSS)
//!   --output-dir <dir>      Output directory (default: .)
//!   --no-video              Skip video tracks
//!   --no-depth              Skip depth tracks
//!   --duration <seconds>    Stop after N seconds (default: Ctrl+C)
//!   --insecure              Disable TLS verification

use anyhow::Result;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use xoq::cmaf::{
    parse_cmaf_init, parse_cmaf_media_segment, MultiTrackRecorder, SampleEntry, TrackConfig,
    TrackFragment,
};
use xoq::{MoqBuilder, MoqTrackReader};

// ---------------------------------------------------------------------------
// JSON helpers (minimal parsing without serde)
// ---------------------------------------------------------------------------

fn extract_string_field(s: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let quote1 = after.find('"')?;
    let rest = &after[quote1 + 1..];
    let quote2 = rest.find('"')?;
    Some(rest[..quote2].to_string())
}

fn extract_bool_field(s: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn find_matching_bracket(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        if ch == open {
            depth += 1;
        }
        if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn iter_objects(s: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0;
    let mut obj_start = None;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start {
                        results.push(&s[start..=i]);
                    }
                }
            }
            _ => {}
        }
    }
    results
}

/// Extract an array field from JSON as a raw string slice (e.g. `"realsense": [...]`).
fn extract_array_field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let bracket = after.find('[')?;
    let arr_start = idx + pattern.len() + bracket;
    let arr_content = &s[arr_start..];
    let end = find_matching_bracket(arr_content, '[', ']')?;
    Some(&s[arr_start..arr_start + end + 1])
}

/// Extract the "general" object from config JSON.
fn extract_object_field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\"", key);
    let idx = s.find(&pattern)?;
    let after = &s[idx + pattern.len()..];
    let brace = after.find('{')?;
    let obj_start = idx + pattern.len() + brace;
    let obj_content = &s[obj_start..];
    let end = find_matching_bracket(obj_content, '{', '}')?;
    Some(&s[obj_start..obj_start + end + 1])
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

struct RealsenseConfig {
    label: String,
    path: String,
}

struct ConfigSources {
    relay: Option<String>,
    realsense: Vec<RealsenseConfig>,
    can_paths: Vec<String>,
}

fn parse_config_file(path: &str) -> Result<ConfigSources> {
    let content = std::fs::read_to_string(path)?;
    let mut sources = ConfigSources {
        relay: None,
        realsense: Vec::new(),
        can_paths: Vec::new(),
    };

    // general.relay
    if let Some(general) = extract_object_field(&content, "general") {
        sources.relay = extract_string_field(general, "relay");
    }

    // realsense[] — enabled entries
    if let Some(rs_array) = extract_array_field(&content, "realsense") {
        for obj in iter_objects(rs_array) {
            let enabled = extract_bool_field(obj, "enabled").unwrap_or(false);
            if !enabled {
                continue;
            }
            let path = match extract_string_field(obj, "path") {
                Some(p) => p,
                None => continue,
            };
            let label = extract_string_field(obj, "label").unwrap_or_else(|| path.clone());
            sources.realsense.push(RealsenseConfig { label, path });
        }
    }

    // armPairs[] — enabled entries, both leftPath and rightPath → {path}/state
    if let Some(arm_array) = extract_array_field(&content, "armPairs") {
        for obj in iter_objects(arm_array) {
            let enabled = extract_bool_field(obj, "enabled").unwrap_or(false);
            if !enabled {
                continue;
            }
            if let Some(left) = extract_string_field(obj, "leftPath") {
                sources.can_paths.push(format!("{}/state", left));
            }
            if let Some(right) = extract_string_field(obj, "rightPath") {
                sources.can_paths.push(format!("{}/state", right));
            }
        }
    }

    Ok(sources)
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn chrono_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let time_of_day = now % secs_per_day;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut y = 1970i64;
    let mut remaining_days = days as i64;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in_months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    for &dim in &days_in_months {
        if remaining_days < dim {
            break;
        }
        remaining_days -= dim;
        m += 1;
    }
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        y,
        m + 1,
        remaining_days + 1,
        hours,
        minutes,
        seconds
    )
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Args {
    relay: String,
    video_paths: Vec<String>,
    video_labels: Vec<String>,
    can_paths: Vec<String>,
    prefix: String,
    output_dir: String,
    record_video: bool,
    record_depth: bool,
    duration_secs: Option<u64>,
    insecure: bool,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let now = chrono_timestamp();
    let mut relay = "https://cdn.1ms.ai".to_string();
    let mut video_paths: Vec<String> = Vec::new();
    let mut video_labels: Vec<String> = Vec::new();
    let mut can_paths: Vec<String> = Vec::new();
    let mut prefix = format!("recording_{}", now);
    let mut output_dir = ".".to_string();
    let mut record_video = true;
    let mut record_depth = true;
    let mut duration_secs: Option<u64> = None;
    let mut insecure = false;
    let mut config_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                config_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--relay" if i + 1 < args.len() => {
                relay = args[i + 1].clone();
                i += 2;
            }
            "--video-path" if i + 1 < args.len() => {
                video_paths.push(args[i + 1].clone());
                video_labels.push(args[i + 1].clone());
                i += 2;
            }
            "--can-path" if i + 1 < args.len() => {
                can_paths.push(args[i + 1].clone());
                i += 2;
            }
            "--prefix" if i + 1 < args.len() => {
                prefix = args[i + 1].clone();
                i += 2;
            }
            "--output-dir" if i + 1 < args.len() => {
                output_dir = args[i + 1].clone();
                i += 2;
            }
            "--no-video" => {
                record_video = false;
                i += 1;
            }
            "--no-depth" => {
                record_depth = false;
                i += 1;
            }
            "--duration" if i + 1 < args.len() => {
                duration_secs = args[i + 1].parse().ok();
                i += 2;
            }
            "--insecure" => {
                insecure = true;
                i += 1;
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

    // Load config file if provided (CLI flags override config values)
    if let Some(cfg_path) = config_path {
        match parse_config_file(&cfg_path) {
            Ok(cfg) => {
                // Relay: config provides default, --relay overrides
                if let Some(cfg_relay) = cfg.relay {
                    // Only use config relay if --relay wasn't explicitly given
                    let cli_had_relay = args.windows(2).any(|w| w[0] == "--relay");
                    if !cli_had_relay {
                        relay = cfg_relay;
                    }
                }
                // Video paths: CLI --video-path overrides; if none given, use config
                if video_paths.is_empty() {
                    for rs in &cfg.realsense {
                        video_paths.push(rs.path.clone());
                        video_labels.push(rs.label.clone());
                    }
                }
                // CAN paths: CLI --can-path overrides; if none given, use config
                if can_paths.is_empty() {
                    can_paths = cfg.can_paths;
                }
                tracing::info!(
                    "Loaded config: {} realsense, {} CAN paths",
                    video_paths.len(),
                    can_paths.len()
                );
            }
            Err(e) => {
                eprintln!("Error loading config {}: {}", cfg_path, e);
                std::process::exit(1);
            }
        }
    }

    // Backwards compat: if no video paths at all, use default
    if video_paths.is_empty() {
        video_paths.push("anon/realsense".to_string());
        video_labels.push("realsense".to_string());
    }

    Args {
        relay,
        video_paths,
        video_labels,
        can_paths,
        prefix,
        output_dir,
        record_video,
        record_depth,
        duration_secs,
        insecure,
    }
}

fn print_usage() {
    println!("MoQ Data Recorder — records video + depth + CAN to a single multi-track fMP4");
    println!();
    println!("Usage: recorder [options]");
    println!();
    println!("Options:");
    println!("  --config <file>         Load config JSON (OpenArm v3 format)");
    println!("  --relay <url>           MoQ relay URL (default: https://cdn.1ms.ai)");
    println!("  --video-path <path>     Video broadcast path (repeatable, overrides config)");
    println!("  --can-path <path>       CAN broadcast path (repeatable, overrides config)");
    println!("  --prefix <name>         Output file prefix (default: recording_YYYYMMDD_HHMMSS)");
    println!("  --output-dir <dir>      Output directory (default: .)");
    println!("  --no-video              Skip video tracks");
    println!("  --no-depth              Skip depth tracks");
    println!("  --duration <seconds>    Stop after N seconds (default: Ctrl+C)");
    println!("  --insecure              Disable TLS verification");
    println!();
    println!("Examples:");
    println!("  recorder --config config.json --duration 60");
    println!("  recorder --video-path anon/realsense --can-path anon/xoq-can-can0/state");
    println!("  recorder --relay https://172.18.133.111:4443 --insecure --prefix teleop_run1");
}

// ---------------------------------------------------------------------------
// CAN buffering
// ---------------------------------------------------------------------------

struct TimestampedCanBatch {
    timestamp_ms: u64,
    data: Vec<u8>,
}

type CanBuffer = Arc<Mutex<Vec<TimestampedCanBatch>>>;

async fn can_reader_task(mut reader: MoqTrackReader, buffer: CanBuffer, cancel: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = reader.read() => {
                match result {
                    Ok(Some(data)) => {
                        let ts = now_ms();
                        let mut buf = buffer.lock().unwrap();
                        buf.push(TimestampedCanBatch {
                            timestamp_ms: ts,
                            data: data.to_vec(),
                        });
                    }
                    Ok(None) => {
                        tracing::info!("CAN track ended");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("CAN read error: {}", e);
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Realsense source
// ---------------------------------------------------------------------------

struct RealsenseSource {
    label: String,
    video_reader: Option<MoqTrackReader>,
    depth_reader: Option<MoqTrackReader>,
    video_track_id: u32,
    depth_track_id: u32,
}

// ---------------------------------------------------------------------------
// fMP4 helpers
// ---------------------------------------------------------------------------

/// Strip the 8-byte LE timestamp prefix that realsense-server prepends to each MoQ frame.
fn strip_timestamp(data: &[u8]) -> &[u8] {
    if data.len() > 8 {
        &data[8..]
    } else {
        data
    }
}

/// Wait for an init segment from a video/depth track.
async fn wait_for_init(
    reader: &mut MoqTrackReader,
    name: &str,
) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    tracing::info!("Waiting for {} init segment...", name);
    loop {
        match reader.read().await? {
            Some(data) => {
                let raw = strip_timestamp(&data);
                if raw.len() > 8 && &raw[4..8] == b"ftyp" {
                    let mut offset = 0;
                    let mut found_moov_end = false;
                    while offset + 8 <= raw.len() {
                        let box_size = u32::from_be_bytes([
                            raw[offset],
                            raw[offset + 1],
                            raw[offset + 2],
                            raw[offset + 3],
                        ]) as usize;
                        let box_type = &raw[offset + 4..offset + 8];
                        if box_size < 8 {
                            break;
                        }
                        let next = offset + box_size;
                        if box_type == b"moov" {
                            found_moov_end = true;
                            offset = next;
                            break;
                        }
                        offset = next;
                    }

                    if found_moov_end {
                        let init = raw[..offset].to_vec();
                        let media = if offset < raw.len() {
                            Some(raw[offset..].to_vec())
                        } else {
                            None
                        };
                        tracing::info!(
                            "Got {} init segment: {} bytes (+ {} bytes media)",
                            name,
                            init.len(),
                            media.as_ref().map(|m| m.len()).unwrap_or(0)
                        );
                        return Ok((init, media));
                    }
                }
                tracing::debug!("Skipping non-init {} frame ({} bytes)", name, raw.len());
            }
            None => anyhow::bail!("{} track ended before init segment", name),
        }
    }
}

/// Skip past init boxes (ftyp+moov) in a combined init+media frame, returning the media portion.
fn skip_init_boxes(raw: &[u8]) -> Option<&[u8]> {
    let mut offset = 0;
    while offset + 8 <= raw.len() {
        let s = u32::from_be_bytes([
            raw[offset],
            raw[offset + 1],
            raw[offset + 2],
            raw[offset + 3],
        ]) as usize;
        if s < 8 {
            break;
        }
        if &raw[offset + 4..offset + 8] == b"moov" {
            offset += s;
            break;
        }
        offset += s;
    }
    if offset < raw.len() {
        Some(&raw[offset..])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("recorder=info".parse()?)
                .add_directive("warn".parse()?),
        )
        .init();

    let args = parse_args();
    let cancel = CancellationToken::new();

    // Ctrl+C handler
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Ctrl+C received, stopping...");
        cancel_clone.cancel();
    });

    // Duration timer
    if let Some(secs) = args.duration_secs {
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            tracing::info!("Duration reached ({}s), stopping...", secs);
            cancel_clone.cancel();
        });
    }

    println!();
    println!("========================================");
    println!("MoQ Recorder");
    println!("========================================");
    println!("Relay:      {}", args.relay);
    for (i, path) in args.video_paths.iter().enumerate() {
        println!("Video [{}]:  {} ({})", i, path, args.video_labels[i]);
    }
    if !args.can_paths.is_empty() {
        for p in &args.can_paths {
            println!("CAN path:   {}", p);
        }
    }
    println!(
        "Tracks:     {}{}{}",
        if args.record_video { "video " } else { "" },
        if args.record_depth { "depth " } else { "" },
        if !args.can_paths.is_empty() {
            "can"
        } else {
            ""
        },
    );
    let output_path = format!("{}/{}.mp4", args.output_dir, args.prefix);
    println!("Output:     {}", output_path);
    if let Some(d) = args.duration_secs {
        println!("Duration:   {}s", d);
    } else {
        println!("Duration:   until Ctrl+C");
    }
    println!("========================================");
    println!();

    // Connect builder
    let mut builder = MoqBuilder::new().relay(&args.relay);
    if args.insecure {
        builder = builder.disable_tls_verify();
    }

    // -----------------------------------------------------------------------
    // Subscribe to all realsense cameras
    // -----------------------------------------------------------------------
    let mut sources: Vec<RealsenseSource> = Vec::new();

    if args.record_video || args.record_depth {
        for (i, video_path) in args.video_paths.iter().enumerate() {
            let label = &args.video_labels[i];
            tracing::info!(
                "Connecting to realsense broadcast [{}]: {}",
                label,
                video_path
            );
            let mut sub = builder
                .clone()
                .path(video_path)
                .connect_subscriber()
                .await?;

            let mut video_reader = None;
            let mut depth_reader = None;

            if args.record_video {
                match sub.subscribe_track("video").await? {
                    Some(r) => {
                        tracing::info!("[{}] Subscribed to video track", label);
                        video_reader = Some(r);
                    }
                    None => tracing::warn!("[{}] Video track not found", label),
                }
            }

            if args.record_depth {
                match sub.subscribe_track("depth").await? {
                    Some(r) => {
                        tracing::info!("[{}] Subscribed to depth track", label);
                        depth_reader = Some(r);
                    }
                    None => tracing::warn!("[{}] Depth track not found", label),
                }
            }

            // Keep subscriber alive
            std::mem::forget(sub);

            sources.push(RealsenseSource {
                label: label.clone(),
                video_reader,
                depth_reader,
                video_track_id: 0, // assigned after init
                depth_track_id: 0,
            });
        }
    }

    // -----------------------------------------------------------------------
    // Subscribe to CAN broadcasts
    // -----------------------------------------------------------------------
    let can_buffer: CanBuffer = Arc::new(Mutex::new(Vec::new()));
    let mut can_tasks = Vec::new();

    for can_path in &args.can_paths {
        tracing::info!("Connecting to CAN broadcast: {}", can_path);
        let mut can_sub = builder.clone().path(can_path).connect_subscriber().await?;
        match can_sub.subscribe_track("can").await? {
            Some(reader) => {
                tracing::info!("Subscribed to CAN track at {}", can_path);
                let buf = Arc::clone(&can_buffer);
                let token = cancel.clone();
                can_tasks.push(tokio::spawn(async move {
                    can_reader_task(reader, buf, token).await;
                }));
                std::mem::forget(can_sub);
            }
            None => tracing::warn!("CAN track not found at {}", can_path),
        }
    }

    // -----------------------------------------------------------------------
    // Wait for init segments from all sources
    // -----------------------------------------------------------------------
    let mut track_configs: Vec<TrackConfig> = Vec::new();
    let mut next_track_id = 1u32;

    // Per-source first media data that arrived with the init segment
    let mut first_media: Vec<(usize, u32, Vec<u8>)> = Vec::new(); // (source_idx, track_id, data)

    for (src_idx, source) in sources.iter_mut().enumerate() {
        if let Some(ref mut vr) = source.video_reader {
            let name = format!("video[{}]", source.label);
            let (init_data, media) = wait_for_init(vr, &name).await?;
            let parsed = parse_cmaf_init(&init_data)?;
            source.video_track_id = next_track_id;
            track_configs.push(TrackConfig {
                track_id: next_track_id,
                timescale: parsed.timescale,
                handler: *b"vide",
                codec_config: parsed.av1c_config,
                width: parsed.width,
                height: parsed.height,
                high_bitdepth: false,
            });
            tracing::info!(
                "[{}] Video track {}: {}x{}, timescale={}",
                source.label,
                next_track_id,
                parsed.width,
                parsed.height,
                parsed.timescale
            );
            if let Some(m) = media {
                first_media.push((src_idx, next_track_id, m));
            }
            next_track_id += 1;
        }

        if let Some(ref mut dr) = source.depth_reader {
            let name = format!("depth[{}]", source.label);
            let (init_data, media) = wait_for_init(dr, &name).await?;
            let parsed = parse_cmaf_init(&init_data)?;
            source.depth_track_id = next_track_id;
            track_configs.push(TrackConfig {
                track_id: next_track_id,
                timescale: parsed.timescale,
                handler: *b"vide",
                codec_config: parsed.av1c_config,
                width: parsed.width,
                height: parsed.height,
                high_bitdepth: true,
            });
            tracing::info!(
                "[{}] Depth track {}: {}x{}, timescale={}",
                source.label,
                next_track_id,
                parsed.width,
                parsed.height,
                parsed.timescale
            );
            if let Some(m) = media {
                first_media.push((src_idx, next_track_id, m));
            }
            next_track_id += 1;
        }
    }

    // CAN track (single merged track for all CAN sources)
    let can_track_id;
    if !args.can_paths.is_empty() {
        can_track_id = next_track_id;
        track_configs.push(TrackConfig {
            track_id: can_track_id,
            timescale: 1000,
            handler: *b"meta",
            codec_config: Vec::new(),
            width: 0,
            height: 0,
            high_bitdepth: false,
        });
        tracing::info!("CAN track {}: metadata, timescale=1000", can_track_id);
    } else {
        can_track_id = 0;
    }

    if track_configs.is_empty() {
        anyhow::bail!("No tracks configured — nothing to record");
    }

    // -----------------------------------------------------------------------
    // Create recorder and write init segment
    // -----------------------------------------------------------------------
    let mut recorder = MultiTrackRecorder::new(track_configs);
    let mut file = std::fs::File::create(&output_path)?;
    let init_segment = recorder.write_init_segment();
    file.write_all(&init_segment)?;
    tracing::info!("Wrote init segment ({} bytes)", init_segment.len());

    let recording_start_ms = now_ms();
    let mut fragment_count = 0u32;
    let mut total_bytes = init_segment.len();

    // Helpers
    let flush_can = |can_buffer: &CanBuffer,
                     can_track_id: u32,
                     recording_start_ms: u64|
     -> Option<TrackFragment> {
        let mut buf = can_buffer.lock().unwrap();
        if buf.is_empty() || can_track_id == 0 {
            return None;
        }
        let batches: Vec<TimestampedCanBatch> = buf.drain(..).collect();
        drop(buf);

        let base_time_ms = batches[0].timestamp_ms.saturating_sub(recording_start_ms);
        let mut samples = Vec::new();
        let mut data = Vec::new();
        let mut prev_ts = base_time_ms;

        for batch in &batches {
            let ts = batch.timestamp_ms.saturating_sub(recording_start_ms);
            let duration = if ts > prev_ts {
                (ts - prev_ts) as u32
            } else {
                1
            };
            prev_ts = ts;
            samples.push(SampleEntry {
                duration,
                size: batch.data.len() as u32,
                flags: 0x02000000,
                composition_offset: 0,
            });
            data.extend_from_slice(&batch.data);
        }

        Some(TrackFragment {
            track_id: can_track_id,
            base_decode_time: base_time_ms,
            samples,
            data,
        })
    };

    let process_media = |raw: &[u8], track_id: u32| -> Result<TrackFragment> {
        let parsed = parse_cmaf_media_segment(raw)?;
        Ok(TrackFragment {
            track_id,
            base_decode_time: parsed.base_decode_time,
            samples: parsed.samples,
            data: parsed.mdat_payload,
        })
    };

    // Process first media segments that arrived with init
    for (_src_idx, track_id, media_data) in first_media {
        let mut frags = Vec::new();
        frags.push(process_media(&media_data, track_id)?);
        if let Some(can_frag) = flush_can(&can_buffer, can_track_id, recording_start_ms) {
            frags.push(can_frag);
        }
        let frag_data = recorder.write_fragment(&frags);
        file.write_all(&frag_data)?;
        total_bytes += frag_data.len();
        fragment_count += 1;
    }

    // -----------------------------------------------------------------------
    // Main recording loop — select across all readers
    // -----------------------------------------------------------------------
    tracing::info!("Recording...");

    loop {
        let active_readers: usize = sources
            .iter()
            .map(|s| {
                (if s.video_reader.is_some() { 1 } else { 0 })
                    + (if s.depth_reader.is_some() { 1 } else { 0 })
            })
            .sum();
        if active_readers == 0 && can_tasks.is_empty() {
            tracing::info!("All tracks ended");
            break;
        }

        // We use tokio::select! over a dynamically-built set of futures.
        // To handle N sources, we use a FuturesUnordered-like approach with indexed channels.
        // For simplicity, we build futures for each active reader and select the first one.

        // We'll use a macro-free approach: spawn each read into a oneshot and select on them.
        // Actually, let's use a simpler approach: poll each source in order with biased select.
        // With a small number of cameras (2-4), this is fine.

        // Build a vec of boxed futures that return (source_idx, "video"|"depth", Result<Option<bytes>>)
        enum ReadResult {
            Cancelled,
            Video(usize, Result<Option<bytes::Bytes>, anyhow::Error>),
            Depth(usize, Result<Option<bytes::Bytes>, anyhow::Error>),
        }

        let cancel_fut = cancel.cancelled();
        tokio::pin!(cancel_fut);

        use std::task::Poll;

        let result: ReadResult = tokio::select! {
            biased;
            _ = &mut cancel_fut => ReadResult::Cancelled,
            res = async {
                // Create a future that resolves when ANY reader has data
                std::future::poll_fn(|cx| {
                    for (idx, source) in sources.iter_mut().enumerate() {
                        if let Some(ref mut vr) = source.video_reader {
                            let fut = vr.read();
                            tokio::pin!(fut);
                            if let Poll::Ready(result) = fut.poll(cx) {
                                return Poll::Ready(ReadResult::Video(idx, result));
                            }
                        }
                        if let Some(ref mut dr) = source.depth_reader {
                            let fut = dr.read();
                            tokio::pin!(fut);
                            if let Poll::Ready(result) = fut.poll(cx) {
                                return Poll::Ready(ReadResult::Depth(idx, result));
                            }
                        }
                    }
                    Poll::Pending
                }).await
            } => res,
        };

        match result {
            ReadResult::Cancelled => {
                tracing::info!("Stopping recording...");
                break;
            }
            ReadResult::Video(idx, Ok(Some(data))) => {
                let track_id = sources[idx].video_track_id;
                let raw = strip_timestamp(&data);
                let media = if raw.len() > 8 && &raw[4..8] == b"ftyp" {
                    skip_init_boxes(raw)
                } else if raw.len() > 8 {
                    Some(raw)
                } else {
                    None
                };
                if let Some(media) = media {
                    if let Ok(frag) = process_media(media, track_id) {
                        let mut frags = vec![frag];
                        if let Some(can_frag) =
                            flush_can(&can_buffer, can_track_id, recording_start_ms)
                        {
                            frags.push(can_frag);
                        }
                        let frag_data = recorder.write_fragment(&frags);
                        file.write_all(&frag_data)?;
                        total_bytes += frag_data.len();
                        fragment_count += 1;
                    }
                }
            }
            ReadResult::Video(idx, Ok(None)) => {
                tracing::info!("[{}] Video track ended", sources[idx].label);
                sources[idx].video_reader = None;
            }
            ReadResult::Video(idx, Err(e)) => {
                tracing::warn!("[{}] Video read error: {}", sources[idx].label, e);
                sources[idx].video_reader = None;
            }
            ReadResult::Depth(idx, Ok(Some(data))) => {
                let track_id = sources[idx].depth_track_id;
                let raw = strip_timestamp(&data);
                let media = if raw.len() > 8 && &raw[4..8] == b"ftyp" {
                    skip_init_boxes(raw)
                } else if raw.len() > 8 {
                    Some(raw)
                } else {
                    None
                };
                if let Some(media) = media {
                    if let Ok(frag) = process_media(media, track_id) {
                        let mut frags = vec![frag];
                        if let Some(can_frag) =
                            flush_can(&can_buffer, can_track_id, recording_start_ms)
                        {
                            frags.push(can_frag);
                        }
                        let frag_data = recorder.write_fragment(&frags);
                        file.write_all(&frag_data)?;
                        total_bytes += frag_data.len();
                        fragment_count += 1;
                    }
                }
            }
            ReadResult::Depth(idx, Ok(None)) => {
                tracing::info!("[{}] Depth track ended", sources[idx].label);
                sources[idx].depth_reader = None;
            }
            ReadResult::Depth(idx, Err(e)) => {
                tracing::warn!("[{}] Depth read error: {}", sources[idx].label, e);
                sources[idx].depth_reader = None;
            }
        }

        // Progress logging every 10 fragments
        if fragment_count.is_multiple_of(10) && fragment_count > 0 {
            let elapsed = (now_ms() - recording_start_ms) / 1000;
            tracing::info!(
                "Recorded {} fragments, {} bytes, {}s elapsed",
                fragment_count,
                total_bytes,
                elapsed
            );
        }
    }

    // Flush remaining CAN data
    if let Some(can_frag) = flush_can(&can_buffer, can_track_id, recording_start_ms) {
        let frag_data = recorder.write_fragment(&[can_frag]);
        file.write_all(&frag_data)?;
        total_bytes += frag_data.len();
        fragment_count += 1;
    }

    file.flush()?;

    let elapsed = (now_ms() - recording_start_ms) / 1000;
    println!();
    println!("========================================");
    println!("Recording complete");
    println!("========================================");
    println!("File:       {}", output_path);
    println!("Fragments:  {}", fragment_count);
    println!(
        "Size:       {} bytes ({:.1} MB)",
        total_bytes,
        total_bytes as f64 / 1_048_576.0
    );
    println!("Duration:   {}s", elapsed);
    println!("========================================");

    Ok(())
}
