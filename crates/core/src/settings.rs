//! User settings, stored as a TOML file the user can also edit by hand.

use crate::engine::CommandEngineConfig;
use crate::text::{
    self, AngleBrackets, Announcement, InterpreterKind, InterpreterPlugin, MarkdownOptions,
    Position, Prepared, Role, SpeakableOptions, Substitution, TableOrder, TextOptions, UrlMode,
    Utterance,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Light or dark windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Same as system",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

pub const MIN_RATE: f32 = 0.5;
pub const MAX_RATE: f32 = 4.0;
/// What one press of the turtle or the rabbit changes.
pub const RATE_STEP: f32 = 0.25;
/// Speeds are kept to this precision.
const RATE_PRECISION: f32 = 0.05;

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
    /// How text is interpreted: "auto", "plain", "markdown", or the id of
    /// one of the `interpreters` plugins.
    pub interpreter: String,
    /// Whether tables are read column by column or row by row.
    pub tables: TableOrder,
    /// How `<` and `>` are read.
    pub angle_brackets: AngleBrackets,
    /// How URLs are read.
    pub urls: UrlMode,
    /// Silence between sentences, in milliseconds.
    pub sentence_pause_ms: u32,
    /// Keep the control window above other windows.
    pub always_on_top: bool,
    /// Light or dark windows.
    pub appearance: Appearance,
    pub shortcuts: Shortcuts,
    /// Per-role voices, pacing and announcements ("read headings with
    /// another voice"). Keys are role names; `code.python` inherits what it
    /// does not set from `code`.
    #[serde(skip_serializing_if = "RoleStyles::is_empty")]
    pub roles: RoleStyles,
    /// Programs that change how text is turned into roles.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interpreters: Vec<InterpreterPlugin>,
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
            interpreter: InterpreterKind::Auto.key().into(),
            tables: TableOrder::Columns,
            angle_brackets: AngleBrackets::Smart,
            urls: UrlMode::Domain,
            sentence_pause_ms: 90,
            always_on_top: true,
            appearance: Appearance::System,
            shortcuts: Shortcuts::default(),
            roles: RoleStyles::default(),
            interpreters: Vec::new(),
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

/// How one [`Role`] is spoken, relative to the main voice. This is the
/// *resolved* style; what the user has set is a [`RoleOverride`].
#[derive(Debug, Clone, PartialEq)]
pub struct RoleStyle {
    /// Voice id; empty to use the main voice.
    pub voice: String,
    /// Multiplied with the main rate.
    pub rate: f32,
    /// Pitch multiplier (engine support varies).
    pub pitch: f32,
    /// Multiplied with the main volume.
    pub volume: f32,
    /// Words spoken before each block of this role, e.g. "Point".
    pub announce: String,
    /// Words spoken after each block of this role.
    pub announce_after: String,
    /// Words spoken once before a run of blocks of this role begins, e.g.
    /// "List" before the first item of a list.
    pub open: String,
    /// Words spoken once after a run of blocks of this role, e.g.
    /// "End of list".
    pub close: String,
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
            announce_after: String::new(),
            open: String::new(),
            close: String::new(),
            pause_after_ms: 320,
            skip: false,
        }
    }
}

impl RoleStyle {
    /// What a role is like before the user changes anything.
    pub fn built_in(root: &str) -> Self {
        let pause_after_ms = match root {
            Role::HEADING => 480,
            Role::LIST_ITEM => 220,
            // Cells are short; the column heading before them gets a beat.
            Role::TABLE => 260,
            _ => 320,
        };
        Self {
            pause_after_ms,
            ..Self::default()
        }
    }
}

/// What the user has set for one role. Anything left out is inherited from
/// the more general role (`code.python` → `code`) and finally from the
/// built-in defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announce_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<bool>,
}

impl RoleOverride {
    fn apply_to(&self, style: &mut RoleStyle) {
        macro_rules! take {
            ($($field:ident),*) => {$(
                if let Some(value) = &self.$field {
                    style.$field = value.clone();
                }
            )*};
        }
        take!(
            voice,
            rate,
            pitch,
            volume,
            announce,
            announce_after,
            open,
            close,
            pause_after_ms,
            skip
        );
    }

    /// Forget every setting that equals what would be inherited anyway.
    fn prune(&mut self, inherited: &RoleStyle) {
        macro_rules! drop_same {
            ($($field:ident),*) => {$(
                if self.$field.as_ref() == Some(&inherited.$field) {
                    self.$field = None;
                }
            )*};
        }
        drop_same!(
            voice,
            rate,
            pitch,
            volume,
            announce,
            announce_after,
            open,
            close,
            pause_after_ms,
            skip
        );
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The user's per-role settings, by role name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleStyles(BTreeMap<String, RoleOverride>);

impl RoleStyles {
    /// The style a role is spoken with: built-in defaults, then what is set
    /// for the general role, then for each more specific one.
    pub fn get(&self, role: &Role) -> RoleStyle {
        let mut style = RoleStyle::built_in(role.root());
        for name in role.lineage() {
            if let Some(over) = self.0.get(name) {
                over.apply_to(&mut style);
            }
        }
        style
    }

    /// What `role` would be like without its own entry.
    fn inherited(&self, role: &Role) -> RoleStyle {
        let mut style = RoleStyle::built_in(role.root());
        for name in role.lineage() {
            if name == role.as_str() {
                break;
            }
            if let Some(over) = self.0.get(name) {
                over.apply_to(&mut style);
            }
        }
        style
    }

    /// The user's settings for a role, created on first use.
    pub fn entry(&mut self, role: &Role) -> &mut RoleOverride {
        self.0.entry(role.as_str().to_string()).or_default()
    }

    /// Roles to offer in the settings: the built-in ones, then every other
    /// role the user has configured.
    pub fn names(&self) -> Vec<Role> {
        let mut names: Vec<Role> = Role::BUILT_IN.iter().map(|n| Role::new(n)).collect();
        for name in self.0.keys() {
            let role = Role::new(name);
            if !names.contains(&role) {
                names.push(role);
            }
        }
        names
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forget all per-role voices (their ids mean nothing to another engine).
    pub fn clear_voices(&mut self) {
        for over in self.0.values_mut() {
            over.voice = None;
        }
    }

    fn normalize(&mut self) {
        // Role names are normalised the same way everywhere.
        let entries = std::mem::take(&mut self.0);
        for (name, mut over) in entries {
            over.rate = over.rate.map(|v| finite_or(v, 1.0).clamp(0.25, 4.0));
            over.pitch = over.pitch.map(|v| finite_or(v, 1.0).clamp(0.5, 2.0));
            over.volume = over.volume.map(|v| finite_or(v, 1.0).clamp(0.0, 2.0));
            over.pause_after_ms = over.pause_after_ms.map(|v| v.min(10_000));
            self.0.insert(Role::new(&name).as_str().to_string(), over);
        }
        // Keep the file to what actually differs. General roles first, so a
        // specific role is compared with its already tidy parent.
        let names: Vec<String> = self.0.keys().cloned().collect();
        for name in names {
            let role = Role::new(&name);
            let inherited = self.inherited(&role);
            let over = self.0.get_mut(&name).expect("just listed");
            over.prune(&inherited);
            let built_in = Role::BUILT_IN.contains(&name.as_str());
            if over.is_empty() && built_in {
                self.0.remove(&name);
            }
        }
    }
}

pub fn clamp_rate(rate: f32) -> f32 {
    if !rate.is_finite() {
        return 1.0;
    }
    // Snap to a fine grid so repeated changes do not accumulate float error.
    let snapped = (rate / RATE_PRECISION).round() / (1.0 / RATE_PRECISION).round();
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
        self.roles.normalize();
        self.interpreter = self.interpreter.trim().to_string();
        if self.interpreter.is_empty() {
            self.interpreter = InterpreterKind::Auto.key().into();
        }
    }

    /// The plugin selected by `interpreter`, if it names one.
    pub fn interpreter_plugin(&self) -> Option<&InterpreterPlugin> {
        if InterpreterKind::from_key(&self.interpreter).is_some() {
            return None;
        }
        self.interpreters.iter().find(|p| p.id == self.interpreter)
    }

    pub fn text_options(&self) -> TextOptions {
        TextOptions {
            interpreter: InterpreterKind::from_key(&self.interpreter).unwrap_or_default(),
            markdown: MarkdownOptions {
                tables: self.tables,
            },
            plugin: self.interpreter_plugin().cloned(),
            speakable: SpeakableOptions {
                angle_brackets: self.angle_brackets,
                urls: self.urls,
                substitutions: self.substitutions.clone(),
            },
        }
    }

    /// Run the text pipeline with these settings: skipped roles are left
    /// out, announcements are added.
    pub fn prepare(&self, raw: &str) -> Vec<Utterance> {
        self.prepare_report(raw).utterances
    }

    /// Like [`Settings::prepare`], also returning anything the user should
    /// be told (a plugin that failed, an interpreter that does not exist).
    pub fn prepare_report(&self, raw: &str) -> Prepared {
        let mut prepared = text::prepare_with(
            raw,
            &self.text_options(),
            |role| self.roles.get(role).skip,
            |role, position| self.announcement(role, position),
        );
        let known = InterpreterKind::from_key(&self.interpreter).is_some()
            || self.interpreter_plugin().is_some();
        if !known && prepared.warning.is_none() {
            prepared.warning = Some(format!(
                "There is no interpreter called \"{}\"; the text was read as usual.",
                self.interpreter
            ));
        }
        prepared
    }

    fn announcement(&self, role: &Role, position: Position) -> Announcement {
        let style = self.roles.get(role);
        let phrase = |parts: [&str; 2]| -> String {
            let mut out = String::new();
            for part in parts.into_iter().map(str::trim).filter(|p| !p.is_empty()) {
                if !out.is_empty() {
                    // A pause between "List" and "Point".
                    if !out.ends_with(['.', ',', ':', ';', '!', '?']) {
                        out.push('.');
                    }
                    out.push(' ');
                }
                out.push_str(part);
            }
            out
        };
        Announcement {
            before: phrase([
                if position.starts_run { &style.open } else { "" },
                &style.announce,
            ]),
            after: phrase([
                &style.announce_after,
                if position.ends_run { &style.close } else { "" },
            ]),
        }
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
# Most of this can also be changed from the settings window. `substitutions`,
# `engines` and `interpreters` (plugins) are documented in the README.
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

        let mut s = Settings {
            rate: 1.7,
            voice: "Alex".into(),
            interpreter: "mine".into(),
            tables: TableOrder::Rows,
            ..Default::default()
        };
        s.roles.entry(&Role::heading()).voice = Some("Samantha".into());
        s.roles.entry(&Role::new("code.python")).skip = Some(true);
        s.interpreters.push(InterpreterPlugin {
            id: "mine".into(),
            name: "Mine".into(),
            program: "python3".into(),
            args: vec!["plugin.py".into()],
            base: None,
            timeout_secs: 5,
        });
        s.substitutions.push(Substitution {
            pattern: "TTS".into(),
            replacement: "text to speech".into(),
            regex: false,
            case_sensitive: true,
            roles: vec![Role::heading()],
        });
        s.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded, s);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[roles.\"code.python\"]"), "{text}");
    }

    #[test]
    fn partial_file_and_clamping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "rate = 99.0\n[roles.quote]\nannounce = \"Quote:\"\n").unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.rate, MAX_RATE);
        let quote = s.roles.get(&Role::quote());
        assert_eq!(quote.announce, "Quote:");
        assert_eq!(quote.rate, 1.0);
        assert_eq!(s.engine, "auto");
        assert_eq!(s.interpreter, "auto");
        assert_eq!(s.tables, TableOrder::Columns);
    }

    #[test]
    fn files_from_the_first_version_still_load() {
        // Every role written out in full, as the first version did.
        let old = "interpreter = \"markdown\"\n\n[roles.paragraph]\nvoice = \"\"\nrate = 1.0\npitch = 1.0\nvolume = 1.0\nannounce = \"\"\npause_after_ms = 320\nskip = false\n\n[roles.heading]\nvoice = \"Daniel\"\nrate = 0.9\npitch = 1.0\nvolume = 1.0\nannounce = \"\"\npause_after_ms = 480\nskip = false\n\n[roles.list_item]\nvoice = \"\"\nrate = 1.0\npitch = 1.0\nvolume = 1.0\nannounce = \"point\"\npause_after_ms = 220\nskip = false\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, old).unwrap();
        let s = Settings::load(&path).unwrap();
        let heading = s.roles.get(&Role::new("heading.2"));
        assert_eq!(
            (heading.voice.as_str(), heading.rate, heading.pause_after_ms),
            ("Daniel", 0.9, 480)
        );
        assert_eq!(s.roles.get(&Role::list_item()).announce, "point");
        // Saving keeps only what differs from the defaults.
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[roles.paragraph]"), "{text}");
        assert!(text.contains("voice = \"Daniel\""));
        assert!(!text.contains("pause_after_ms"), "{text}");
        assert_eq!(Settings::load(&path).unwrap(), s);
    }

    #[test]
    fn specific_roles_inherit_from_general_ones() {
        let mut s = Settings::default();
        s.roles.entry(&Role::code()).voice = Some("Fred".into());
        s.roles.entry(&Role::code()).rate = Some(0.8);
        s.roles.entry(&Role::new("code.python")).rate = Some(1.2);
        s.roles.entry(&Role::new("code.python")).open = Some("Python".into());
        let python = s.roles.get(&Role::new("code.python"));
        assert_eq!(
            (python.voice.as_str(), python.rate, python.open.as_str()),
            ("Fred", 1.2, "Python")
        );
        let js = s.roles.get(&Role::new("code.javascript"));
        assert_eq!(
            (js.voice.as_str(), js.rate, js.open.as_str()),
            ("Fred", 0.8, "")
        );
        // Unknown roles get plain defaults, built-in ones their own.
        assert_eq!(s.roles.get(&Role::new("aside")), RoleStyle::default());
        assert_eq!(s.roles.get(&Role::new("heading.1")).pause_after_ms, 480);
        // Offered in the settings: built-ins first, then the custom ones.
        let names: Vec<String> = s.roles.names().iter().map(|r| r.to_string()).collect();
        assert_eq!(&names[..6], &Role::BUILT_IN);
        assert_eq!(&names[6..], &["code.python"]);
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
        let mut s = Settings {
            interpreter: "markdown".into(),
            ..Default::default()
        };
        s.roles.entry(&Role::code()).skip = Some(true);
        // Skipping `code` also skips `code.rust`.
        let u = s.prepare("Text.\n\n```rust\ncode here\n```\n\nMore.");
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn lists_can_be_opened_and_closed() {
        let mut s = Settings {
            interpreter: "markdown".into(),
            ..Default::default()
        };
        let list = s.roles.entry(&Role::list_item());
        list.open = Some("List".into());
        list.announce = Some("Point".into());
        list.close = Some("End of list".into());
        let spoken: Vec<String> = s
            .prepare("Shopping:\n\n- Milk\n- Eggs\n\nThanks.")
            .into_iter()
            .map(|u| u.spoken)
            .collect();
        assert_eq!(
            spoken,
            vec![
                "Shopping:",
                "List. Point. Milk",
                "Point. Eggs. End of list",
                "Thanks."
            ]
        );
    }

    #[test]
    fn tables_are_read_column_first_by_default() {
        let mut s = Settings {
            interpreter: "markdown".into(),
            ..Default::default()
        };
        let table = "| Name | Age |\n|---|---|\n| Ada | 36 |\n| Linus | 55 |\n";
        let read = |s: &Settings| -> Vec<String> {
            s.prepare(table).into_iter().map(|u| u.spoken).collect()
        };
        assert_eq!(
            read(&s),
            vec!["Name.", "Ada.", "Linus.", "Age.", "36.", "55."]
        );
        // `table` settings apply to headers and cells alike, as one run.
        s.roles.entry(&Role::new("table")).open = Some("Table".into());
        s.roles.entry(&Role::new("table")).close = Some("End of table".into());
        let spoken = read(&s);
        assert_eq!(spoken.first().unwrap(), "Table. Name.");
        assert_eq!(spoken.last().unwrap(), "55. End of table");
        s.tables = TableOrder::Rows;
        assert_eq!(
            read(&s),
            vec!["Table. Name, Age.", "Ada, 36.", "Linus, 55. End of table"]
        );
    }

    #[test]
    fn unknown_interpreter_is_reported() {
        let s = Settings {
            interpreter: "nope".into(),
            ..Default::default()
        };
        let prepared = s.prepare_report("Hello.");
        assert_eq!(prepared.utterances.len(), 1);
        assert!(prepared.warning.unwrap().contains("nope"));
        assert!(Settings::default()
            .prepare_report("Hello.")
            .warning
            .is_none());
    }

    #[test]
    fn rate_snaps_to_grid() {
        assert_eq!(clamp_rate(1.0 + 0.1 + 0.1 + 0.1), 1.3);
        assert_eq!(clamp_rate(0.0), MIN_RATE);
        assert_eq!(clamp_rate(f32::NAN), 1.0);
    }
}
