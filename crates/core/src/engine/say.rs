//! macOS system voices through `/usr/bin/say`.
//!
//! `say` renders to a file instead of the speakers, which gives us the same
//! PCM-in-hand model as every other engine. The text is passed in a file and
//! never on the command line.

use super::{run_command, temp_path, wav, Audio, Engine, SynthRequest, Voice};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Words per minute at rate 1.0. `say` voices default to roughly this.
const BASE_WPM: f32 = 180.0;

pub struct SayEngine {
    program: PathBuf,
}

impl SayEngine {
    pub fn detect() -> Option<Self> {
        let program = PathBuf::from("/usr/bin/say");
        program.exists().then_some(Self { program })
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
        let input = temp_path("txt");
        let output = temp_path("wav");
        let mut text = neutralize_embedded_commands(&req.text);
        // Our own, trusted, embedded commands for what `say` has no flag for.
        let mut prefix = String::new();
        if (req.volume - 1.0).abs() > 0.01 {
            prefix.push_str(&format!("[[volm {:.2}]] ", req.volume.clamp(0.0, 1.0)));
        }
        if (req.pitch - 1.0).abs() > 0.01 {
            // Relative baseline pitch in semitone-ish steps.
            let delta = (12.0 * req.pitch.max(0.1).log2()).round() as i32;
            let sign = if delta >= 0 { "+" } else { "-" };
            prefix.push_str(&format!("[[pbas {sign}{}]] ", delta.abs()));
        }
        text.insert_str(0, &prefix);
        std::fs::write(&input, text.as_bytes()).context("could not write temp text file")?;

        let wpm = (BASE_WPM * req.rate).round().clamp(60.0, 720.0) as i32;
        let mut cmd = Command::new(&self.program);
        if let Some(voice) = req.voice.as_deref().filter(|v| !v.is_empty() && !v.starts_with('-')) {
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
