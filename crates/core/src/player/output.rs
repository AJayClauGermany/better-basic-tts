//! Audio outputs.
//!
//! The player needs only a few verbs from an output: play a buffer from an
//! offset, line up the buffer that follows, pause, resume, stop, and "where
//! are you". Keeping the PCM on our side (instead of handing text to an OS
//! speech API and hoping) is what makes the transport behave identically
//! everywhere.
//!
//! Lining up the next buffer matters for how reading *sounds*: the output
//! moves from one sentence to the next by itself, on the exact sample, so the
//! flow does not depend on how promptly the control thread gets scheduled
//! (operating systems are stingy with timers for background apps).

use crate::engine::Audio;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

pub trait AudioOutput {
    /// Replace whatever is playing (and whatever is lined up) with `audio`,
    /// starting at `start_sample`.
    fn play(&mut self, audio: Arc<Audio>, start_sample: usize) -> Result<()>;
    /// Line up `audio` to start the instant the current buffer ends. Returns
    /// false if this output cannot do that for this buffer (for example
    /// because its sample rate differs); the caller then starts it with
    /// [`AudioOutput::play`] once the current buffer has finished.
    fn queue_next(&mut self, _audio: Arc<Audio>) -> bool {
        false
    }
    /// Remove the lined-up buffer. Returns false if there was none — in
    /// particular when the output has already moved on to it.
    fn clear_next(&mut self) -> bool {
        false
    }
    /// True, once, after the output has moved on to the lined-up buffer.
    /// From then on [`AudioOutput::position`] and [`AudioOutput::finished`]
    /// refer to that buffer.
    fn advanced(&mut self) -> bool {
        false
    }
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    /// Position within the current buffer, in samples.
    fn position(&self) -> usize;
    /// True once the current buffer has been played to its end and nothing
    /// is lined up (and also when nothing was ever played).
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
    next: Option<Arc<Audio>>,
    advanced: bool,
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
            next: None,
            advanced: false,
            base: 0,
            since: None,
        }
    }

    /// Samples played so far, not limited to the buffer's length.
    fn raw_position(&self) -> usize {
        let Some(audio) = &self.current else {
            return 0;
        };
        let elapsed = self
            .since
            .map(|t| t.elapsed().as_secs_f64() * self.speed)
            .unwrap_or(0.0);
        self.base + (elapsed * audio.sample_rate as f64) as usize
    }

    /// Move on to the lined-up buffer if the clock says the current one is
    /// over, carrying the overshoot along like a real device would.
    fn roll(&mut self) {
        let Some(current) = &self.current else {
            return;
        };
        let raw = self.raw_position();
        if raw < current.samples.len() || self.next.is_none() {
            return;
        }
        let over = raw - current.samples.len();
        let next = self.next.take().expect("checked above");
        self.base = over.min(next.samples.len());
        self.since = self.since.map(|_| Instant::now());
        self.current = Some(next);
        self.advanced = true;
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
        self.next = None;
        self.advanced = false;
        self.since = Some(Instant::now());
        Ok(())
    }

    fn queue_next(&mut self, audio: Arc<Audio>) -> bool {
        match &self.current {
            Some(current) if current.sample_rate == audio.sample_rate => {
                self.roll();
                self.next = Some(audio);
                true
            }
            _ => false,
        }
    }

    fn clear_next(&mut self) -> bool {
        self.roll();
        self.next.take().is_some()
    }

    fn advanced(&mut self) -> bool {
        self.roll();
        std::mem::take(&mut self.advanced)
    }

    fn pause(&mut self) {
        self.roll();
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
        self.next = None;
        self.advanced = false;
        self.since = None;
        self.base = 0;
    }

    fn position(&self) -> usize {
        match &self.current {
            Some(audio) => self.raw_position().min(audio.samples.len()),
            None => 0,
        }
    }

    fn finished(&self) -> bool {
        match &self.current {
            Some(audio) => self.next.is_none() && self.raw_position() >= audio.samples.len(),
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
    use std::sync::Mutex;
    use std::time::Duration;

    /// State shared between the control thread and the audio callback.
    struct Shared {
        /// Position in the buffer the callback is reading.
        cursor: AtomicUsize,
        /// Length of that buffer.
        len: AtomicUsize,
        paused: AtomicBool,
        stopped: AtomicBool,
        /// The buffer to continue with. The callback only ever `try_lock`s.
        next: Mutex<Option<Arc<Audio>>>,
        /// How many times the callback has moved on to a lined-up buffer.
        advances: AtomicUsize,
    }

    struct Playing {
        audio: Arc<Audio>,
        shared: Arc<Shared>,
        /// What we lined up, to become `audio` once the callback takes it.
        queued: Option<Arc<Audio>>,
        /// Value of `shared.advances` we have already reported.
        seen_advances: usize,
    }

    /// Plays through the system's default output device.
    pub struct DeviceOutput {
        sink: Option<MixerDeviceSink>,
        current: Option<Playing>,
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
            if let Some(playing) = self.current.take() {
                playing.shared.stopped.store(true, Ordering::Release);
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
                len: AtomicUsize::new(audio.samples.len()),
                paused: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                next: Mutex::new(None),
                advances: AtomicUsize::new(0),
            });
            let source = BufferSource {
                audio: audio.clone(),
                shared: shared.clone(),
                sample_rate: SampleRate::new(audio.sample_rate.max(1)).unwrap(),
                fade_in: FADE_SAMPLES,
            };
            self.sink()?.mixer().add(source);
            self.current = Some(Playing {
                audio,
                shared,
                queued: None,
                seen_advances: 0,
            });
            Ok(())
        }

        fn queue_next(&mut self, audio: Arc<Audio>) -> bool {
            let Some(playing) = &mut self.current else {
                return false;
            };
            // One stream has one sample rate; a voice with another rate is
            // started as a stream of its own.
            if playing.audio.sample_rate != audio.sample_rate || playing.queued.is_some() {
                return false;
            }
            let mut next = playing.shared.next.lock().unwrap();
            // If the callback has already run off the end there is nothing
            // to continue from.
            let cursor = playing.shared.cursor.load(Ordering::Acquire);
            if cursor >= playing.shared.len.load(Ordering::Acquire) {
                return false;
            }
            *next = Some(audio.clone());
            drop(next);
            playing.queued = Some(audio);
            true
        }

        fn clear_next(&mut self) -> bool {
            let Some(playing) = &mut self.current else {
                return false;
            };
            let taken = playing.shared.next.lock().unwrap().take().is_some();
            if taken {
                playing.queued = None;
            }
            taken
        }

        fn advanced(&mut self) -> bool {
            let Some(playing) = &mut self.current else {
                return false;
            };
            let advances = playing.shared.advances.load(Ordering::Acquire);
            if advances == playing.seen_advances {
                return false;
            }
            playing.seen_advances = advances;
            if let Some(audio) = playing.queued.take() {
                playing.audio = audio;
            }
            true
        }

        fn pause(&mut self) {
            if let Some(playing) = &self.current {
                playing.shared.paused.store(true, Ordering::Release);
            }
        }

        fn resume(&mut self) {
            if let Some(playing) = &self.current {
                playing.shared.paused.store(false, Ordering::Release);
            }
        }

        fn stop(&mut self) {
            self.halt_current();
        }

        fn position(&self) -> usize {
            self.current
                .as_ref()
                .map(|p| p.shared.cursor.load(Ordering::Acquire))
                .unwrap_or(0)
        }

        fn finished(&self) -> bool {
            match &self.current {
                Some(playing) => {
                    playing.queued.is_none()
                        && playing.shared.cursor.load(Ordering::Acquire)
                            >= playing.shared.len.load(Ordering::Acquire)
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
            let mut i = self.shared.cursor.load(Ordering::Acquire);
            if i >= self.audio.samples.len() {
                // End of this sentence: carry straight on with the next one
                // if it has been lined up. Never block the audio thread; if
                // the control thread holds the lock right now, play one
                // sample of silence and look again.
                let Ok(mut next) = self.shared.next.try_lock() else {
                    return Some(0.0);
                };
                let audio = next.take()?;
                self.shared
                    .len
                    .store(audio.samples.len(), Ordering::Release);
                self.shared.cursor.store(0, Ordering::Release);
                self.shared.advances.fetch_add(1, Ordering::AcqRel);
                drop(next);
                self.audio = audio;
                i = 0;
            }
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
