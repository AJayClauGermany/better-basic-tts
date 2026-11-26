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
//!
//! # Persistent engines
//!
//! Neural models take seconds to load, which is far too slow to pay once per
//! sentence. With `persistent = true` the program is started once and then
//! spoken to over a tiny line protocol (one JSON object per line):
//!
//! ```text
//! → {"text": "Hello.", "voice": "expr-voice-2-f", "rate": 1.2, "pitch": 1.0, "output": "/tmp/x.wav"}
//! ← {"ok": true}
//! → {"voices": true}
//! ← {"ok": true, "voices": [{"id": "expr-voice-2-f", "name": "Voice 2 (female)", "language": "en"}]}
//! ← {"ok": false, "error": "what went wrong"}        (on failure)
//! ```
//!
//! The program writes the WAV to the `output` path and answers on stdout.
//! Stdout lines that are not JSON objects are ignored, because libraries
//! like to print there. See `contrib/engines` for ready-made adapters.

use super::{run_command, temp_path, wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a persistent engine may take to answer one request. The first
/// request includes loading (and possibly downloading) the model.
const PERSISTENT_TIMEOUT: Duration = Duration::from_secs(300);

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
    /// Keep the program running and talk to it over the JSON line protocol
    /// described in the module documentation. `args` are passed once, at
    /// start, without placeholder substitution.
    #[serde(default, skip_serializing_if = "is_false")]
    pub persistent: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_wpm() -> f32 {
    175.0
}

pub struct CommandEngine {
    cfg: CommandEngineConfig,
    name: String,
    /// The running program, for persistent engines.
    server: Mutex<Option<Server>>,
    /// Voices reported by a persistent engine.
    reported_voices: Mutex<Option<Vec<Voice>>>,
}

impl CommandEngine {
    pub fn new(cfg: CommandEngineConfig) -> Self {
        let name = if cfg.name.is_empty() {
            cfg.id.clone()
        } else {
            cfg.name.clone()
        };
        Self {
            cfg,
            name,
            server: Mutex::new(None),
            reported_voices: Mutex::new(None),
        }
    }

    fn configured_voices(&self) -> Vec<Voice> {
        self.cfg
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
            .collect()
    }

    /// Send one request to the persistent program, starting it if needed.
    fn ask(&self, request: &ServerRequest) -> Result<ServerResponse> {
        let mut guard = self.server.lock().unwrap();
        if guard.as_mut().is_some_and(|s| !s.alive()) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(Server::start(&self.cfg).with_context(|| {
                format!("could not start {} (`{}`)", self.name, self.cfg.program)
            })?);
        }
        let server = guard.as_mut().expect("just started");
        match server.ask(request) {
            Ok(response) => Ok(response),
            Err(err) => {
                // Whatever went wrong, start from a clean slate next time.
                let detail = server.stderr_tail();
                *guard = None;
                if detail.is_empty() {
                    Err(err.context(format!("{} stopped answering", self.name)))
                } else {
                    Err(err.context(format!("{} failed: {detail}", self.name)))
                }
            }
        }
    }

    fn synthesize_persistent(&self, req: &SynthRequest) -> Result<Audio> {
        let output = temp_path("wav");
        let response = self.ask(&ServerRequest {
            text: Some(&req.text),
            voice: req.voice.as_deref(),
            rate: Some(req.rate),
            pitch: Some(req.pitch),
            volume: Some(req.volume),
            output: Some(output.to_string_lossy().into_owned()),
            voices: None,
        });
        let result = response.and_then(|response| {
            if !response.ok {
                bail!(
                    "{}: {}",
                    self.name,
                    response.error.unwrap_or_else(|| "synthesis failed".into())
                );
            }
            let bytes = std::fs::read(&output)
                .with_context(|| format!("{} did not write its output file", self.name))?;
            wav::parse(&bytes).with_context(|| format!("{} produced unreadable audio", self.name))
        });
        let _ = std::fs::remove_file(&output);
        result
    }
}

#[derive(Serialize)]
struct ServerRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pitch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voices: Option<bool>,
}

#[derive(Deserialize)]
struct ServerResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    voices: Vec<ReportedVoice>,
}

#[derive(Deserialize)]
struct ReportedVoice {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    language: String,
}

/// A running persistent engine program.
struct Server {
    child: Child,
    stdin: ChildStdin,
    /// JSON lines from the program's stdout.
    lines: flume::Receiver<String>,
    /// The last few lines of its stderr, for error messages.
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Server {
    fn start(cfg: &CommandEngineConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.program);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let child_stderr = child.stderr.take().expect("stderr was piped");

        let (tx, lines) = flume::unbounded();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // Libraries print progress to stdout; only JSON is for us.
                if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let tail = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(child_stderr).lines() {
                let Ok(line) = line else { break };
                log::debug!("engine: {line}");
                let mut tail = tail.lock().unwrap();
                if tail.len() >= 8 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
            stderr,
        })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn ask(&mut self, request: &ServerRequest) -> Result<ServerResponse> {
        // Answers to earlier, abandoned requests must not be mistaken for ours.
        while self.lines.try_recv().is_ok() {}
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .context("the engine program closed its input")?;
        let answer = self
            .lines
            .recv_timeout(PERSISTENT_TIMEOUT)
            .map_err(|e| match e {
                flume::RecvTimeoutError::Timeout => anyhow!("no answer in time"),
                flume::RecvTimeoutError::Disconnected => anyhow!("the engine program exited"),
            })?;
        serde_json::from_str(&answer).context("the engine program sent an invalid answer")
    }

    fn stderr_tail(&self) -> String {
        // Give the stderr thread a moment to catch the dying words.
        std::thread::sleep(Duration::from_millis(50));
        let tail = self.stderr.lock().unwrap();
        tail.iter()
            .rev()
            .take(3)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        // Voices listed in the settings always win.
        if !self.cfg.voices.is_empty() || !self.cfg.persistent {
            return Ok(self.configured_voices());
        }
        if let Some(voices) = self.reported_voices.lock().unwrap().clone() {
            return Ok(voices);
        }
        let response = self.ask(&ServerRequest {
            text: None,
            voice: None,
            rate: None,
            pitch: None,
            volume: None,
            output: None,
            voices: Some(true),
        })?;
        let voices: Vec<Voice> = response
            .voices
            .into_iter()
            .map(|v| Voice {
                name: if v.name.is_empty() {
                    v.id.clone()
                } else {
                    v.name
                },
                id: v.id,
                language: v.language,
            })
            .collect();
        *self.reported_voices.lock().unwrap() = Some(voices.clone());
        Ok(voices)
    }

    fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
        if self.cfg.persistent {
            return self.synthesize_persistent(req);
        }
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
            persistent: false,
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
            persistent: false,
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
            persistent: false,
        });
        assert!(engine.synthesize(&SynthRequest::new("x")).is_err());
    }
}

#[cfg(all(test, unix))]
mod persistent_tests {
    use super::*;

    /// A stand-in for a neural engine: answers the protocol with a fixed tone.
    const FAKE_SERVER: &str = r#"
import json, struct, sys
print("loading model... (noise on stdout must be ignored)", flush=True)
count = 0
for line in sys.stdin:
    req = json.loads(line)
    if req.get("voices"):
        print(json.dumps({"ok": True, "voices": [{"id": "a", "name": "Voice A", "language": "en"}, {"id": "b"}]}), flush=True)
        continue
    if "crash" in req["text"]:
        sys.stderr.write("model exploded\n"); sys.stderr.flush(); sys.exit(3)
    if "fail" in req["text"]:
        print(json.dumps({"ok": False, "error": "cannot say that"}), flush=True)
        continue
    count += 1
    n = int(800 / req["rate"]) + (100 if req.get("voice") == "b" else 0)
    data = struct.pack("<%dh" % n, *([1000] * n))
    with open(req["output"], "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, 8000, 16000, 2, 16) + b"data" + struct.pack("<I", len(data)) + data)
    print(json.dumps({"ok": True, "served": count}), flush=True)
"#;

    fn fake_engine() -> Option<(CommandEngine, std::path::PathBuf)> {
        let python = which::which("python3").ok()?;
        let script = temp_path("py");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        let engine = CommandEngine::new(CommandEngineConfig {
            id: "fake".into(),
            name: "Fake".into(),
            program: python.to_string_lossy().into_owned(),
            args: vec![script.to_string_lossy().into_owned()],
            voices: vec![],
            base_wpm: 175.0,
            raw_sample_rate: None,
            persistent: true,
        });
        Some((engine, script))
    }

    #[test]
    fn persistent_protocol() {
        let Some((engine, script)) = fake_engine() else {
            eprintln!("python3 not installed; skipping");
            return;
        };
        // Voices come from the program.
        let voices = engine.voices().unwrap();
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].name, "Voice A");
        assert_eq!(voices[1].name, "b");

        // Several requests reuse one process; rate and voice are passed on.
        let a = engine.synthesize(&SynthRequest::new("one <two>")).unwrap();
        assert_eq!((a.sample_rate, a.samples.len()), (8000, 800));
        let mut req = SynthRequest::new("three");
        req.rate = 2.0;
        req.voice = Some("b".into());
        assert_eq!(engine.synthesize(&req).unwrap().samples.len(), 500);

        // A refusal is an error, and the process lives on.
        let err = engine.synthesize(&SynthRequest::new("fail")).unwrap_err();
        assert!(format!("{err:#}").contains("cannot say that"));
        assert!(engine.synthesize(&SynthRequest::new("again")).is_ok());

        // A crash is reported with the program's last words, then recovered from.
        let err = engine.synthesize(&SynthRequest::new("crash")).unwrap_err();
        assert!(format!("{err:#}").contains("model exploded"), "{err:#}");
        assert!(engine.synthesize(&SynthRequest::new("back")).is_ok());

        std::fs::remove_file(script).unwrap();
    }
}
