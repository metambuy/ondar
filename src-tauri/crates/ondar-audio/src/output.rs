//! The output owner's seam (0.1.2, F1). The engine opens its output through an
//! [`OutputOpener`], so the session tests can stand a bare `rodio::mixer::mixer` and a puller
//! thread in for the device, and count the opens; production opens the default device.
//!
//! What the engine keeps of an open is the [`Mixer`] (each attach connects its own `Player` to
//! it, `engine.rs::run_session`), the [`OutputFormat`] every session converts to, and a guard
//! whose drop closes the device. Nothing here is on the audio callback's path.

use std::sync::mpsc::Sender;

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

/// How an open output tells the engine it is gone: the open's generation, on a channel the
/// engine drains every wake (`Engine::check_output_lost`). The generation lets the engine ignore
/// a late signal from an output it has already replaced. Sending never blocks (an unbounded
/// `mpsc`), so it is safe from cpal's error-callback thread.
#[derive(Clone)]
pub(crate) struct LossSignal {
    tx: Sender<u64>,
    generation: u64,
}

impl LossSignal {
    pub(crate) fn new(tx: Sender<u64>, generation: u64) -> Self {
        Self { tx, generation }
    }

    pub(crate) fn fire(&self) {
        // A send error means the engine is gone; there is nothing to tell.
        let _ = self.tx.send(self.generation);
    }
}

/// How the engine opens its output. `lost` is fired when the output reports that its device is
/// gone.
pub(crate) trait OutputOpener: Send {
    fn open(&mut self, lost: LossSignal) -> Result<Opened, String>;
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
///
/// The loss: on rodio 0.23.0 / cpal 0.18.2 a stream on the default device follows a new default
/// by itself (`DeviceChanged`, measured 5 of 5 Bluetooth losses and 10 of 10 switches,
/// 2026-10-30); `DeviceNotAvailable` means no output was left to follow (with no output device
/// at all, cpal pauses the stream, the F1 shape), so it fires `lost` (decision 4).
pub(crate) struct DeviceOpener;

impl OutputOpener for DeviceOpener {
    fn open(&mut self, lost: LossSignal) -> Result<Opened, String> {
        let mut sink = DeviceSinkBuilder::from_default_device()
            .map_err(|e| e.to_string())?
            .with_buffer_size(rodio::cpal::BufferSize::Default)
            .with_error_callback(move |e| {
                log_output_error(&e);
                if e.kind() == rodio::CpalErrorKind::DeviceNotAvailable {
                    lost.fire();
                }
            })
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
