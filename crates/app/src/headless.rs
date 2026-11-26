//! Everything the command line can do without opening a window.

use anyhow::{anyhow, bail, Context, Result};
use bbtts_core::engine::{wav, Audio, SynthRequest};
use bbtts_core::player::output::{AudioOutput, DeviceOutput};
use bbtts_core::player::PlayerConfig;
use bbtts_core::{EngineRegistry, Player, Settings, Status};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Print to stdout, quietly giving up if the reader went away
/// (`better-basic-tts voices | head`).
fn print_all(text: &str) {
    use std::io::Write as _;
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

fn engine_for(settings: &Settings) -> Result<Arc<dyn bbtts_core::Engine>> {
    let registry = EngineRegistry::detect(&settings.engines);
    if settings.engine != "auto"
        && !settings.engine.is_empty()
        && !registry.engines().iter().any(|e| e.id() == settings.engine)
    {
        bail!(
            "unknown engine \"{}\" (run `better-basic-tts engines` for a list)",
            settings.engine
        );
    }
    registry
        .resolve(&settings.engine)
        .ok_or_else(|| anyhow!(EngineRegistry::missing_engine_help()))
}

pub fn list_engines(settings: &Settings) -> Result<()> {
    let registry = EngineRegistry::detect(&settings.engines);
    if registry.is_empty() {
        bail!(EngineRegistry::missing_engine_help());
    }
    let active = registry
        .resolve(&settings.engine)
        .map(|e| e.id().to_string());
    let mut out = String::new();
    for engine in registry.engines() {
        let mark = if Some(engine.id()) == active.as_deref() {
            "*"
        } else {
            " "
        };
        let _ = writeln!(out, "{mark} {:<12} {}", engine.id(), engine.name());
    }
    print_all(&out);
    Ok(())
}

pub fn list_voices(settings: &Settings) -> Result<()> {
    let engine = engine_for(settings)?;
    let voices = engine.voices()?;
    let mut out = String::new();
    if voices.is_empty() {
        let _ = writeln!(out, "{} reports no voices.", engine.name());
    }
    for voice in voices {
        let _ = writeln!(
            out,
            "{:<36} {:<12} {}",
            voice.id, voice.language, voice.name
        );
    }
    print_all(&out);
    Ok(())
}

/// Print the result of the text pipeline: role, what is shown, what is said.
pub fn explain(settings: &Settings, text: &str) {
    let mut out = String::new();
    for (i, u) in settings.prepare(text).iter().enumerate() {
        let _ = writeln!(out, "{:>3} [{}] {}", i + 1, u.role.key(), u.display);
        if u.spoken != u.display {
            let _ = writeln!(out, "      spoken as: {}", u.spoken);
        }
    }
    print_all(&out);
}

/// Synthesize the whole text into one WAV file.
pub fn render_to_file(settings: &Settings, text: &str, path: &Path) -> Result<()> {
    let engine = engine_for(settings)?;
    let utterances = settings.prepare(text);
    if utterances.is_empty() {
        bail!("there is no text to read");
    }
    let mut out: Option<Audio> = None;
    for (i, u) in utterances.iter().enumerate() {
        let style = settings.roles.get(u.role);
        let starts_block = i == 0 || utterances[i - 1].ends_block;
        let text = if starts_block && !style.announce.trim().is_empty() {
            format!("{} {}", style.announce.trim(), u.spoken)
        } else {
            u.spoken.clone()
        };
        let mut audio = engine
            .synthesize(&SynthRequest {
                text,
                voice: if !style.voice.is_empty() {
                    Some(style.voice.clone())
                } else if !settings.voice.is_empty() {
                    Some(settings.voice.clone())
                } else {
                    None
                },
                rate: settings.rate * style.rate,
                pitch: style.pitch,
                volume: (settings.volume * style.volume).clamp(0.0, 1.0),
            })
            .with_context(|| format!("could not synthesize sentence {}", i + 1))?;
        audio.trim_silence(25);
        audio.append_silence(if u.ends_block {
            style.pause_after_ms
        } else {
            settings.sentence_pause_ms
        });
        match &mut out {
            None => out = Some(audio),
            Some(total) => {
                let audio = resample(audio, total.sample_rate);
                total.samples.extend(audio.samples);
            }
        }
    }
    let audio = out.expect("at least one utterance");
    std::fs::write(path, wav::encode(&audio))
        .with_context(|| format!("could not write {}", path.display()))?;
    println!(
        "Wrote {} ({:.1} s, {} sentences)",
        path.display(),
        audio.duration_secs(),
        utterances.len()
    );
    Ok(())
}

/// Linear resampling, only needed when role voices differ in sample rate.
fn resample(audio: Audio, target: u32) -> Audio {
    if audio.sample_rate == target || audio.samples.is_empty() {
        return audio;
    }
    let ratio = audio.sample_rate as f64 / target as f64;
    let n = (audio.samples.len() as f64 / ratio) as usize;
    let samples = (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let a = pos.floor() as usize;
            let b = (a + 1).min(audio.samples.len() - 1);
            let t = (pos - a as f64) as f32;
            audio.samples[a] * (1.0 - t) + audio.samples[b] * t
        })
        .collect();
    Audio {
        sample_rate: target,
        samples,
    }
}

/// Speak through the default audio device and return when done.
pub fn speak_and_wait(settings: &Settings, text: &str) -> Result<()> {
    let engine = engine_for(settings)?;
    let utterances = settings.prepare(text);
    if utterances.is_empty() {
        bail!("there is no text to read");
    }
    DeviceOutput::probe()?;
    let (tx, rx) = flume::unbounded::<()>();
    let player = Player::new(
        || -> Box<dyn AudioOutput> { Box::new(DeviceOutput::new()) },
        PlayerConfig::from_settings(settings, Some(engine)),
        settings.rate,
        move || {
            let _ = tx.send(());
        },
    );
    player.speak(utterances, "");
    let mut started = false;
    loop {
        let _ = rx.recv_timeout(Duration::from_millis(200));
        let snapshot = player.snapshot();
        if let Some(error) = snapshot.error {
            bail!(error);
        }
        match snapshot.status {
            Status::Idle if started => return Ok(()),
            Status::Idle => {}
            _ => started = true,
        }
    }
}

/// Text for `benchmark` when none is given: sentences of mixed length.
pub const BENCHMARK_SAMPLE: &str = "\
The quick brown fox jumps over the lazy dog. It does so every morning. \
Nobody knows why, although several theories have been put forward over the years, \
none of them entirely convincing. Short one. \
Reading aloud works best when one sentence follows the next without a noticeable gap. \
That is what this measures. If the engine needs longer to produce a sentence than the \
sentence takes to play, the listener hears a pause. Otherwise they should not.";

/// Time the speech engine against playback, sentence by sentence.
pub fn benchmark(settings: &Settings, text: &str) -> Result<()> {
    use std::time::Instant;

    let engine = engine_for(settings)?;
    let utterances: Vec<_> = settings.prepare(text).into_iter().take(12).collect();
    if utterances.is_empty() {
        bail!("there is no text to read");
    }
    let request = |u: &bbtts_core::Utterance| {
        let style = settings.roles.get(u.role);
        SynthRequest {
            text: u.spoken.clone(),
            voice: if !style.voice.is_empty() {
                Some(style.voice.clone())
            } else if !settings.voice.is_empty() {
                Some(settings.voice.clone())
            } else {
                None
            },
            rate: settings.rate * style.rate,
            pitch: style.pitch,
            volume: settings.volume,
        }
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Engine: {}   voice: {}   speed: {}x\n",
        engine.name(),
        if settings.voice.is_empty() {
            "(default)"
        } else {
            &settings.voice
        },
        settings.rate
    );
    let _ = writeln!(
        out,
        "{:>3}  {:>9}  {:>8}  {:>7}  text",
        "#", "synthesis", "playback", "ratio"
    );
    print_all(&out);

    // One at a time. The first call may include loading the voice.
    let (mut synth_total, mut audio_total) = (0.0f64, 0.0f64);
    for (i, u) in utterances.iter().enumerate() {
        let started = Instant::now();
        let audio = engine
            .synthesize(&request(u))
            .with_context(|| format!("could not synthesize sentence {}", i + 1))?;
        let synth = started.elapsed().as_secs_f64();
        let playback = audio.duration_secs();
        let ratio = playback / synth.max(1e-6);
        synth_total += synth;
        audio_total += playback;
        let preview: String = u.display.chars().take(40).collect();
        print_all(&format!(
            "{:>3}  {:>7.0}ms  {:>6.0}ms  {:>6.1}x  {}\n",
            i + 1,
            synth * 1000.0,
            playback * 1000.0,
            ratio,
            preview
        ));
    }

    // The way the player prepares ahead: three calls at once, and several
    // sentences per call for engines that are cheaper that way.
    let batch_size = engine.batch_size().max(1);
    let started = Instant::now();
    let queue = std::sync::Mutex::new(utterances.chunks(batch_size));
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| loop {
                let next = queue.lock().unwrap().next();
                let Some(chunk) = next else { break };
                let texts: Vec<String> = chunk.iter().map(|u| u.spoken.clone()).collect();
                let _ = engine.synthesize_batch(&texts, &request(&chunk[0]));
            });
        }
    });
    let parallel = audio_total / started.elapsed().as_secs_f64().max(1e-6);
    let sequential = audio_total / synth_total.max(1e-6);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nOne sentence at a time: {sequential:.1}x faster than playback."
    );
    let how = if batch_size > 1 {
        format!("Prepared ahead ({batch_size} per call, 3 calls at once):")
    } else {
        "Prepared ahead (3 at once):".to_string()
    };
    let _ = writeln!(out, "{how} {parallel:.1}x faster than playback.");
    let verdict = if parallel >= 3.0 {
        "The engine keeps up comfortably; reading should not pause between sentences."
    } else if parallel >= 1.2 {
        "The engine only just keeps up at this speed; an occasional pause is possible."
    } else {
        "The engine is slower than playback at this speed; reading will pause between sentences."
    };
    let _ = writeln!(out, "{verdict}");
    print_all(&out);
    Ok(())
}
