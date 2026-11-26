//! Text pipeline: raw input → blocks with roles → speakable sentences.
//!
//! ```text
//! raw text ──sanitize──▶ interpreter ──▶ [Block{role,text}]
//!                                   ──speakable──▶ ──segment──▶ [Utterance]
//! ```
//!
//! Input is *always* treated as data. Nothing in the text can switch voices,
//! change the rate or inject markup into an engine: characters that engines
//! treat specially (angle brackets, `[[...]]` on macOS, ...) are either
//! spelled out or neutralised before synthesis.

mod interpret;
mod sanitize;
mod segment;
mod speakable;

pub use interpret::{detect_markdown, interpret, Block, InterpreterKind};
pub use sanitize::sanitize;
pub use segment::split_sentences;
pub use speakable::{AngleBrackets, SpeakableOptions, Substitution, UrlMode};

use serde::{Deserialize, Serialize};

/// The structural role of a piece of text. Interpreters assign roles, and the
/// user maps roles to voices / rates in the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Paragraph,
    Heading,
    Quote,
    Code,
    ListItem,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Paragraph,
        Role::Heading,
        Role::Quote,
        Role::Code,
        Role::ListItem,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Role::Paragraph => "Paragraph",
            Role::Heading => "Heading",
            Role::Quote => "Quote",
            Role::Code => "Code",
            Role::ListItem => "List item",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Role::Paragraph => "paragraph",
            Role::Heading => "heading",
            Role::Quote => "quote",
            Role::Code => "code",
            Role::ListItem => "list_item",
        }
    }
}

/// One unit of speech: a sentence (or short fragment) with the role of the
/// block it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub role: Role,
    /// What is sent to the engine (symbols expanded, substitutions applied).
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
    pub speakable: SpeakableOptions,
}

/// Run the full pipeline.
pub fn prepare(raw: &str, opts: &TextOptions) -> Vec<Utterance> {
    let clean = sanitize(raw);
    let blocks = interpret(&clean, opts.interpreter);
    let speakable = speakable::Speakable::new(&opts.speakable);
    let mut out = Vec::new();
    for block in blocks {
        let sentences = split_sentences(&block.text);
        let n = sentences.len();
        for (i, sentence) in sentences.into_iter().enumerate() {
            let spoken = speakable.apply(&sentence, block.role);
            // A sentence made only of punctuation has nothing to say.
            if !spoken.chars().any(|c| c.is_alphanumeric()) {
                continue;
            }
            out.push(Utterance {
                role: block.role,
                spoken,
                display: sentence,
                ends_block: i + 1 == n,
            });
        }
        // If the trailing sentences were dropped, still mark the block end.
        if let Some(last) = out.last_mut() {
            if last.role == block.role && n > 0 {
                last.ends_block = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let roles: Vec<_> = u.iter().map(|u| u.role).collect();
        assert_eq!(
            roles,
            vec![Role::Heading, Role::Paragraph, Role::Paragraph, Role::Quote]
        );
        assert!(u[0].ends_block);
        assert!(!u[1].ends_block);
        assert!(u[2].ends_block);
    }

    #[test]
    fn punctuation_only_is_dropped() {
        let u = prepare("Hello.\n\n---\n\n***\n\nBye.", &TextOptions::default());
        assert_eq!(u.len(), 2);
    }
}
