//! The output owner's seam (0.1.2, F1). The engine opens its output through an
//! [`OutputOpener`], so the session tests can stand a bare `rodio::mixer::mixer` and a puller
//! thread in for the device, and count the opens; production opens the default device.
//!
//! What the engine keeps of an open is the [`Mixer`] (each attach connects its own `Player` to
//! it, `engine.rs::run_session`), the [`OutputFormat`] every session converts to, and a guard
//! whose drop closes the device. Nothing here is on the audio callback's path.

use rodio::DeviceSinkBuilder;
use rodio::mixer::Mixer;

use crate::engine::{OutputFormat, log_output_error};

/// One open output.
pub(crate) struct Opened {
    pub(crate) mixer: Mixer,
    pub(crate) format: OutputFormat,
    /// Keeps the output open; dropping it closes the device (production: the
    /// `MixerDeviceSink`, whose drop disposes the `AudioUnit`, 74-289 ms on a live device,
    /// measured 2026-10-30).
    pub(crate) guard: Box<dyn Send>,
}

/// How the engine opens its output.
pub(crate) trait OutputOpener: Send {
    fn open(&mut self) -> Result<Opened, String>;
}

/// The default output device.
///
/// The device's own buffer (`BufferSize::Default`), not rodio's: rodio 0.23.0 asks for a `Fixed`
/// ~50 ms (2048 frames at 44.1 kHz, `stream.rs:207-217`), which cpal 0.18.2 refuses on a device
/// whose range is lower (`14..=1024` on an Audio Pro A15 W, measured 2026-10-30), so
/// `open_default_sink` failed on every device. `Default` gave 512 frames on both devices measured
/// (11.6 ms at 44.1 kHz, 10.7 ms at 48 kHz). No fallback to another device: an open on an output
/// nobody hears is worse than an honest `Error { code: Device }` (rodio PR #914, which clamps the
/// buffer, is unreleased; re-check at each rodio release). ONDAR.md, "0.1.2: decisions", 1.
pub(crate) struct DeviceOpener;

impl OutputOpener for DeviceOpener {
    fn open(&mut self) -> Result<Opened, String> {
        let mut sink = DeviceSinkBuilder::from_default_device()
            .map_err(|e| e.to_string())?
            .with_buffer_size(rodio::cpal::BufferSize::Default)
            .with_error_callback(|e| log_output_error(&e))
            .open_stream()
            .map_err(|e| e.to_string())?;
        sink.log_on_drop(false);
        let format = OutputFormat {
            channels: sink.config().channel_count(),
            sample_rate: sink.config().sample_rate(),
        };
        Ok(Opened {
            mixer: sink.mixer().clone(),
            format,
            guard: Box::new(sink),
        })
    }
}
