//! Audio client - connects to a remote audio server for bidirectional streaming.
//!
//! Usage:
//!   audio_client <server-id>                # Duplex mode (default)
//!   audio_client <server-id> --record out.raw  # Record remote mic to file
//!   audio_client <server-id> --play in.raw     # Play file to remote speaker
//!   audio_client --moq anon/my-audio           # MoQ relay

use anyhow::Result;
use std::io::Write;
use xoq::sounddevice_impl::AudioStreamBuilder;

fn print_usage() {
    println!("Usage: audio_client <server-id> [options]");
    println!();
    println!("Options:");
    println!("  --record <file>     Record remote mic to raw PCM file");
    println!("  --play <file>       Play raw PCM file to remote speaker");
    println!("  --duplex            Full duplex (record + play local mic, default)");
    println!("  --moq <path>        Use MoQ relay transport");
    println!("  --sample-rate <hz>  Sample rate (default: 48000)");
    println!("  --channels <n>      Number of channels (default: 1)");
    println!("  --frames <n>        Number of frames to record (default: unlimited)");
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("xoq=info".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }

    let mut source = String::new();
    let mut record_file: Option<String> = None;
    let mut play_file: Option<String> = None;
    let mut sample_rate = 48000u32;
    let mut channels = 1u16;
    let mut max_frames: Option<usize> = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--record" if i + 1 < args.len() => {
                record_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--play" if i + 1 < args.len() => {
                play_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--moq" if i + 1 < args.len() => {
                source = args[i + 1].clone();
                i += 2;
            }
            "--sample-rate" if i + 1 < args.len() => {
                sample_rate = args[i + 1].parse()?;
                i += 2;
            }
            "--channels" if i + 1 < args.len() => {
                channels = args[i + 1].parse()?;
                i += 2;
            }
            "--frames" if i + 1 < args.len() => {
                max_frames = Some(args[i + 1].parse()?);
                i += 2;
            }
            "--duplex" => {
                i += 1;
            }
            _ => {
                if source.is_empty() && !args[i].starts_with("--") {
                    source = args[i].clone();
                }
                i += 1;
            }
        }
    }

    if source.is_empty() {
        print_usage();
        return Ok(());
    }

    println!("Connecting to: {}", source);

    let mut stream = AudioStreamBuilder::new(&source)
        .sample_rate(sample_rate)
        .channels(channels)
        .open()?;

    println!("Connected! Config: {}Hz, {}ch", sample_rate, channels);

    if let Some(ref path) = record_file {
        // Record mode: read from remote mic and save to file
        println!("Recording to: {}", path);
        let mut file = std::fs::File::create(path)?;
        let mut total_frames = 0usize;

        loop {
            let frame = stream.read_chunk()?;
            file.write_all(&frame.data)?;
            total_frames += frame.frame_count as usize;

            if total_frames % (sample_rate as usize) == 0 {
                println!("Recorded {:.1}s", total_frames as f64 / sample_rate as f64);
            }

            if let Some(max) = max_frames {
                if total_frames >= max {
                    break;
                }
            }
        }

        println!("Done. Total frames: {}", total_frames);
    } else if let Some(ref path) = play_file {
        // Play mode: read file and send to remote speaker
        println!("Playing: {}", path);
        let data = std::fs::read(path)?;

        // Assume I16 mono — convert to f32
        let samples: Vec<f32> = data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();

        stream.play(&samples)?;
        println!("Done.");
    } else {
        // Duplex mode: print frames as they arrive
        println!("Duplex mode — reading from remote mic (Ctrl+C to stop)");
        let mut total_frames = 0usize;

        loop {
            let frame = stream.read_chunk()?;
            total_frames += frame.frame_count as usize;

            if total_frames % (sample_rate as usize) < frame.frame_count as usize {
                println!(
                    "Audio: {}Hz {}ch, {:.1}s received, chunk={} frames",
                    frame.config.sample_rate,
                    frame.config.channels,
                    total_frames as f64 / sample_rate as f64,
                    frame.frame_count,
                );
            }
        }
    }

    Ok(())
}
