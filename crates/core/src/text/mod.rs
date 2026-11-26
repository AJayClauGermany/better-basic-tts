//! Text pipeline: raw input → blocks with roles → speakable sentences.
//!
//! ```text
//! raw text ──sanitize──▶ interpreter ──▶ [Block{role,text}] ──plugin?──▶
//!          ──segment──▶ ──speakable──▶ ──announcements──▶ [Utterance]
//! ```
//!
//! Input is *always* treated as data. Nothing in the text can switch voices,
//! change the rate or inject markup into an engine: characters that engines
//! treat specially (angle brackets, `[[...]]` on macOS, ...) are either
//! spelled out or neutralised before synthesis. That also holds for text
//! that comes back from an interpreter plugin.

mod interpret;
pub mod plugin;
mod sanitize;
mod segment;
mod speakable;

pub use interpret::{
    detect_markdown, interpret, Block, InterpreterKind, MarkdownOptions, TableOrder,
};
pub use plugin::InterpreterPlugin;
pub use sanitize::sanitize;
pub use segment::split_sentences;
pub use speakable::{AngleBrackets, SpeakableOptions, Substitution, UrlMode};

use serde::{Deserialize, Serialize};

/// The structural role of a piece of text, such as `heading` or `quote`.
///
/// Roles are plain names, so interpreters and plugins can introduce new ones
/// without the rest of the app knowing about them. A dot makes a more
/// specific kind of a role: `code.python` is a `code`, `heading.2` is a
/// `heading`. Whatever is not configured for the specific role is taken from
/// the general one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Role(String);

impl From<String> for Role {
    fn from(name: String) -> Self {
        Role::new(&name)
    }
}

impl From<Role> for String {
    fn from(role: Role) -> Self {
        role.0
    }
}

impl Role {
    pub const PARAGRAPH: &'static str = "paragraph";
    pub const HEADING: &'static str = "heading";
    pub const QUOTE: &'static str = "quote";
    pub const CODE: &'static str = "code";
    pub const LIST_ITEM: &'static str = "list_item";
    pub const TABLE: &'static str = "table";

    /// The roles the built-in interpreters produce (possibly with a more
    /// specific suffix, e.g. `code.python`).
    pub const BUILT_IN: [&'static str; 6] = [
        Self::HEADING,
        Self::PARAGRAPH,
        Self::QUOTE,
        Self::CODE,
        Self::LIST_ITEM,
        Self::TABLE,
    ];

    /// Make a role from a name. Names are lower case and may contain
    /// letters, digits, `_`, `-` and `.`; anything else is dropped. An empty
    /// name becomes `paragraph`.
    pub fn new(name: &str) -> Self {
        let cleaned: String = name
            .trim()
            .chars()
            .flat_map(char::to_lowercase)
            .map(|c| if c == ' ' { '_' } else { c })
            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .collect();
        let cleaned = cleaned.trim_matches('.');
        // No empty segments ("a..b").
        let cleaned = cleaned
            .split('.')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        if cleaned.is_empty() {
            Role(Self::PARAGRAPH.to_string())
        } else {
            Role(cleaned)
        }
    }

    pub fn paragraph() -> Self {
        Role(Self::PARAGRAPH.into())
    }
    pub fn heading() -> Self {
        Role(Self::HEADING.into())
    }
    pub fn quote() -> Self {
        Role(Self::QUOTE.into())
    }
    pub fn code() -> Self {
        Role(Self::CODE.into())
    }
    pub fn list_item() -> Self {
        Role(Self::LIST_ITEM.into())
    }

    /// A more specific kind of this role: `code` + `python` → `code.python`.
    pub fn specific(&self, kind: &str) -> Self {
        Role::new(&format!("{}.{}", self.0, kind))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The general role this one belongs to: `code.python` → `code`.
    pub fn root(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }

    /// From general to specific: `code.python.3` → `code`, `code.python`,
    /// `code.python.3`.
    pub fn lineage(&self) -> impl Iterator<Item = &str> {
        let name = self.0.as_str();
        name.match_indices('.')
            .map(move |(i, _)| &name[..i])
            .chain(std::iter::once(name))
    }

    /// Is this role `base` or a more specific kind of it?
    pub fn is_a(&self, base: &str) -> bool {
        self.0 == base
            || (self.0.len() > base.len()
                && self.0.starts_with(base)
                && self.0.as_bytes()[base.len()] == b'.')
    }

    /// For display: `list_item` → "List item", `code.python` → "Code (python)".
    pub fn label(&self) -> String {
        let mut parts = self.0.splitn(2, '.');
        let root = parts.next().unwrap_or_default().replace('_', " ");
        let mut label = String::new();
        let mut chars = root.chars();
        if let Some(first) = chars.next() {
            label.extend(first.to_uppercase());
            label.push_str(chars.as_str());
        }
        if let Some(rest) = parts.next() {
            label.push_str(&format!(" ({})", rest.replace('_', " ")));
        }
        label
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One unit of speech: a sentence (or short fragment) with the role of the
/// block it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub role: Role,
    /// What is sent to the engine (symbols expanded, substitutions applied,
    /// announcements added).
    pub spoken: String,
    /// What the user sees in the UI (the original wording).
    pub display: String,
    /// True for the last sentence of its block; used to insert a longer pause.
    pub ends_block: bool,
}

/// Options for the whole text pipeline.
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    pub interpreter: InterpreterKind,
    pub markdown: MarkdownOptions,
    /// An external program that produces or rewrites the blocks.
    pub plugin: Option<InterpreterPlugin>,
    pub speakable: SpeakableOptions,
}

/// Words to speak around a block, decided by the caller per role.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Announcement {
    /// Spoken before the block.
    pub before: String,
    /// Spoken after the block.
    pub after: String,
}

/// Where a block sits in a *run*: consecutive blocks of the same general
/// role, such as the items of one list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub starts_run: bool,
    pub ends_run: bool,
}

/// Result of [`prepare_with`].
#[derive(Debug, Clone, Default)]
pub struct Prepared {
    pub utterances: Vec<Utterance>,
    /// Something went wrong that the user should know about, but reading can
    /// go ahead (for example, a plugin failed and was bypassed).
    pub warning: Option<String>,
}

/// Run the full pipeline without announcements.
pub fn prepare(raw: &str, opts: &TextOptions) -> Vec<Utterance> {
    prepare_with(raw, opts, |_| false, |_, _| Announcement::default()).utterances
}

/// Turn raw text into blocks: sanitize, interpret, then let the plugin (if
/// any) have its say. A failing plugin is bypassed and reported.
pub fn blocks(raw: &str, opts: &TextOptions) -> (Vec<Block>, Option<String>) {
    let clean = sanitize(raw);
    let Some(plugin) = &opts.plugin else {
        return (interpret(&clean, opts.interpreter, &opts.markdown), None);
    };
    let base = match plugin.base {
        Some(kind) => interpret(&clean, kind, &opts.markdown),
        None => Vec::new(),
    };
    match plugin::run(plugin, &clean, &base) {
        Ok(blocks) => (interpret::tidy(blocks), None),
        Err(err) => {
            let fallback = if plugin.base.is_some() {
                base
            } else {
                interpret(&clean, InterpreterKind::Auto, &opts.markdown)
            };
            (
                fallback,
                Some(format!(
                    "The \"{}\" plugin failed and was skipped: {err:#}",
                    plugin.name()
                )),
            )
        }
    }
}

/// Run the full pipeline.
///
/// * `skip` — roles that are not read at all.
/// * `announce` — words to speak around each block.
pub fn prepare_with(
    raw: &str,
    opts: &TextOptions,
    skip: impl Fn(&Role) -> bool,
    announce: impl Fn(&Role, Position) -> Announcement,
) -> Prepared {
    let (mut blocks, warning) = blocks(raw, opts);
    blocks.retain(|b| !skip(&b.role));
    let speakable = speakable::Speakable::new(&opts.speakable);

    let mut out: Vec<Utterance> = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let position = Position {
            starts_run: index == 0 || blocks[index - 1].role.root() != block.role.root(),
            ends_run: blocks
                .get(index + 1)
                .is_none_or(|next| next.role.root() != block.role.root()),
        };
        let first = out.len();
        for sentence in split_sentences(&block.text) {
            let spoken = speakable.apply(&sentence, &block.role);
            // A sentence made only of punctuation has nothing to say.
            if !spoken.chars().any(|c| c.is_alphanumeric()) {
                continue;
            }
            out.push(Utterance {
                role: block.role.clone(),
                spoken,
                display: sentence,
                ends_block: false,
            });
        }
        if out.len() == first {
            continue;
        }
        // Announcements are made safe for engines like any other text.
        let announcement = announce(&block.role, position);
        let before = speakable.apply(&announcement.before, &block.role);
        let after = speakable.apply(&announcement.after, &block.role);
        if !before.is_empty() {
            out[first].spoken = join_spoken(&before, &out[first].spoken);
        }
        let last = out.last_mut().expect("the block produced sentences");
        if !after.is_empty() {
            last.spoken = join_spoken(&last.spoken, &after);
        }
        last.ends_block = true;
    }
    Prepared {
        utterances: out,
        warning,
    }
}

/// Join two pieces of speech with a pause between them: a period is added
/// unless the first piece already ends in punctuation.
fn join_spoken(first: &str, second: &str) -> String {
    let first = first.trim_end();
    if first.ends_with(['.', ',', ':', ';', '!', '?', '…']) {
        format!("{first} {second}")
    } else {
        format!("{first}. {second}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names() {
        assert_eq!(Role::new(" Code.Python ").as_str(), "code.python");
        assert_eq!(Role::new("List Item").as_str(), "list_item");
        assert_eq!(Role::new("a..b.").as_str(), "a.b");
        assert_eq!(Role::new("<script>").as_str(), "script");
        assert_eq!(Role::new("").as_str(), "paragraph");
        let role = Role::new("code.python.3");
        assert_eq!(
            role.lineage().collect::<Vec<_>>(),
            vec!["code", "code.python", "code.python.3"]
        );
        assert_eq!(role.root(), "code");
        assert!(role.is_a("code") && role.is_a("code.python") && role.is_a("code.python.3"));
        assert!(!role.is_a("cod") && !Role::new("codex").is_a("code"));
        assert_eq!(Role::new("list_item").label(), "List item");
        assert_eq!(Role::new("code.python").label(), "Code (python)");
        assert_eq!(Role::code().specific("Rust"), Role::new("code.rust"));
    }

    #[test]
    fn pipeline_keeps_angle_bracket_content() {
        let u = prepare("Use Vec<String> when a < b.", &TextOptions::default());
        assert_eq!(u.len(), 1);
        assert!(u[0].spoken.contains("less than"), "{}", u[0].spoken);
        assert!(u[0].spoken.contains("String"));
        assert!(!u[0].spoken.contains('<'));
        assert_eq!(u[0].display, "Use Vec<String> when a < b.");
    }

    #[test]
    fn markdown_roles_flow_through() {
        let opts = TextOptions {
            interpreter: InterpreterKind::Markdown,
            ..Default::default()
        };
        let u = prepare("# Title\n\nHello there. Second one.\n\n> A quote.\n", &opts);
        let roles: Vec<_> = u.iter().map(|u| u.role.as_str()).collect();
        assert_eq!(roles, vec!["heading.1", "paragraph", "paragraph", "quote"]);
        assert!(u[0].ends_block);
        assert!(!u[1].ends_block);
        assert!(u[2].ends_block);
    }

    #[test]
    fn punctuation_only_is_dropped() {
        let u = prepare("Hello.\n\n---\n\n***\n\nBye.", &TextOptions::default());
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn announcements_wrap_blocks_and_runs() {
        let opts = TextOptions {
            interpreter: InterpreterKind::Markdown,
            ..Default::default()
        };
        let text = "Intro.\n\n- One\n- Two. Still two.\n- Three\n\nOutro.";
        let prepared = prepare_with(
            text,
            &opts,
            |_| false,
            |role, position| {
                if !role.is_a("list_item") {
                    return Announcement::default();
                }
                Announcement {
                    before: if position.starts_run {
                        "List. Point".into()
                    } else {
                        "Point".into()
                    },
                    after: if position.ends_run {
                        "End of <list>".into()
                    } else {
                        String::new()
                    },
                }
            },
        );
        let spoken: Vec<_> = prepared
            .utterances
            .iter()
            .map(|u| u.spoken.as_str())
            .collect();
        assert_eq!(
            spoken,
            vec![
                "Intro.",
                "List. Point. One",
                "Point. Two.",
                "Still two.",
                // Announcements go through the same safety pass as the text.
                "Point. Three. End of list tag",
                "Outro.",
            ]
        );
        // What is shown stays what was written.
        assert_eq!(prepared.utterances[1].display, "One");
    }

    #[test]
    fn skipped_roles_do_not_break_runs() {
        let opts = TextOptions {
            interpreter: InterpreterKind::Markdown,
            ..Default::default()
        };
        let text = "- One\n\n```\ncode\n```\n\n- Two\n";
        let prepared = prepare_with(
            text,
            &opts,
            |role| role.is_a("code"),
            |_, position| Announcement {
                before: if position.starts_run {
                    "Open".into()
                } else {
                    String::new()
                },
                after: String::new(),
            },
        );
        let spoken: Vec<_> = prepared
            .utterances
            .iter()
            .map(|u| u.spoken.as_str())
            .collect();
        // With the code block skipped the two items are one list.
        assert_eq!(spoken, vec!["Open. One", "Two"]);
    }
}
