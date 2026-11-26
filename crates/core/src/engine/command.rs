//! User-defined engines: any local program that can turn text into a WAV.
//!
//! This is how neural voices (Piper, KittenTTS, NeuTTS, ...) plug in without
//! the app having to bundle their runtimes. Example `settings.toml` entry:
//!
//! ```toml
//! [[engines]]
//! id = "piper"
//! name = "Piper"
//! program = "piper"
//! args = ["--model", "{voice}", "--length_scale", "{length_scale}", "--output_file", "{output}"]
//! voices = ["/home/me/voices/en_US-lessac-medium.onnx"]
//! ```
//!
//! The text is written to the program's stdin (or to a temp file if an
//! argument contains `{input}`); it is never spliced into the command line,
//! and the program is started directly, not through a shell.

use super::{run_command, temp_path, wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEngineConfig {
    /// Unique id used in `engine = "..."`.
    pub id: String,
    /// Name shown in the settings.
    #[serde(default)]
    pub name: String,
    /// Executable (looked up on PATH).
    pub program: String,
    /// Arguments. Placeholders:
    ///
    /// * `{voice}` – selected voice id (or the first of `voices`)
    /// * `{rate}` – speed multiplier, e.g. `1.25`
    /// * `{length_scale}` – `1 / rate`, what Piper-style engines expect
    /// * `{wpm}` – words per minute (`rate * base_wpm`)
    /// * `{pitch}` – pitch multiplier
    /// * `{output}` – path of a WAV file the program must write; without it
    ///   the WAV is read from stdout
    /// * `{input}` – path of a UTF-8 text file; without it the text is piped
    ///   to stdin
    #[serde(default)]
    pub args: Vec<String>,
    /// Voice ids offered in the UI.
    #[serde(default)]
    pub voices: Vec<String>,
    /// Words per minute at rate 1.0, for `{wpm}`.
    #[serde(default = "default_wpm")]
    pub base_wpm: f32,
    /// If set, stdout is raw little-endian 16-bit mono PCM at this rate
    /// instead of a WAV file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_sample_rate: Option<u32>,
}

fn default_wpm() -> f32 {
    175.0
}

pub struct CommandEngine {
    cfg: CommandEngineConfig,
    name: String,
}

impl CommandEngine {
    pub fn new(cfg: CommandEngineConfig) -> Self {
        let name = if cfg.name.is_empty() {
            cfg.id.clone()
        } else {
            cfg.name.clone()
        };
        Self { cfg, name }
    }
}

impl Engine for CommandEngine {
    fn id(&self) -> &str {
        &self.cfg.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn voices(&self) -> Result<Vec<Voice>> {
        Ok(self
            .cfg
            .voices
            .iter()
            .map(|v| Voice {
                id: v.clone(),
                // Show file names rather than full model paths.
                name: std::path::Path::new(v)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| v.clone()),
                language: String::new(),
            })
            .collect())
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        let voice = req
            .voice
            .clone()
            .filter(|v| self.cfg.voices.is_empty() || self.cfg.voices.contains(v))
            .or_else(|| self.cfg.voices.first().cloned())
            .unwrap_or_default();
        let rate = req.rate.clamp(0.1, 10.0);
        let uses_output = self.cfg.args.iter().any(|a| a.contains("{output}"));
        let uses_input = self.cfg.args.iter().any(|a| a.contains("{input}"));
        let output = temp_path("wav");
        let input = temp_path("txt");

        let mut cmd = Command::new(&self.cfg.program);
        for arg in &self.cfg.args {
            cmd.arg(
                arg.replace("{voice}", &voice)
                    .replace("{rate}", &format!("{rate:.3}"))
                    .replace("{length_scale}", &format!("{:.3}", 1.0 / rate))
                    .replace("{wpm}", &format!("{:.0}", self.cfg.base_wpm * rate))
                    .replace("{pitch}", &format!("{:.3}", req.pitch))
                    .replace("{output}", &output.to_string_lossy())
                    .replace("{input}", &input.to_string_lossy()),
            );
        }
        if uses_input {
            std::fs::write(&input, req.text.as_bytes())?;
        }
        let stdin = (!uses_input).then_some(req.text.as_bytes());
        let result = run_command(cmd, stdin).and_then(|stdout| {
            let bytes = if uses_output {
                std::fs::read(&output)
                    .with_context(|| format!("{} did not write its output file", self.name))?
            } else {
                stdout
            };
            if bytes.is_empty() {
                bail!("{} produced no audio", self.name);
            }
            match self.cfg.raw_sample_rate {
                Some(sr) if !uses_output => Ok(wav::from_raw_s16le(&bytes, sr)),
                _ => wav::parse(&bytes)
                    .with_context(|| format!("{} produced unreadable audio", self.name)),
            }
        });
        if uses_input {
            let _ = std::fs::remove_file(&input);
        }
        if uses_output {
            let _ = std::fs::remove_file(&output);
        }
        result
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn stdin_to_stdout_raw() {
        // `cat` echoes the text bytes back; read them as raw PCM.
        let engine = CommandEngine::new(CommandEngineConfig {
            id: "cat".into(),
            name: String::new(),
            program: "cat".into(),
            args: vec![],
            voices: vec![],
            base_wpm: 175.0,
            raw_sample_rate: Some(8000),
        });
        let audio = engine.synthesize(&SynthRequest::new("abcdefgh")).unwrap();
        assert_eq!(audio.sample_rate, 8000);
        assert_eq!(audio.samples.len(), 4);
    }

    #[test]
    fn placeholders_and_output_file() {
        // sh -c 'cp fixture $1' -- {output}: the text never reaches argv.
        let fixture = temp_path("wav");
        std::fs::write(
            &fixture,
            wav::encode(&Audio {
                sample_rate: 16000,
                samples: vec![0.25; 32],
            }),
        )
        .unwrap();
        let engine = CommandEngine::new(CommandEngineConfig {
            id: "copy".into(),
            name: "Copy".into(),
            program: "cp".into(),
            args: vec![fixture.to_string_lossy().into_owned(), "{output}".into()],
            voices: vec!["a".into()],
            base_wpm: 175.0,
            raw_sample_rate: None,
        });
        let audio = engine
            .synthesize(&SynthRequest::new("$(rm -rf /); `x`"))
            .unwrap();
        assert_eq!(audio.samples.len(), 32);
        std::fs::remove_file(fixture).unwrap();
    }

    #[test]
    fn missing_program_is_an_error() {
        let engine = CommandEngine::new(CommandEngineConfig {
            id: "nope".into(),
            name: String::new(),
            program: "definitely-not-a-real-tts-binary".into(),
            args: vec![],
            voices: vec![],
            base_wpm: 175.0,
            raw_sample_rate: None,
        });
        assert!(engine.synthesize(&SynthRequest::new("x")).is_err());
    }
}
