//! Interpreter plugins: change how text is read without recompiling.
//!
//! A plugin is any program. When text is about to be read, the program is
//! started, receives one JSON object on standard input and answers with one
//! JSON object on standard output:
//!
//! ```text
//! → {"version": 1,
//!    "text": "# Title\n\nSome *text*.",
//!    "blocks": [{"role": "heading.1", "text": "Title"},
//!               {"role": "paragraph", "text": "Some text."}]}
//! ← {"blocks": [{"role": "heading.1", "text": "Title"},
//!               {"role": "aside", "text": "Some text."}]}
//! ```
//!
//! * `text` is the original text; `blocks` is what the built-in interpreter
//!   named by `base` made of it (empty with `base = "none"`). A plugin that
//!   only wants to tweak things edits `blocks`; one that understands a
//!   format of its own works from `text`.
//! * Roles are free-form names. `code.python` is a kind of `code`: settings
//!   not given for the specific role come from the general one. New roles
//!   show up under **Settings → Voices by role** once they are configured.
//! * Whatever comes back is still plain text to the rest of the pipeline:
//!   angle brackets, links and substitutions are handled as usual, so a
//!   plugin cannot smuggle markup to a speech engine.
//!
//! If the program fails, takes too long or answers with something else, the
//! built-in interpretation is used and the user is told.

use super::{Block, InterpreterKind};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterpreterPlugin {
    /// Unique id, used in `interpreter = "..."`.
    pub id: String,
    /// Name shown in the settings.
    #[serde(default)]
    pub name: String,
    /// Executable (looked up on PATH).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Which built-in interpreter prepares `blocks` for the plugin:
    /// `"auto"`, `"plain"`, `"markdown"`, or `"none"` to start from the raw
    /// text alone.
    #[serde(default = "default_base", with = "base")]
    pub base: Option<InterpreterKind>,
    /// Give up after this many seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_base() -> Option<InterpreterKind> {
    Some(InterpreterKind::Auto)
}

fn default_timeout() -> u64 {
    10
}

mod base {
    use super::InterpreterKind;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<InterpreterKind>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.map(InterpreterKind::key).unwrap_or("none"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<InterpreterKind>, D::Error> {
        let key = String::deserialize(deserializer)?;
        if key == "none" {
            return Ok(None);
        }
        InterpreterKind::from_key(&key).map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown base \"{key}\" (expected auto, plain, markdown or none)"
            ))
        })
    }
}

impl InterpreterPlugin {
    pub fn name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    version: u32,
    text: &'a str,
    blocks: &'a [Block],
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Response {
    Object { blocks: Vec<Block> },
    List(Vec<Block>),
}

/// Upper bound for what a plugin may send back.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Run the plugin on `text` and the blocks the base interpreter produced.
pub fn run(plugin: &InterpreterPlugin, text: &str, blocks: &[Block]) -> Result<Vec<Block>> {
    let request = serde_json::to_vec(&Request {
        version: 1,
        text,
        blocks,
    })?;

    let mut cmd = Command::new(&plugin.program);
    cmd.args(&plugin.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start `{}`", plugin.program))?;

    // Feed and drain on separate threads so a full pipe cannot deadlock us.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&request);
    });
    let stdout = child.stdout.take().expect("stdout was piped");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes);
        bytes
    });
    let stderr = child.stderr.take().expect("stderr was piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.take(64 * 1024).read_to_string(&mut text);
        text
    });

    let deadline = Instant::now() + Duration::from_secs(plugin.timeout_secs.clamp(1, 600));
    let status = loop {
        match child.try_wait().context("could not wait for the plugin")? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("no answer within {} seconds", plugin.timeout_secs);
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let _ = writer.join();
    let output = reader
        .join()
        .map_err(|_| anyhow!("could not read the answer"))?;
    let errors = errors.join().unwrap_or_default();

    if !status.success() {
        let last = errors.lines().rev().find(|l| !l.trim().is_empty());
        bail!(
            "it exited with {status}{}",
            last.map(|l| format!(": {}", l.trim().chars().take(300).collect::<String>()))
                .unwrap_or_default()
        );
    }
    let response: Response =
        serde_json::from_slice(&output).context("its answer is not the expected JSON")?;
    Ok(match response {
        Response::Object { blocks } | Response::List(blocks) => blocks,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::text::{blocks, MarkdownOptions, Role, TextOptions};

    fn python_plugin(script: &str, base: Option<InterpreterKind>) -> Option<InterpreterPlugin> {
        let python = which::which("python3").ok()?;
        let path = crate::engine::temp_path("py");
        std::fs::write(&path, script).unwrap();
        Some(InterpreterPlugin {
            id: "test".into(),
            name: "Test plugin".into(),
            program: python.to_string_lossy().into_owned(),
            args: vec![path.to_string_lossy().into_owned()],
            base,
            timeout_secs: 2,
        })
    }

    fn options(plugin: InterpreterPlugin) -> TextOptions {
        TextOptions {
            interpreter: InterpreterKind::Markdown,
            markdown: MarkdownOptions::default(),
            plugin: Some(plugin),
            ..Default::default()
        }
    }

    #[test]
    fn a_plugin_can_rewrite_blocks_and_invent_roles() {
        let script = r#"
import json, sys
request = json.load(sys.stdin)
out = []
for block in request["blocks"]:
    if block["text"].startswith("Note:"):
        block["role"] = "Callout.Note"          # a role nobody has heard of
    if block["role"].startswith("code"):
        block["text"] = "<b>" + block["text"]   # must stay harmless text
    out.append(block)
out.append({"role": "paragraph", "text": "Read from: " + request["text"][:7]})
json.dump({"blocks": out}, sys.stdout)
"#;
        let Some(plugin) = python_plugin(script, Some(InterpreterKind::Markdown)) else {
            eprintln!("python3 not installed; skipping");
            return;
        };
        let opts = options(plugin);
        let (blocks, warning) = blocks("# Title\n\nNote: careful.\n\n```js\nx\n```\n", &opts);
        assert_eq!(warning, None);
        let got: Vec<(&str, &str)> = blocks
            .iter()
            .map(|b| (b.role.as_str(), b.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("heading.1", "Title"),
                ("callout.note", "Note: careful."),
                ("code.js", "<b>x"),
                ("paragraph", "Read from: # Title"),
            ]
        );
        // Plugin output goes through the same safety pass as everything else.
        let spoken = crate::text::prepare("```js\nx\n```\n", &opts);
        assert_eq!(spoken[0].spoken, "b tag x");
        assert!(blocks[1].role.is_a("callout"));
        assert_eq!(blocks[1].role, Role::new("callout.note"));
    }

    #[test]
    fn a_plugin_can_work_from_the_raw_text_alone() {
        let script = r#"
import json, sys
request = json.load(sys.stdin)
assert request["blocks"] == []
print(json.dumps([{"role": "line", "text": line} for line in request["text"].splitlines()]))
"#;
        let Some(plugin) = python_plugin(script, None) else {
            return;
        };
        let (blocks, warning) = blocks("# one\n\ntwo", &options(plugin));
        assert_eq!(warning, None);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            (blocks[0].role.as_str(), blocks[0].text.as_str()),
            ("line", "# one")
        );
    }

    #[test]
    fn failures_fall_back_to_the_built_in_interpretation() {
        for (script, expected) in [
            (
                "import sys; sys.stderr.write('boom\\n'); sys.exit(3)",
                "boom",
            ),
            ("print('this is not json')", "not the expected JSON"),
            ("import time; time.sleep(30)", "no answer within"),
        ] {
            let Some(plugin) = python_plugin(script, Some(InterpreterKind::Markdown)) else {
                return;
            };
            let (blocks, warning) = blocks("# Title\n\nBody.", &options(plugin));
            let warning = warning.expect("a warning");
            assert!(warning.contains("Test plugin"), "{warning}");
            assert!(warning.contains(expected), "{warning}");
            assert_eq!(blocks.len(), 2, "the built-in blocks are used instead");
            assert_eq!(blocks[0].role.as_str(), "heading.1");
        }
        let missing = InterpreterPlugin {
            id: "gone".into(),
            name: String::new(),
            program: "definitely-not-a-real-plugin".into(),
            args: vec![],
            base: None,
            timeout_secs: 1,
        };
        let (blocks, warning) = blocks("Plain text.", &options(missing));
        assert!(warning.unwrap().contains("gone"));
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn config_round_trip() {
        let plugin: InterpreterPlugin =
            toml::from_str("id = \"x\"\nprogram = \"python3\"\nbase = \"none\"").unwrap();
        assert_eq!(plugin.base, None);
        assert_eq!(plugin.timeout_secs, 10);
        let text = toml::to_string(&plugin).unwrap();
        assert!(text.contains("base = \"none\""));
        let default: InterpreterPlugin = toml::from_str("id = \"x\"\nprogram = \"p\"").unwrap();
        assert_eq!(default.base, Some(InterpreterKind::Auto));
        assert!(
            toml::from_str::<InterpreterPlugin>("id=\"x\"\nprogram=\"p\"\nbase=\"html\"").is_err()
        );
    }
}
