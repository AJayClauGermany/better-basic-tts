//! macOS system voices through `/usr/bin/say`.
//!
//! `say` renders to a file instead of the speakers, which gives us the same
//! PCM-in-hand model as every other engine. The text is passed in a file and
//! never on the command line.

use super::{run_command, temp_path, wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// Words per minute at rate 1.0. `say` voices default to roughly this.
const BASE_WPM: f32 = 180.0;

/// Sentences rendered per `say` call when preparing ahead.
const BATCH_SIZE: usize = 5;
/// Silence requested between the sentences of a batch, in milliseconds.
/// Far longer than any pause inside a sentence, so the cuts are unambiguous.
const SEPARATOR_MS: u32 = 3000;
/// Stop batching for a voice after this many failed attempts in a row.
const MAX_BATCH_FAILURES: u32 = 2;

pub struct SayEngine {
    program: PathBuf,
    /// Per voice: how often in a row a batch could not be taken apart.
    batch_failures: Mutex<HashMap<String, u32>>,
}

impl SayEngine {
    pub fn detect() -> Option<Self> {
        let program = PathBuf::from("/usr/bin/say");
        program.exists().then(|| Self {
            program,
            batch_failures: Mutex::default(),
        })
    }
}

/// macOS speech has *embedded commands*: `[[rate 400]]`, `[[volm 0]]`,
/// `[[slnc 60000]]`, ... Text we read must not be able to issue them, so the
/// delimiters are broken up. This is the macOS analogue of the angle bracket
/// handling in `text::speakable`.
pub(crate) fn neutralize_embedded_commands(text: &str) -> String {
    let mut out = text.to_string();
    while out.contains("[[") {
        out = out.replace("[[", "[ [");
    }
    while out.contains("]]") {
        out = out.replace("]]", "] ]");
    }
    out
}

/// Parse the output of `say -v ?`:
///
/// ```text
/// Alex                en_US    # Most people recognize me by my voice.
/// Bad News            en_US    # The light you see at the end of the tunnel...
/// Eddy (English (UK)) en_GB    # Hello! My name is Eddy.
/// ```
pub(crate) fn parse_voices(listing: &str) -> Vec<Voice> {
    let mut voices = Vec::new();
    for line in listing.lines() {
        let head = line.split('#').next().unwrap_or(line).trim_end();
        let Some(split) = head.rfind(char::is_whitespace) else {
            continue;
        };
        let (name, locale) = (head[..split].trim(), head[split..].trim());
        if name.is_empty() || locale.is_empty() {
            continue;
        }
        voices.push(Voice {
            id: name.to_string(),
            name: name.to_string(),
            language: locale.replace('_', "-"),
        });
    }
    voices
}

impl Engine for SayEngine {
    fn id(&self) -> &str {
        "say"
    }

    fn name(&self) -> &str {
        "macOS system voices"
    }

    fn voices(&self) -> Result<Vec<Voice>> {
        let mut cmd = Command::new(&self.program);
        cmd.args(["-v", "?"]);
        let out = run_command(cmd, None)?;
        Ok(parse_voices(&String::from_utf8_lossy(&out)))
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        self.render(&neutralize_embedded_commands(&req.text), req)
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    fn synthesize_batch(&self, texts: &[String], params: &SynthRequest) -> Result<Vec<Audio>> {
        if let Some(pieces) = self.try_batch(texts, params)? {
            return Ok(pieces);
        }
        texts
            .iter()
            .map(|text| {
                self.synthesize(&SynthRequest {
                    text: text.clone(),
                    ..params.clone()
                })
            })
            .collect()
    }
}

impl SayEngine {
    /// Run `say` on text that is already safe (embedded commands defused,
    /// except the ones we add ourselves).
    fn render(&self, safe_text: &str, req: &SynthRequest) -> Result<Audio> {
        let input = temp_path("txt");
        let output = temp_path("wav");
        // Our own, trusted, embedded commands for what `say` has no flag for.
        let mut text = String::new();
        if (req.volume - 1.0).abs() > 0.01 {
            text.push_str(&format!("[[volm {:.2}]] ", req.volume.clamp(0.0, 1.0)));
        }
        if (req.pitch - 1.0).abs() > 0.01 {
            // Relative baseline pitch in semitone-ish steps.
            let delta = (12.0 * req.pitch.max(0.1).log2()).round() as i32;
            let sign = if delta >= 0 { "+" } else { "-" };
            text.push_str(&format!("[[pbas {sign}{}]] ", delta.abs()));
        }
        text.push_str(safe_text);
        std::fs::write(&input, text.as_bytes()).context("could not write temp text file")?;

        let wpm = (BASE_WPM * req.rate).round().clamp(60.0, 720.0) as i32;
        let mut cmd = Command::new(&self.program);
        if let Some(voice) = req
            .voice
            .as_deref()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
        {
            cmd.arg("-v").arg(voice);
        }
        cmd.arg("-r")
            .arg(wpm.to_string())
            .arg("-o")
            .arg(&output)
            .arg("--file-format=WAVE")
            .arg("--data-format=LEI16@22050")
            .arg("-f")
            .arg(&input);
        let result = run_command(cmd, None).and_then(|_| {
            let bytes = std::fs::read(&output).context("`say` produced no audio file")?;
            wav::parse(&bytes).context("`say` produced unreadable audio")
        });
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
        result
    }

    /// Render several sentences with one `say` call and cut the result apart.
    ///
    /// Starting `say` costs about a second no matter how short the text is,
    /// which is more than a short sentence takes to play at a brisk speed.
    /// So the sentences are sent together, separated by a long silence that
    /// we ask for with an embedded command, and the audio is split at those
    /// silences afterwards.
    ///
    /// Returns `Ok(None)` when that did not yield exactly one piece per
    /// sentence (a voice may ignore the silence command); the caller then
    /// renders the sentences one by one. A voice that keeps failing is no
    /// longer tried.
    fn try_batch(&self, texts: &[String], params: &SynthRequest) -> Result<Option<Vec<Audio>>> {
        if texts.len() < 2 {
            return Ok(None);
        }
        let voice = params.voice.clone().unwrap_or_default();
        if self
            .batch_failures
            .lock()
            .unwrap()
            .get(&voice)
            .copied()
            .unwrap_or(0)
            >= MAX_BATCH_FAILURES
        {
            return Ok(None);
        }
        let separator = format!(" [[slnc {SEPARATOR_MS}]] ");
        let joined = texts
            .iter()
            .map(|t| neutralize_embedded_commands(t))
            .collect::<Vec<_>>()
            .join(&separator);
        let audio = self.render(&joined, params)?;
        let pieces = audio.split_at_pauses(texts.len());
        let mut failures = self.batch_failures.lock().unwrap();
        match pieces {
            Some(pieces) => {
                failures.remove(&voice);
                Ok(Some(pieces))
            }
            None => {
                let count = failures.entry(voice).or_insert(0);
                *count += 1;
                log::info!(
                    "could not take {} sentences apart after rendering them together (attempt {count})",
                    texts.len()
                );
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_voice_listing() {
        let listing = "Alex                en_US    # Most people recognize me by my voice.\nBad News            en_US    # The light you see\nEddy (English (UK)) en_GB    # Hello! My name is Eddy.\n";
        let v = parse_voices(listing);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].id, "Alex");
        assert_eq!(v[1].id, "Bad News");
        assert_eq!(v[2].id, "Eddy (English (UK))");
        assert_eq!(v[2].language, "en-GB");
    }

    #[test]
    fn embedded_commands_are_defused() {
        assert_eq!(
            neutralize_embedded_commands("a [[rate 900]] b [[[x]]]"),
            "a [ [rate 900] ] b [ [ [x] ] ]"
        );
    }
}

/// These run on a real Mac only (CI has one): they are the proof that the
/// system voice honours the silence command and that batching is used.
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;

    #[test]
    fn batches_are_taken_apart_per_sentence() {
        let engine = SayEngine::detect().expect("/usr/bin/say");
        let texts: Vec<String> = [
            "The quick brown fox jumps over the lazy dog.",
            "Short one.",
            "Wait, this one has a comma, and a second clause; even a semicolon.",
            "Done.",
        ]
        .map(String::from)
        .to_vec();
        for rate in [1.0, 2.5] {
            let mut params = SynthRequest::new("");
            params.rate = rate;
            let pieces = engine
                .try_batch(&texts, &params)
                .unwrap()
                .unwrap_or_else(|| panic!("batch at rate {rate} could not be taken apart"));
            assert_eq!(pieces.len(), texts.len());
            let durations: Vec<f64> = pieces.iter().map(|p| p.duration_secs()).collect();
            eprintln!("rate {rate}: sentence durations {durations:?}");
            for d in &durations {
                assert!(*d > 0.1, "a piece is empty: {durations:?}");
            }
            // The long sentences are longer than the short ones.
            assert!(durations[0] > durations[1] && durations[2] > durations[3]);
            // And each piece matches the sentence rendered on its own.
            for (text, piece) in texts.iter().zip(&pieces) {
                let mut alone = engine
                    .synthesize(&SynthRequest {
                        text: text.clone(),
                        ..params.clone()
                    })
                    .unwrap();
                alone.trim_silence(0);
                let (a, b) = (alone.duration_secs(), piece.duration_secs());
                assert!(
                    (a - b).abs() < 0.35 + a * 0.2,
                    "{text:?}: alone {a:.2}s, in batch {b:.2}s"
                );
            }
        }
    }
}
