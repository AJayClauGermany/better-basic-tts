//! eSpeak NG — small, fast, available everywhere. The default on Linux.

use super::{run_command, wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Words per minute eSpeak uses at rate 1.0.
const BASE_WPM: f32 = 175.0;

pub struct EspeakEngine {
    program: PathBuf,
}

impl EspeakEngine {
    pub fn detect() -> Option<Self> {
        ["espeak-ng", "espeak"]
            .iter()
            .find_map(|name| which::which(name).ok())
            .map(|program| Self { program })
    }

    fn args(req: &SynthRequest) -> Vec<String> {
        let wpm = (BASE_WPM * req.rate).round().clamp(80.0, 900.0) as i32;
        // eSpeak pitch is 0..=99 with 50 as the default.
        let pitch = (50.0 + (req.pitch - 1.0) * 50.0).round().clamp(0.0, 99.0) as i32;
        let amplitude = (req.volume * 100.0).round().clamp(0.0, 200.0) as i32;
        let mut args = vec![
            "-s".to_string(),
            wpm.to_string(),
            "-p".to_string(),
            pitch.to_string(),
            "-a".to_string(),
            amplitude.to_string(),
        ];
        if let Some(voice) = req.voice.as_deref().filter(|v| is_safe_voice(v)) {
            args.push("-v".into());
            args.push(voice.to_string());
        }
        // Text comes from stdin and `-m` (markup) is never passed, so the
        // text cannot be read as options or as SSML.
        args.push("--stdin".into());
        args.push("--stdout".into());
        args
    }
}

/// Voice ids come from the settings file; only accept what eSpeak itself
/// would list (`gmw/en-US`, `en-us+f3`).
fn is_safe_voice(v: &str) -> bool {
    !v.is_empty()
        && !v.starts_with('-')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '+'))
}

pub(crate) fn parse_voices(listing: &str) -> Vec<Voice> {
    let mut voices = Vec::new();
    for line in listing.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let (language, name, file) = (cols[1], cols[3], cols[4]);
        // MBROLA voices need extra data files that are rarely installed.
        if file.starts_with("mb/") {
            continue;
        }
        voices.push(Voice {
            id: file.to_string(),
            name: name.replace('_', " "),
            language: language.to_string(),
        });
    }
    voices
}

impl Engine for EspeakEngine {
    fn id(&self) -> &str {
        "espeak"
    }

    fn name(&self) -> &str {
        "eSpeak NG"
    }

    fn voices(&self) -> Result<Vec<Voice>> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--voices");
        let out = run_command(cmd, None)?;
        Ok(parse_voices(&String::from_utf8_lossy(&out)))
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        let mut cmd = Command::new(&self.program);
        cmd.args(Self::args(req));
        let out = run_command(cmd, Some(req.text.as_bytes()))?;
        wav::parse(&out).context("eSpeak produced unreadable audio")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_voice_table() {
        let listing = "Pty Language       Age/Gender VoiceName          File                 Other Languages\n 2  en-gb           --/M      English_(Great_Britain) gmw/en               (en 2)\n 3  en-uk           --/M      english-mb-en1     mb/mb-en1            (en-gb 3)(en 2)\n 2  en-us           --/M      English_(America)  gmw/en-US            (en 3)\n";
        let v = parse_voices(listing);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].id, "gmw/en-US");
        assert_eq!(v[1].name, "English (America)");
        assert_eq!(v[1].language, "en-us");
    }

    #[test]
    fn maps_rate_and_rejects_odd_voices() {
        let mut req = SynthRequest::new("hi");
        req.rate = 2.0;
        req.voice = Some("--evil".into());
        let args = EspeakEngine::args(&req);
        assert_eq!(args[1], "350");
        assert!(!args.contains(&"-v".to_string()));
        req.voice = Some("gmw/en-US".into());
        assert!(EspeakEngine::args(&req).contains(&"gmw/en-US".to_string()));
    }

    #[test]
    fn speaks_if_installed() {
        let Some(engine) = EspeakEngine::detect() else {
            eprintln!("espeak-ng not installed; skipping");
            return;
        };
        assert!(!engine.voices().unwrap().is_empty());
        let slow = engine.synthesize(&SynthRequest::new("Hello world.")).unwrap();
        assert!(slow.duration_secs() > 0.3);
        let mut req = SynthRequest::new("Hello world.");
        req.rate = 2.0;
        let fast = engine.synthesize(&req).unwrap();
        assert!(fast.duration_secs() < slow.duration_secs());
    }
}
