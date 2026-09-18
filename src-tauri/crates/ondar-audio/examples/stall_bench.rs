//! Headless bench for stall/reconnect testing (`scripts/stall-server.py`, README "Stall
//! testing"). Drives the real `AudioEngine` — the same code path the popover's dev transport
//! (`src/panel/Transport.tsx`) uses — without a webview or a human clicking Play, so runs are
//! scriptable and give
//! exact timestamps to correlate against the server's own log.
//!
//! Usage: `cargo run -p ondar-audio --example stall_bench -- <url> [duration_secs]`
//!
//! Env overrides read by the engine itself: `ONDAR_READ_TIMEOUT_SECS`,
//! `ONDAR_RETRY_TIMEOUT_SECS`, `ONDAR_PREFETCH_BYTES`. Set `RUST_LOG` (e.g.
//! `stream_download=debug,ondar_audio=debug`) to see the underlying `stream-download`/engine
//! logs alongside this binary's own event trace.

use std::env;
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use ondar_audio::{AudioCommand, AudioEngine, EngineEvent};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ondar_audio=debug")),
        )
        .init();

    let mut args = env::args().skip(1);
    let Some(url) = args.next() else {
        eprintln!("usage: stall_bench <url> [duration_secs]");
        std::process::exit(2);
    };
    let duration_secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    let (engine, events) = AudioEngine::start("Ondar/stall-bench".to_string());
    let start = Instant::now();
    println!("[{:7.3}s] play url={url}", 0.0);
    engine.send(AudioCommand::Play {
        url,
        station_id: "stall-bench".to_string(),
    });

    let deadline = start + Duration::from_secs(duration_secs);
    let mut reconnects = 0u64;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match events.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(ev) => {
                let t = start.elapsed().as_secs_f64();
                match &ev {
                    EngineEvent::State(s) => println!("[{t:7.3}s] state    {s:?}"),
                    EngineEvent::StreamInfo(i) => println!("[{t:7.3}s] stream   {i:?}"),
                    EngineEvent::Metadata(m) => println!("[{t:7.3}s] metadata {m:?}"),
                    EngineEvent::Reconnect(r) => {
                        reconnects = r.count;
                        println!("[{t:7.3}s] reconnect count={}", r.count);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    let final_state = engine.state();
    println!(
        "--- summary: elapsed={:.1}s final_state={:?} internal_reconnects={} ---",
        start.elapsed().as_secs_f64(),
        final_state,
        reconnects
    );

    // Tripwire. `PREFETCH_BYTES` has to cover at least one decoder read or the decode thread
    // starves on its first refill — measured at 16 KB as an audible dropout 2 s into playback
    // with no network fault. The read size is chosen by rodio/symphonia, not by Ondar, so a
    // dependency bump can break this silently. Checked here because this is where the evidence
    // was found, and it costs nothing.
    let max_read = ondar_audio::icy::MAX_OBSERVED_READ.load(Ordering::Relaxed) as u64;
    let prefetch = ondar_audio::stream::prefetch_bytes();
    if max_read > prefetch {
        println!(
            "!!! PREFETCH TOO SMALL: decoder asked for {max_read} B, prefetch is {prefetch} B. \
             Expect spontaneous underruns ~2 s into playback on a healthy network. The read \
             size is rodio/symphonia's, not ours — re-derive PREFETCH_BYTES (stream.rs) after \
             a dependency bump."
        );
    } else {
        println!("--- max decoder read {max_read} B vs prefetch {prefetch} B: ok ---");
    }
    engine.send(AudioCommand::Stop);
    std::thread::sleep(Duration::from_millis(200));
}
