//! macOS system voices through `AVSpeechSynthesizer`, in-process.
//!
//! **Experimental.** `/usr/bin/say` (see [`super::say`]) costs about a second
//! per call; synthesising in-process should cost close to nothing. But the
//! API only delivers audio while the process's *main* run loop is being
//! serviced, so this engine is offered only when that is known to be the
//! case (see [`main_loop_is_serviced`]), and it is not the default yet.

use super::{Audio, Engine, SynthRequest, Voice};
use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioCommonFormat, AVAudioPCMBuffer, AVSpeechSynthesisVoice,
    AVSpeechSynthesizer, AVSpeechUtterance,
};
use objc2_foundation::{NSDate, NSRunLoop, NSString, NSThread};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

static MAIN_LOOP_SERVICED: AtomicBool = AtomicBool::new(false);

/// Tell the engine layer whether the main thread is running its run loop
/// (true inside the GUI app, and inside [`run_with_main_loop`]).
pub fn set_main_loop_serviced(serviced: bool) {
    MAIN_LOOP_SERVICED.store(serviced, Ordering::Release);
}

pub fn main_loop_is_serviced() -> bool {
    MAIN_LOOP_SERVICED.load(Ordering::Acquire)
}

/// Run `work` on another thread while this (the main) thread services its
/// run loop, and return what it returns. For command line use, where no UI
/// toolkit is running the loop for us.
pub fn run_with_main_loop<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    if !NSThread::isMainThread_class() {
        return work();
    }
    set_main_loop_serviced(true);
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(work);
        while !worker.is_finished() {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            NSRunLoop::mainRunLoop().runUntilDate(&until);
        }
        worker.join()
    });
    set_main_loop_serviced(false);
    match result {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// One piece of what the synthesizer hands back.
enum Chunk {
    Samples { sample_rate: u32, samples: Vec<f32> },
    Unsupported(String),
    End,
}

struct Rendered {
    audio: Audio,
    /// Time until the first audio arrived.
    first_audio: Duration,
    total: Duration,
    chunks: usize,
    /// For diagnostics: the thread the audio arrived on.
    callback_thread: String,
}

/// How long to wait for the first audio, and for the rest.
const FIRST_AUDIO_TIMEOUT: Duration = Duration::from_secs(12);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// `AVSpeechUtterance.rate` runs from 0 to 1 with 0.5 as the voice's normal
/// speed, and it is not linear in speed. These pairs of (speed multiplier,
/// rate value) were measured with the default system voice by timing the
/// same sentence at each rate value.
const RATE_SCALE: [(f32, f32); 13] = [
    (0.589, 0.10),
    (0.715, 0.20),
    (0.769, 0.30),
    (0.908, 0.40),
    (1.000, 0.50),
    (1.292, 0.55),
    (1.591, 0.60),
    (1.825, 0.65),
    (2.108, 0.70),
    (2.483, 0.75),
    (2.802, 0.80),
    (3.508, 0.90),
    (4.026, 1.00),
];

/// The rate value that makes the voice `multiplier` times its normal speed.
fn utterance_rate(multiplier: f32) -> f32 {
    let m = if multiplier.is_finite() {
        multiplier
    } else {
        1.0
    };
    let (first, last) = (RATE_SCALE[0], RATE_SCALE[RATE_SCALE.len() - 1]);
    if m >= last.0 {
        return last.1;
    }
    // Below the slowest measured point, carry on along the first segment.
    let segment = if m <= first.0 {
        (first, RATE_SCALE[1])
    } else {
        RATE_SCALE
            .windows(2)
            .map(|w| (w[0], w[1]))
            .find(|(_, hi)| m <= hi.0)
            .unwrap_or((RATE_SCALE[RATE_SCALE.len() - 2], last))
    };
    let ((m0, r0), (m1, r1)) = segment;
    (r0 + (m - m0) / (m1 - m0) * (r1 - r0)).clamp(0.0, 1.0)
}

/// Synthesizers kept per thread, at most this many (one per voice in use).
const KEPT_SYNTHESIZERS: usize = 6;

thread_local! {
    /// Warm synthesizers of this synthesis thread, by voice. A synthesizer
    /// that has spoken with a voice answers in milliseconds; a new one, or
    /// one switching voices, needs a few hundred. Keeping one per voice
    /// makes reading with several voices (headings, quotes, ...) as smooth
    /// as reading with one.
    static SYNTHESIZERS: std::cell::RefCell<Vec<(String, Retained<AVSpeechSynthesizer>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn take_synthesizer(voice: &str) -> Option<Retained<AVSpeechSynthesizer>> {
    SYNTHESIZERS.with(|kept| {
        let mut kept = kept.borrow_mut();
        let index = kept.iter().position(|(v, _)| v == voice)?;
        Some(kept.remove(index).1)
    })
}

fn keep_synthesizer(voice: &str, synthesizer: Retained<AVSpeechSynthesizer>) {
    SYNTHESIZERS.with(|kept| {
        let mut kept = kept.borrow_mut();
        // Most recently used last; the oldest makes room.
        if kept.len() >= KEPT_SYNTHESIZERS {
            kept.remove(0);
        }
        kept.push((voice.to_string(), synthesizer));
    });
}

/// Find a voice by its identifier or, failing that, by its name (so a voice
/// chosen for the `say` engine, which goes by names, still applies).
fn find_voice(id_or_name: &str) -> Option<Retained<AVSpeechSynthesisVoice>> {
    // SAFETY: plain Objective-C calls.
    unsafe {
        if let Some(voice) =
            AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(id_or_name))
        {
            return Some(voice);
        }
        AVSpeechSynthesisVoice::speechVoices()
            .iter()
            .find(|v| v.name().to_string().eq_ignore_ascii_case(id_or_name))
    }
}

/// Render `text`. `raw_rate`, if given, is put on the utterance as is
/// (used to calibrate the scale).
fn render(
    text: &str,
    voice: Option<&str>,
    rate: f32,
    raw_rate: Option<f32>,
    pitch: f32,
    volume: f32,
    reuse: bool,
) -> Result<Rendered> {
    let (tx, rx) = mpsc::channel::<(Chunk, String)>();
    let tx = std::sync::Mutex::new(tx);
    let callback = RcBlock::new(move |buffer: NonNull<AVAudioBuffer>| {
        // SAFETY: the system passes a valid buffer that lives for the call;
        // the speech synthesizer documents these as PCM buffers.
        let chunk = unsafe {
            let pcm = &*(buffer.as_ptr() as *const AVAudioPCMBuffer);
            let frames = pcm.frameLength() as usize;
            if frames == 0 {
                Chunk::End
            } else {
                let format = pcm.format();
                let sample_rate = format.sampleRate().round() as u32;
                let common = format.commonFormat();
                if common == AVAudioCommonFormat::PCMFormatFloat32 {
                    let channels = pcm.floatChannelData();
                    if channels.is_null() {
                        Chunk::Unsupported("float32 without channel data".into())
                    } else {
                        let first = (*channels).as_ptr();
                        Chunk::Samples {
                            sample_rate,
                            samples: std::slice::from_raw_parts(first, frames).to_vec(),
                        }
                    }
                } else if common == AVAudioCommonFormat::PCMFormatInt16 {
                    let channels = pcm.int16ChannelData();
                    if channels.is_null() {
                        Chunk::Unsupported("int16 without channel data".into())
                    } else {
                        let first = (*channels).as_ptr();
                        Chunk::Samples {
                            sample_rate,
                            samples: std::slice::from_raw_parts(first, frames)
                                .iter()
                                .map(|&s| s as f32 / 32768.0)
                                .collect(),
                        }
                    }
                } else {
                    Chunk::Unsupported(format!("sample format {}", common.0))
                }
            }
        };
        let thread = format!(
            "{} (main={})",
            std::thread::current().name().unwrap_or("unnamed"),
            NSThread::isMainThread_class()
        );
        let _ = tx.lock().unwrap().send((chunk, thread));
    });

    let started = Instant::now();
    // SAFETY: plain Objective-C calls with valid arguments. The synthesizer
    // is kept alive until we stop listening.
    let synthesizer: Retained<AVSpeechSynthesizer> = unsafe {
        let kept = if reuse {
            take_synthesizer(voice.unwrap_or_default())
        } else {
            None
        };
        let synthesizer = kept.unwrap_or_else(|| AVSpeechSynthesizer::new());
        let utterance = AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));
        if let Some(found) = voice.filter(|v| !v.is_empty()).and_then(find_voice) {
            utterance.setVoice(Some(&found));
        }
        utterance.setRate(raw_rate.unwrap_or_else(|| utterance_rate(rate)));
        utterance.setPitchMultiplier(pitch.clamp(0.5, 2.0));
        utterance.setVolume(volume.clamp(0.0, 1.0));
        synthesizer.writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&callback));
        synthesizer
    };

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = 0u32;
    let mut first_audio = None;
    let mut chunks = 0usize;
    let mut callback_thread = String::new();
    loop {
        let waited = started.elapsed();
        let limit = if first_audio.is_some() {
            TOTAL_TIMEOUT
        } else {
            FIRST_AUDIO_TIMEOUT
        };
        if waited >= limit {
            drop(synthesizer);
            if first_audio.is_none() {
                bail!("the system voice produced no audio within {limit:?}");
            }
            bail!("the system voice did not finish within {limit:?}");
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((
                Chunk::Samples {
                    sample_rate: sr,
                    samples: s,
                },
                thread,
            )) => {
                first_audio.get_or_insert(started.elapsed());
                chunks += 1;
                callback_thread = thread;
                if sample_rate == 0 {
                    sample_rate = sr;
                }
                if sr == sample_rate {
                    samples.extend(s);
                }
            }
            Ok((Chunk::Unsupported(what), _)) => {
                bail!("the system voice returned audio we cannot read ({what})")
            }
            Ok((Chunk::End, _)) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("the system voice stopped unexpectedly"))
            }
        }
    }
    // A synthesizer that finished cleanly is kept for the next sentence; one
    // that timed out or failed was dropped above and is not reused.
    if reuse {
        keep_synthesizer(voice.unwrap_or_default(), synthesizer);
    } else {
        drop(synthesizer);
    }
    if sample_rate == 0 || samples.is_empty() {
        bail!("the system voice produced no audio for this text");
    }
    Ok(Rendered {
        audio: Audio {
            sample_rate,
            samples,
        },
        first_audio: first_audio.unwrap_or_default(),
        total: started.elapsed(),
        chunks,
        callback_thread,
    })
}

pub struct AvEngine;

impl AvEngine {
    /// Available when the main run loop is serviced; see the module docs.
    pub fn detect() -> Option<Self> {
        main_loop_is_serviced().then_some(Self)
    }
}

impl Engine for AvEngine {
    fn id(&self) -> &str {
        "av"
    }

    fn name(&self) -> &str {
        "macOS system voices (native, experimental)"
    }

    fn voices(&self) -> Result<Vec<Voice>> {
        // SAFETY: plain Objective-C calls.
        let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
        Ok(voices
            .iter()
            .map(|voice| {
                // SAFETY: plain property reads.
                unsafe {
                    Voice {
                        id: voice.identifier().to_string(),
                        name: voice.name().to_string(),
                        language: voice.language().to_string(),
                    }
                }
            })
            .collect())
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        if !main_loop_is_serviced() {
            bail!("the native macOS voice needs the app's main loop, which is not running");
        }
        render(
            &req.text,
            req.voice.as_deref(),
            req.rate,
            None,
            req.pitch,
            req.volume,
            true,
        )
        .map(|r| r.audio)
    }
}

/// Print how the native speech API behaves on this machine. Call from a
/// worker thread inside [`run_with_main_loop`].
pub fn probe() {
    let line = |label: &str, result: &Result<Rendered>| {
        match result {
        Ok(r) => println!(
            "AVPROBE {label}: ok first_audio={:?} total={:?} audio={:.3}s rate={} chunks={} callback_thread={}",
            r.first_audio,
            r.total,
            r.audio.duration_secs(),
            r.audio.sample_rate,
            r.chunks,
            r.callback_thread
        ),
        Err(e) => println!("AVPROBE {label}: FAILED {e:#}"),
    }
    };
    println!(
        "AVPROBE calling thread: {:?} main={} main_loop_serviced={}",
        std::thread::current().name(),
        NSThread::isMainThread_class(),
        main_loop_is_serviced()
    );
    // SAFETY: plain Objective-C calls.
    let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
    println!("AVPROBE voices: {}", voices.len());

    let short = "Short one.";
    let long = "Nobody knows why, although several theories have been put forward over the years, none of them entirely convincing.";

    let cold = render(short, None, 1.0, None, 1.0, 1.0, false);
    line("cold, short", &cold);
    if cold.is_err() {
        println!("AVPROBE conclusion: no audio even with the main run loop serviced");
        return;
    }
    line(
        "warm, short",
        &render(short, None, 1.0, None, 1.0, 1.0, false),
    );
    line(
        "warm, long",
        &render(long, None, 1.0, None, 1.0, 1.0, false),
    );
    line(
        "warm, short again",
        &render(short, None, 1.0, None, 1.0, 1.0, false),
    );
    line(
        "named voice (Samantha), short",
        &render(
            short,
            Some("com.apple.voice.super-compact.en-US.Samantha"),
            1.0,
            None,
            1.0,
            1.0,
            false,
        ),
    );
    line(
        "named voice (Fred), short",
        &render(
            short,
            Some("com.apple.speech.synthesis.voice.Fred"),
            1.0,
            None,
            1.0,
            1.0,
            false,
        ),
    );

    // The same again on one synthesizer kept between calls.
    for label in ["reused 1", "reused 2", "reused 3", "reused 4"] {
        line(
            &format!("{label}, short"),
            &render(short, None, 1.0, None, 1.0, 1.0, true),
        );
    }
    line(
        "reused, long",
        &render(long, None, 1.0, None, 1.0, 1.0, true),
    );
    line(
        "reused, by voice name (Daniel)",
        &render(short, Some("Daniel"), 1.0, None, 1.0, 1.0, true),
    );
    // Alternating voices, as with a voice per role: each should stay warm.
    for round in 1..=3 {
        for name in ["Daniel", "Samantha"] {
            line(
                &format!("alternating round {round}, {name}"),
                &render(short, Some(name), 1.0, None, 1.0, 1.0, true),
            );
        }
    }
    // The calibrated scale: these should come out at the speed asked for.
    let normal = render(long, None, 1.0, None, 1.0, 1.0, true).map(|r| r.audio.duration_secs());
    for speed in [0.5f32, 1.5, 2.5, 3.5] {
        if let (Ok(normal), Ok(r)) = (&normal, render(long, None, speed, None, 1.0, 1.0, true)) {
            println!(
                "AVPROBE speed {speed}x: measured {:.2}x",
                normal / r.audio.duration_secs()
            );
        }
    }

    // What the 0..1 rate scale means, measured as playback duration.
    for raw in [
        0.1f32, 0.2, 0.3, 0.4, 0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.9, 1.0,
    ] {
        match render(long, None, 1.0, Some(raw), 1.0, 1.0, false) {
            Ok(r) => println!(
                "AVPROBE rate {raw:.2}: audio={:.3}s synthesis={:?}",
                r.audio.duration_secs(),
                r.total
            ),
            Err(e) => println!("AVPROBE rate {raw:.2}: FAILED {e:#}"),
        }
    }

    // Three at once, as the player prepares ahead.
    let started = Instant::now();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| render(long, None, 1.0, None, 1.0, 1.0, false).is_ok()))
            .collect();
        let ok: Vec<bool> = handles
            .into_iter()
            .map(|h| h.join().unwrap_or(false))
            .collect();
        println!("AVPROBE three at once: {:?} ok={ok:?}", started.elapsed());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_scale_is_monotonic_and_hits_the_measured_points() {
        assert_eq!(utterance_rate(1.0), 0.5);
        assert!((utterance_rate(2.108) - 0.70).abs() < 1e-4);
        assert!((utterance_rate(1.45) - 0.576).abs() < 0.01);
        assert_eq!(utterance_rate(10.0), 1.0);
        assert!(utterance_rate(0.5) < 0.1 && utterance_rate(0.5) >= 0.0);
        let mut last = -1.0;
        for step in 0..=70 {
            let rate = utterance_rate(0.5 + step as f32 * 0.05);
            assert!(rate >= last, "not monotonic at step {step}");
            last = rate;
        }
        assert_eq!(utterance_rate(f32::NAN), 0.5);
    }
}
