//! User settings, stored as a TOML file the user can also edit by hand.

use crate::engine::CommandEngineConfig;
use crate::text::{
    self, AngleBrackets, InterpreterKind, Role, SpeakableOptions, Substitution, TextOptions,
    UrlMode, Utterance,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MIN_RATE: f32 = 0.5;
pub const MAX_RATE: f32 = 4.0;
pub const RATE_STEP: f32 = 0.1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Engine id, or "auto" for the platform's native voices.
    pub engine: String,
    /// Voice id within the engine; empty for the engine's default voice.
    pub voice: String,
    /// Speed multiplier. The turtle and rabbit buttons change this.
    pub rate: f32,
    /// 0.0 ..= 1.0
    pub volume: f32,
    /// How text is interpreted (plain, Markdown, auto-detect).
    pub interpreter: InterpreterKind,
    /// How `<` and `>` are read.
    pub angle_brackets: AngleBrackets,
    /// How URLs are read.
    pub urls: UrlMode,
    /// Silence between sentences, in milliseconds.
    pub sentence_pause_ms: u32,
    /// Keep the control window above other windows.
    pub always_on_top: bool,
    pub shortcuts: Shortcuts,
    /// Per-role voices and pacing ("read headings with another voice").
    pub roles: RoleStyles,
    /// Find / replace rules applied before speaking.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub substitutions: Vec<Substitution>,
    /// Additional local engines (Piper, KittenTTS, NeuTTS, ...).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<CommandEngineConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: "auto".into(),
            voice: String::new(),
            rate: 1.0,
            volume: 1.0,
            interpreter: InterpreterKind::Auto,
            angle_brackets: AngleBrackets::Smart,
            urls: UrlMode::Domain,
            sentence_pause_ms: 90,
            always_on_top: true,
            shortcuts: Shortcuts::default(),
            roles: RoleStyles::default(),
            substitutions: Vec::new(),
            engines: Vec::new(),
        }
    }
}

/// Global keyboard shortcuts. Empty strings mean "not bound".
///
/// Syntax: modifiers joined with `+`, then a key, e.g. `Ctrl+Alt+S`,
/// `Cmd+Shift+S`, `Alt+Escape`. `CmdOrCtrl` is Command on macOS and Control
/// elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    /// Read the current selection; if something is being read, stop instead.
    pub speak_selection: String,
    /// Read the clipboard.
    pub speak_clipboard: String,
    pub pause_resume: String,
    pub stop: String,
    pub previous: String,
    pub next: String,
    pub slower: String,
    pub faster: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            speak_selection: "Ctrl+Alt+S".into(),
            speak_clipboard: String::new(),
            pause_resume: "Ctrl+Alt+P".into(),
            stop: String::new(),
            previous: String::new(),
            next: String::new(),
            slower: String::new(),
            faster: String::new(),
        }
    }
}

/// How one [`Role`] is spoken, relative to the main voice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleStyle {
    /// Voice id; empty to use the main voice.
    pub voice: String,
    /// Multiplied with the main rate.
    pub rate: f32,
    /// Pitch multiplier (engine support varies).
    pub pitch: f32,
    /// Multiplied with the main volume.
    pub volume: f32,
    /// Words spoken before each block of this role, e.g. "Quote:".
    pub announce: String,
    /// Silence after a block of this role, in milliseconds.
    pub pause_after_ms: u32,
    /// Do not read blocks of this role at all.
    pub skip: bool,
}

impl Default for RoleStyle {
    fn default() -> Self {
        Self {
            voice: String::new(),
            rate: 1.0,
            pitch: 1.0,
            volume: 1.0,
            announce: String::new(),
            pause_after_ms: 320,
            skip: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleStyles {
    pub paragraph: RoleStyle,
    pub heading: RoleStyle,
    pub quote: RoleStyle,
    pub code: RoleStyle,
    pub list_item: RoleStyle,
}

impl Default for RoleStyles {
    fn default() -> Self {
        Self {
            paragraph: RoleStyle::default(),
            heading: RoleStyle {
                pause_after_ms: 480,
                ..RoleStyle::default()
            },
            quote: RoleStyle::default(),
            code: RoleStyle::default(),
            list_item: RoleStyle {
                pause_after_ms: 220,
                ..RoleStyle::default()
            },
        }
    }
}

impl RoleStyles {
    pub fn get(&self, role: Role) -> &RoleStyle {
        match role {
            Role::Paragraph => &self.paragraph,
            Role::Heading => &self.heading,
            Role::Quote => &self.quote,
            Role::Code => &self.code,
            Role::ListItem => &self.list_item,
        }
    }

    pub fn get_mut(&mut self, role: Role) -> &mut RoleStyle {
        match role {
            Role::Paragraph => &mut self.paragraph,
            Role::Heading => &mut self.heading,
            Role::Quote => &mut self.quote,
            Role::Code => &mut self.code,
            Role::ListItem => &mut self.list_item,
        }
    }
}

pub fn clamp_rate(rate: f32) -> f32 {
    if !rate.is_finite() {
        return 1.0;
    }
    // Snap to the step grid so repeated clicks do not accumulate float error.
    let snapped = (rate / RATE_STEP).round() / (1.0 / RATE_STEP).round();
    snapped.clamp(MIN_RATE, MAX_RATE)
}

impl Settings {
    /// `<config dir>/better-basic-tts/settings.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("better-basic-tts")
            .join("settings.toml")
    }

    /// Load settings. A missing file yields the defaults; a broken file is an
    /// error (the caller should fall back to defaults *without* overwriting
    /// the user's file).
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        };
        let mut settings: Settings =
            toml::from_str(&text).with_context(|| format!("could not parse {}", path.display()))?;
        settings.normalize();
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        let body = toml::to_string_pretty(self).context("could not serialize settings")?;
        let text = format!("{HEADER}{body}");
        // Write-then-rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    /// Bring hand-edited values back into their valid ranges.
    pub fn normalize(&mut self) {
        self.rate = clamp_rate(self.rate);
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.sentence_pause_ms = self.sentence_pause_ms.min(5_000);
        for role in Role::ALL {
            let style = self.roles.get_mut(role);
            style.rate = finite_or(style.rate, 1.0).clamp(0.25, 4.0);
            style.pitch = finite_or(style.pitch, 1.0).clamp(0.5, 2.0);
            style.volume = finite_or(style.volume, 1.0).clamp(0.0, 2.0);
            style.pause_after_ms = style.pause_after_ms.min(10_000);
        }
    }

    pub fn text_options(&self) -> TextOptions {
        TextOptions {
            interpreter: self.interpreter,
            speakable: SpeakableOptions {
                angle_brackets: self.angle_brackets,
                urls: self.urls,
                substitutions: self.substitutions.clone(),
            },
        }
    }

    /// Run the text pipeline with these settings, dropping skipped roles.
    pub fn prepare(&self, raw: &str) -> Vec<Utterance> {
        let mut utterances = text::prepare(raw, &self.text_options());
        utterances.retain(|u| !self.roles.get(u.role).skip);
        utterances
    }
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

const HEADER: &str = "\
# Better Basic TTS settings.
#
# Everything here can also be changed from the settings panel, except
# `substitutions` and `engines`, which are documented in the README.
# Delete this file to go back to the defaults.

";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());

        let mut s = Settings::default();
        s.rate = 1.7;
        s.voice = "Alex".into();
        s.roles.heading.voice = "Samantha".into();
        s.roles.code.skip = true;
        s.substitutions.push(Substitution {
            pattern: "TTS".into(),
            replacement: "text to speech".into(),
            regex: false,
            case_sensitive: true,
            roles: vec![Role::Heading],
        });
        s.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn partial_file_and_clamping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "rate = 99.0\n[roles.quote]\nannounce = \"Quote:\"\n").unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.rate, MAX_RATE);
        assert_eq!(s.roles.quote.announce, "Quote:");
        assert_eq!(s.roles.quote.rate, 1.0);
        assert_eq!(s.engine, "auto");
    }

    #[test]
    fn broken_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "rate = [not toml").unwrap();
        assert!(Settings::load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "rate = [not toml");
    }

    #[test]
    fn skip_roles() {
        let mut s = Settings::default();
        s.interpreter = InterpreterKind::Markdown;
        s.roles.code.skip = true;
        let u = s.prepare("Text.\n\n```\ncode here\n```\n\nMore.");
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn rate_snaps_to_grid() {
        assert_eq!(clamp_rate(1.0 + 0.1 + 0.1 + 0.1), 1.3);
        assert_eq!(clamp_rate(0.0), MIN_RATE);
        assert_eq!(clamp_rate(f32::NAN), 1.0);
    }
}
