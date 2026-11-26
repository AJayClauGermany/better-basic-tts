//! A probe of the native macOS speech API (`AVSpeechSynthesizer`).
//!
//! `/usr/bin/say` costs about a second per call. Synthesising in-process
//! should not, but how the API behaves outside a normal app (which thread
//! the audio arrives on, whether a run loop is needed, what the rate scale
//! means) is not documented well enough to build on blind. This test runs
//! on the Mac CI runner and prints what it finds; it only fails if the API
//! cannot be called at all.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioCommonFormat, AVAudioPCMBuffer, AVSpeechSynthesisVoice,
    AVSpeechSynthesizer, AVSpeechUtterance,
};
use objc2_foundation::{NSDate, NSRunLoop, NSString};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

enum Chunk {
    Audio {
        frames: usize,
        sample_rate: f64,
        format: String,
        thread: String,
    },
    End,
}

struct Outcome {
    first_audio: Option<Duration>,
    total: Duration,
    frames: usize,
    sample_rate: f64,
    format: String,
    thread: String,
    finished: bool,
}

/// Synthesize `text`, waiting at most `timeout`. With `pump`, the calling
/// thread's run loop is turned while waiting.
fn synthesize(
    synthesizer: &AVSpeechSynthesizer,
    text: &str,
    rate: Option<f32>,
    voice: Option<&AVSpeechSynthesisVoice>,
    timeout: Duration,
    pump: bool,
) -> Outcome {
    let (tx, rx) = mpsc::channel::<Chunk>();
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
                let kind = match format.commonFormat() {
                    AVAudioCommonFormat::PCMFormatFloat32 => "float32",
                    AVAudioCommonFormat::PCMFormatInt16 => "int16",
                    AVAudioCommonFormat::PCMFormatInt32 => "int32",
                    AVAudioCommonFormat::PCMFormatFloat64 => "float64",
                    _ => "other",
                };
                Chunk::Audio {
                    frames,
                    sample_rate: format.sampleRate(),
                    format: format!(
                        "{kind}, {} channel(s), interleaved={}",
                        format.channelCount(),
                        format.isInterleaved()
                    ),
                    thread: format!(
                        "{:?} (main={})",
                        std::thread::current().name(),
                        objc2_foundation::NSThread::isMainThread_class()
                    ),
                }
            }
        };
        let _ = tx.lock().unwrap().send(chunk);
    });

    let started = Instant::now();
    // SAFETY: plain Objective-C calls with valid arguments.
    unsafe {
        let utterance = AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));
        if let Some(rate) = rate {
            utterance.setRate(rate);
        }
        if voice.is_some() {
            utterance.setVoice(voice);
        }
        synthesizer.writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&callback));
    }

    let mut outcome = Outcome {
        first_audio: None,
        total: Duration::ZERO,
        frames: 0,
        sample_rate: 0.0,
        format: String::new(),
        thread: String::new(),
        finished: false,
    };
    let deadline = started + timeout;
    while Instant::now() < deadline {
        if pump {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            NSRunLoop::currentRunLoop().runUntilDate(&until);
        }
        let wait = if pump {
            Duration::from_millis(1)
        } else {
            Duration::from_millis(50)
        };
        match rx.recv_timeout(wait) {
            Ok(Chunk::Audio {
                frames,
                sample_rate,
                format,
                thread,
            }) => {
                outcome.first_audio.get_or_insert(started.elapsed());
                outcome.frames += frames;
                outcome.sample_rate = sample_rate;
                outcome.format = format;
                outcome.thread = thread;
            }
            Ok(Chunk::End) => {
                outcome.finished = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    outcome.total = started.elapsed();
    outcome
}

fn report(label: &str, o: &Outcome) {
    let audio = if o.sample_rate > 0.0 {
        o.frames as f64 / o.sample_rate
    } else {
        0.0
    };
    eprintln!(
        "AVPROBE {label}: finished={} first_audio={:?} total={:?} audio={audio:.2}s rate={} format=[{}] callback_thread={}",
        o.finished, o.first_audio, o.total, o.sample_rate, o.format, o.thread
    );
}

// Run explicitly (`cargo test -p bbtts-core av_probe -- --ignored --nocapture`):
// it exercises an API we do not depend on yet and must not be able to break
// the normal test run.
#[test]
#[ignore = "exploratory; run on a Mac with --ignored --nocapture"]
fn probe_native_speech_api() {
    eprintln!(
        "AVPROBE test thread: {:?} (main={})",
        std::thread::current().name(),
        objc2_foundation::NSThread::isMainThread_class()
    );
    // SAFETY: plain Objective-C calls.
    let (synthesizer, voices): (Retained<AVSpeechSynthesizer>, _) = unsafe {
        (
            AVSpeechSynthesizer::new(),
            AVSpeechSynthesisVoice::speechVoices(),
        )
    };
    eprintln!("AVPROBE voices: {}", voices.len());
    for voice in voices.iter().take(400) {
        // SAFETY: plain property reads.
        let (id, name, language) = unsafe {
            (
                voice.identifier().to_string(),
                voice.name().to_string(),
                voice.language().to_string(),
            )
        };
        if language.starts_with("en-US") || language.starts_with("en-GB") {
            eprintln!("AVPROBE voice: {name} [{language}] {id}");
        }
    }

    let short = "Short one.";
    let long = "Nobody knows why, although several theories have been put forward over the years, none of them entirely convincing.";

    // 1. Without a run loop: does audio arrive at all, and how fast?
    let first = synthesize(
        &synthesizer,
        short,
        None,
        None,
        Duration::from_secs(12),
        false,
    );
    report("no run loop, cold, short", &first);
    let needs_pump = !first.finished;
    if needs_pump {
        let pumped = synthesize(
            &synthesizer,
            short,
            None,
            None,
            Duration::from_secs(12),
            true,
        );
        report("pumping this thread's run loop, short", &pumped);
        if !pumped.finished {
            eprintln!("AVPROBE conclusion: no audio from a background thread, with or without its run loop");
            return;
        }
    }

    // 2. Warm calls: the cost per sentence once the voice is loaded.
    for (label, text) in [
        ("warm, short", short),
        ("warm, long", long),
        ("warm, short again", short),
    ] {
        let o = synthesize(
            &synthesizer,
            text,
            None,
            None,
            Duration::from_secs(20),
            needs_pump,
        );
        report(label, &o);
    }

    // 3. A fresh synthesizer object each time (is the object or the process warm?).
    // SAFETY: plain Objective-C call.
    let fresh = unsafe { AVSpeechSynthesizer::new() };
    let o = synthesize(
        &fresh,
        short,
        None,
        None,
        Duration::from_secs(20),
        needs_pump,
    );
    report("new synthesizer object, short", &o);

    // 4. What the 0..1 rate scale means, in playback duration.
    for rate in [0.2f32, 0.3, 0.4, 0.5, 0.55, 0.6, 0.65, 0.7, 0.8, 0.9, 1.0] {
        let o = synthesize(
            &synthesizer,
            long,
            Some(rate),
            None,
            Duration::from_secs(30),
            needs_pump,
        );
        let audio = if o.sample_rate > 0.0 {
            o.frames as f64 / o.sample_rate
        } else {
            0.0
        };
        eprintln!(
            "AVPROBE rate {rate:.2}: finished={} audio={audio:.3}s synthesis={:?}",
            o.finished, o.total
        );
    }

    // 5. Three at once on one synthesizer, as the player would ask.
    let started = Instant::now();
    let o = synthesize(
        &synthesizer,
        long,
        None,
        None,
        Duration::from_secs(30),
        needs_pump,
    );
    eprintln!(
        "AVPROBE sequential long: {:?} finished={}",
        started.elapsed(),
        o.finished
    );
    std::thread::scope(|scope| {
        let started = Instant::now();
        let handles: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    // SAFETY: plain Objective-C call.
                    let own = unsafe { AVSpeechSynthesizer::new() };
                    synthesize(&own, long, None, None, Duration::from_secs(30), needs_pump).finished
                })
            })
            .collect();
        let finished: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        eprintln!(
            "AVPROBE three synthesizers in parallel: {:?} finished={finished:?}",
            started.elapsed()
        );
    });
}
