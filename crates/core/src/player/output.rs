//! Audio outputs.
//!
//! The player only needs five verbs from an output: play a buffer from an
//! offset, pause, resume, stop, and "where are you". Keeping the PCM on our
//! side (instead of handing text to an OS speech API and hoping) is what makes
//! the transport behave identically everywhere.

use crate::engine::Audio;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

pub trait AudioOutput {
    /// Replace whatever is playing with `audio`, starting at `start_sample`.
    fn play(&mut self, audio: Arc<Audio>, start_sample: usize) -> Result<()>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    /// Position within the current buffer, in samples.
    fn position(&self) -> usize;
    /// True once the current buffer has been played to its end (and also when
    /// nothing was ever played).
    fn finished(&self) -> bool;
    /// Called after a while of inactivity; release the audio device.
    fn release(&mut self) {}
}

/// An output that plays to nowhere but keeps real time. Used in tests and as
/// a fallback when no audio device can be opened, so the UI stays functional.
pub struct SilentOutput {
    /// How much faster than real time the clock runs (tests use > 1).
    speed: f64,
    current: Option<Arc<Audio>>,
    /// Samples played before `since`.
    base: usize,
    since: Option<Instant>,
}

impl SilentOutput {
    pub fn new() -> Self {
        Self::with_speed(1.0)
    }

    pub fn with_speed(speed: f64) -> Self {
        Self {
            speed,
            current: None,
            base: 0,
            since: None,
        }
    }
}

impl Default for SilentOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput for SilentOutput {
    fn play(&mut self, audio: Arc<Audio>, start_sample: usize) -> Result<()> {
        self.base = start_sample.min(audio.samples.len());
        self.current = Some(audio);
        self.since = Some(Instant::now());
        Ok(())
    }

    fn pause(&mut self) {
        self.base = self.position();
        self.since = None;
    }

    fn resume(&mut self) {
        if self.current.is_some() && self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    fn stop(&mut self) {
        self.current = None;
        self.since = None;
        self.base = 0;
    }

    fn position(&self) -> usize {
        let Some(audio) = &self.current else {
            return 0;
        };
        let elapsed = self
            .since
            .map(|t| t.elapsed().as_secs_f64() * self.speed)
            .unwrap_or(0.0);
        (self.base + (elapsed * audio.sample_rate as f64) as usize).min(audio.samples.len())
    }

    fn finished(&self) -> bool {
        match &self.current {
            Some(audio) => self.position() >= audio.samples.len(),
            None => true,
        }
    }
}

#[cfg(feature = "audio")]
pub use device::DeviceOutput;

#[cfg(feature = "audio")]
mod device {
    use super::*;
    use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, SampleRate, Source};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// State shared between the control thread and the audio callback.
    struct Shared {
        cursor: AtomicUsize,
        paused: AtomicBool,
        stopped: AtomicBool,
    }

    /// Plays through the system's default output device.
    pub struct DeviceOutput {
        sink: Option<MixerDeviceSink>,
        current: Option<(Arc<Audio>, Arc<Shared>)>,
    }

    impl DeviceOutput {
        pub fn new() -> Self {
            Self {
                sink: None,
                current: None,
            }
        }

        /// Check that a device can be opened at all.
        pub fn probe() -> Result<()> {
            let mut sink = DeviceSinkBuilder::open_default_sink()
                .map_err(|e| anyhow::anyhow!("no audio output device: {e}"))?;
            sink.log_on_drop(false);
            Ok(())
        }

        fn sink(&mut self) -> Result<&MixerDeviceSink> {
            if self.sink.is_none() {
                let mut sink = DeviceSinkBuilder::open_default_sink()
                    .map_err(|e| anyhow::anyhow!("could not open the audio device: {e}"))?;
                sink.log_on_drop(false);
                self.sink = Some(sink);
            }
            Ok(self.sink.as_ref().unwrap())
        }

        fn halt_current(&mut self) {
            if let Some((_, shared)) = self.current.take() {
                shared.stopped.store(true, Ordering::Release);
            }
        }
    }

    impl Default for DeviceOutput {
        fn default() -> Self {
            Self::new()
        }
    }

    impl AudioOutput for DeviceOutput {
        fn play(&mut self, audio: Arc<Audio>, start_sample: usize) -> Result<()> {
            self.halt_current();
            let shared = Arc::new(Shared {
                cursor: AtomicUsize::new(start_sample.min(audio.samples.len())),
                paused: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            });
            let source = BufferSource {
                audio: audio.clone(),
                shared: shared.clone(),
                sample_rate: SampleRate::new(audio.sample_rate.max(1)).unwrap(),
                fade_in: FADE_SAMPLES,
            };
            self.sink()?.mixer().add(source);
            self.current = Some((audio, shared));
            Ok(())
        }

        fn pause(&mut self) {
            if let Some((_, shared)) = &self.current {
                shared.paused.store(true, Ordering::Release);
            }
        }

        fn resume(&mut self) {
            if let Some((_, shared)) = &self.current {
                shared.paused.store(false, Ordering::Release);
            }
        }

        fn stop(&mut self) {
            self.halt_current();
        }

        fn position(&self) -> usize {
            self.current
                .as_ref()
                .map(|(_, s)| s.cursor.load(Ordering::Acquire))
                .unwrap_or(0)
        }

        fn finished(&self) -> bool {
            match &self.current {
                Some((audio, shared)) => {
                    shared.cursor.load(Ordering::Acquire) >= audio.samples.len()
                }
                None => true,
            }
        }

        fn release(&mut self) {
            self.halt_current();
            self.sink = None;
        }
    }

    /// Samples over which playback fades in, to avoid a click when starting
    /// in the middle of a waveform.
    const FADE_SAMPLES: usize = 96;

    struct BufferSource {
        audio: Arc<Audio>,
        shared: Arc<Shared>,
        sample_rate: SampleRate,
        fade_in: usize,
    }

    impl Iterator for BufferSource {
        type Item = rodio::Sample;

        fn next(&mut self) -> Option<Self::Item> {
            if self.shared.stopped.load(Ordering::Acquire) {
                return None;
            }
            if self.shared.paused.load(Ordering::Acquire) {
                // Keep the source alive in the mixer, emitting silence.
                self.fade_in = FADE_SAMPLES;
                return Some(0.0);
            }
            let i = self.shared.cursor.load(Ordering::Acquire);
            let sample = *self.audio.samples.get(i)?;
            self.shared.cursor.store(i + 1, Ordering::Release);
            let gain = if self.fade_in > 0 {
                self.fade_in -= 1;
                1.0 - self.fade_in as f32 / FADE_SAMPLES as f32
            } else {
                1.0
            };
            Some((sample * gain) as rodio::Sample)
        }
    }

    impl Source for BufferSource {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> ChannelCount {
            ChannelCount::new(1).unwrap()
        }

        fn sample_rate(&self) -> SampleRate {
            self.sample_rate
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }
}
