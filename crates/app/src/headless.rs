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
