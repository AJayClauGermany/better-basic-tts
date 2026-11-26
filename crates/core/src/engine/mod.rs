//! Speech engines.
//!
//! An [`Engine`] turns one piece of text into PCM audio, locally. The player
//! does the playback itself, which is what makes pause / skip / speed behave
//! identically on every platform and with every engine.
//!
//! Built-in engines:
//!
//! | id        | platform        | backend                                        |
//! |-----------|-----------------|------------------------------------------------|
//! | `say`     | macOS           | the system voices, through `/usr/bin/say`      |
//! | `winrt`   | Windows 10+     | the system voices, `Windows.Media.SpeechSynthesis` |
//! | `espeak`  | any (Linux default) | `espeak-ng` if installed                   |
//! | custom    | any             | any local command that produces a WAV file (Piper, KittenTTS, NeuTTS, ...) |

#[cfg(target_os = "macos")]
pub mod av;
mod command;
mod espeak;
mod say;
pub mod wav;
#[cfg(windows)]
mod winrt;

pub use command::{CommandEngine, CommandEngineConfig};
pub use espeak::EspeakEngine;
pub use say::SayEngine;
#[cfg(windows)]
pub use winrt::WinRtEngine;

use anyhow::Result;
use std::sync::Arc;

/// Mono PCM audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn silence(sample_rate: u32, millis: u32) -> Self {
        let n = (sample_rate as u64 * millis as u64 / 1000) as usize;
        Self {
            sample_rate,
            samples: vec![0.0; n],
        }
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f64 / self.sample_rate as f64
        }
    }

    /// Remove leading / trailing near-silence beyond `keep_ms`, so that the
    /// gaps between sentences are decided by us and not by the engine.
    pub fn trim_silence(&mut self, keep_ms: u32) {
        const THRESHOLD: f32 = 0.004;
        let keep = (self.sample_rate as u64 * keep_ms as u64 / 1000) as usize;
        let first = self.samples.iter().position(|s| s.abs() > THRESHOLD);
        let Some(first) = first else {
            return;
        };
        let last = self
            .samples
            .iter()
            .rposition(|s| s.abs() > THRESHOLD)
            .unwrap_or(self.samples.len() - 1);
        let start = first.saturating_sub(keep);
        let end = (last + 1 + keep).min(self.samples.len());
        if start > 0 || end < self.samples.len() {
            self.samples = self.samples[start..end].to_vec();
        }
    }

    /// Cut the audio into `parts` pieces at the `parts - 1` longest pauses.
    ///
    /// Used to take apart several sentences rendered in one go with a long
    /// silence between them. Returns `None` unless the cuts are unambiguous:
    /// each must be a real pause, and clearly longer than any other pause in
    /// the audio (such as one after a comma).
    pub fn split_at_pauses(&self, parts: usize) -> Option<Vec<Audio>> {
        const THRESHOLD: f32 = 0.004;
        if parts == 0 || self.sample_rate == 0 {
            return None;
        }
        if parts == 1 {
            return Some(vec![self.clone()]);
        }
        let first = self.samples.iter().position(|s| s.abs() > THRESHOLD)?;
        let last = self.samples.iter().rposition(|s| s.abs() > THRESHOLD)?;

        // Silent runs strictly inside the spoken part: (start, length).
        let min_run = self.sample_rate as usize / 10;
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut run_start = None;
        for (i, sample) in self.samples.iter().enumerate().take(last + 1).skip(first) {
            let silent = sample.abs() <= THRESHOLD;
            match (silent, run_start) {
                (true, None) => run_start = Some(i),
                (false, Some(start)) => {
                    if i - start >= min_run {
                        runs.push((start, i - start));
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        if runs.len() < parts - 1 {
            return None;
        }
        runs.sort_by_key(|&(_, len)| std::cmp::Reverse(len));
        let shortest_cut = runs[parts - 2].1;
        let longest_other = runs.get(parts - 1).map(|r| r.1).unwrap_or(0);
        let min_cut = self.sample_rate as usize * 7 / 10;
        if shortest_cut < min_cut || shortest_cut < longest_other * 2 {
            return None;
        }
        let mut cuts: Vec<(usize, usize)> = runs[..parts - 1].to_vec();
        cuts.sort_by_key(|&(start, _)| start);

        let mut pieces = Vec::with_capacity(parts);
        let mut begin = first;
        for (start, len) in cuts {
            pieces.push(Audio {
                sample_rate: self.sample_rate,
                samples: self.samples[begin..start].to_vec(),
            });
            begin = start + len;
        }
        pieces.push(Audio {
            sample_rate: self.sample_rate,
            samples: self.samples[begin..=last].to_vec(),
        });
        Some(pieces)
    }

    pub fn append_silence(&mut self, millis: u32) {
        let n = (self.sample_rate as u64 * millis as u64 / 1000) as usize;
        self.samples.extend(std::iter::repeat_n(0.0, n));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// Stable identifier, stored in the settings.
    pub id: String,
    /// Human readable name.
    pub name: String,
    /// BCP-47-ish language tag, may be empty.
    pub language: String,
}

impl Voice {
    pub fn label(&self) -> String {
        if self.language.is_empty() || self.name.contains(&self.language) {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.language)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SynthRequest {
    /// Plain text. Engines must never interpret it as markup.
    pub text: String,
    /// Voice id from [`Engine::voices`]; `None` means the engine default.
    pub voice: Option<String>,
    /// Speed multiplier, 1.0 is the voice's normal speed.
    pub rate: f32,
    /// Pitch multiplier, 1.0 is the voice's normal pitch.
    pub pitch: f32,
    /// 0.0 ..= 1.0
    pub volume: f32,
}

impl SynthRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: None,
            rate: 1.0,
            pitch: 1.0,
            volume: 1.0,
        }
    }
}

/// A local speech engine. Implementations must be callable from any thread.
pub trait Engine: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn voices(&self) -> Result<Vec<Voice>>;
    fn synthesize(&self, req: &SynthRequest) -> Result<Audio>;

    /// How many consecutive sentences the player may hand over in one call
    /// to [`Engine::synthesize_batch`]. Engines with a high fixed cost per
    /// call (macOS `say` needs about a second to get going, however short
    /// the text) return more than 1.
    fn batch_size(&self) -> usize {
        1
    }

    /// Synthesize several texts that share voice, rate, pitch and volume
    /// (taken from `params`; its `text` is ignored). Returns one [`Audio`]
    /// per text, in order.
    fn synthesize_batch(&self, texts: &[String], params: &SynthRequest) -> Result<Vec<Audio>> {
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

/// Tell the engine layer that the main thread is running its event loop
/// (the GUI does). Only matters on macOS, where the native speech API
/// delivers audio through that loop.
pub fn set_main_loop_serviced(serviced: bool) {
    #[cfg(target_os = "macos")]
    av::set_main_loop_serviced(serviced);
    #[cfg(not(target_os = "macos"))]
    let _ = serviced;
}

/// Run `work` so that engines needing the main event loop can function when
/// no UI is running it: on macOS the work moves to another thread while the
/// calling (main) thread services its run loop. Elsewhere it just runs.
pub fn run_with_main_loop<R: Send>(work: impl FnOnce() -> R + Send) -> R {
    #[cfg(target_os = "macos")]
    {
        av::run_with_main_loop(work)
    }
    #[cfg(not(target_os = "macos"))]
    {
        work()
    }
}

/// Print how the native macOS speech API behaves on this machine.
pub fn probe_native_speech() {
    #[cfg(target_os = "macos")]
    av::probe();
    #[cfg(not(target_os = "macos"))]
    println!("The native speech probe only exists on macOS.");
}

/// All engines available on this machine.
#[derive(Clone, Default)]
pub struct EngineRegistry {
    engines: Vec<Arc<dyn Engine>>,
}

impl EngineRegistry {
    /// Detect the built-in engines and add the user's command engines.
    pub fn detect(custom: &[CommandEngineConfig]) -> Self {
        let mut engines: Vec<Arc<dyn Engine>> = Vec::new();

        #[cfg(target_os = "macos")]
        if let Some(e) = SayEngine::detect() {
            engines.push(Arc::new(e));
        }
        // Experimental, so after `say`: it has to be chosen explicitly.
        #[cfg(target_os = "macos")]
        if let Some(e) = av::AvEngine::detect() {
            engines.push(Arc::new(e));
        }
        #[cfg(windows)]
        if let Some(e) = WinRtEngine::detect() {
            engines.push(Arc::new(e));
        }
        // User engines come before espeak so that on Linux a configured
        // neural voice wins over the robotic fallback when engine = "auto".
        for cfg in custom {
            if cfg.id.is_empty() || engines.iter().any(|e| e.id() == cfg.id) {
                log::warn!(
                    "ignoring custom engine with empty or duplicate id {:?}",
                    cfg.id
                );
                continue;
            }
            engines.push(Arc::new(CommandEngine::new(cfg.clone())));
        }
        if let Some(e) = EspeakEngine::detect() {
            engines.push(Arc::new(e));
        }
        Self { engines }
    }

    pub fn from_engines(engines: Vec<Arc<dyn Engine>>) -> Self {
        Self { engines }
    }

    pub fn engines(&self) -> &[Arc<dyn Engine>] {
        &self.engines
    }

    pub fn is_empty(&self) -> bool {
        self.engines.is_empty()
    }

    /// Resolve an engine id from the settings. `""` and `"auto"` pick the
    /// first (most native) engine.
    pub fn resolve(&self, id: &str) -> Option<Arc<dyn Engine>> {
        if id.is_empty() || id == "auto" {
            return self.engines.first().cloned();
        }
        self.engines
            .iter()
            .find(|e| e.id() == id)
            .or_else(|| self.engines.first())
            .cloned()
    }

    /// What to tell the user when no engine was found.
    pub fn missing_engine_help() -> &'static str {
        if cfg!(target_os = "linux") {
            "No speech engine found. Install espeak-ng, or add a local engine in the settings file."
        } else if cfg!(target_os = "macos") {
            "The system speech command /usr/bin/say was not found."
        } else {
            "No speech engine found. Windows 10 or later with at least one installed voice is required."
        }
    }
}

/// Create a unique temp file path with the given extension.
pub(crate) fn temp_path(ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "bbtts-{}-{}-{:x}.{}",
        std::process::id(),
        n,
        nanos,
        ext
    ))
}

/// Run a command, feed `stdin`, return stdout. Never goes through a shell.
pub(crate) fn run_command(mut cmd: std::process::Command, stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    use anyhow::{bail, Context};
    use std::io::Write;
    use std::process::Stdio;

    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start `{program}`"))?;
    let writer = stdin.map(|data| {
        let mut pipe = child.stdin.take().expect("stdin was piped");
        let data = data.to_vec();
        // Write on another thread so a full stdout pipe cannot deadlock us.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        })
    });
    let output = child
        .wait_with_output()
        .with_context(|| format!("`{program}` failed"))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`{program}` exited with {}: {}",
            output.status,
            stderr.trim().chars().take(400).collect::<String>()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `speech` seconds of signal with pauses of the given lengths between.
    fn spoken(sample_rate: u32, parts: &[(f32, f32)]) -> Audio {
        let mut samples = vec![0.0; sample_rate as usize / 5];
        for &(speech, pause) in parts {
            samples.extend(std::iter::repeat_n(
                0.5,
                (speech * sample_rate as f32) as usize,
            ));
            samples.extend(std::iter::repeat_n(
                0.0,
                (pause * sample_rate as f32) as usize,
            ));
        }
        Audio {
            sample_rate,
            samples,
        }
    }

    #[test]
    fn splits_at_the_long_pauses_only() {
        // Three sentences separated by 2 s; a 0.4 s comma pause inside the second.
        let audio = spoken(1000, &[(1.0, 2.0), (0.5, 0.4), (0.7, 2.0), (1.5, 0.3)]);
        let parts = audio.split_at_pauses(3).unwrap();
        let lengths: Vec<usize> = parts.iter().map(|p| p.samples.len()).collect();
        assert_eq!(lengths, vec![1000, 1600, 1500]);
        // Asking for a split that is not there is refused, not guessed.
        assert!(audio.split_at_pauses(5).is_none());
        assert!(spoken(1000, &[(1.0, 0.5), (1.0, 0.45), (1.0, 0.0)])
            .split_at_pauses(3)
            .is_none());
        assert!(Audio::silence(1000, 500).split_at_pauses(2).is_none());
        assert_eq!(audio.split_at_pauses(1).unwrap().len(), 1);
    }

    #[test]
    fn default_batch_is_one_call_per_text() {
        struct Echo;
        impl Engine for Echo {
            fn id(&self) -> &str {
                "echo"
            }
            fn name(&self) -> &str {
                "Echo"
            }
            fn voices(&self) -> Result<Vec<Voice>> {
                Ok(vec![])
            }
            fn synthesize(&self, req: &SynthRequest) -> Result<Audio> {
                Ok(Audio {
                    sample_rate: 1000,
                    samples: vec![req.rate; req.text.len()],
                })
            }
        }
        let mut params = SynthRequest::new("");
        params.rate = 2.0;
        let out = Echo
            .synthesize_batch(&["ab".to_string(), "abcd".to_string()], &params)
            .unwrap();
        assert_eq!(
            out.iter().map(|a| a.samples.len()).collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert_eq!(out[0].samples[0], 2.0);
        assert_eq!(Echo.batch_size(), 1);
    }

    #[test]
    fn trim_silence_keeps_margin() {
        let mut a = Audio {
            sample_rate: 1000,
            samples: [vec![0.0; 500], vec![0.5; 100], vec![0.0; 500]].concat(),
        };
        a.trim_silence(20);
        assert_eq!(a.samples.len(), 140);
        let mut silent = Audio::silence(1000, 100);
        silent.trim_silence(10);
        assert_eq!(silent.samples.len(), 100);
    }
}
