//! Interpreters: turn text into blocks with roles.
//!
//! This is the main extension point for "how should this text be read".
//! Adding a new format means adding a variant to [`InterpreterKind`] and a
//! function that returns `Vec<Block>`; everything downstream (voices per
//! role, pauses, sentence navigation) keeps working unchanged.

use super::Role;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpreterKind {
    /// Markdown if the text looks like Markdown, plain text otherwise.
    #[default]
    Auto,
    /// Every blank-line separated chunk is a paragraph. Nothing is markup.
    Plain,
    /// CommonMark + tables / strikethrough / task lists.
    Markdown,
}

impl InterpreterKind {
    pub const ALL: [InterpreterKind; 3] = [
        InterpreterKind::Auto,
        InterpreterKind::Plain,
        InterpreterKind::Markdown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InterpreterKind::Auto => "Auto-detect",
            InterpreterKind::Plain => "Plain text",
            InterpreterKind::Markdown => "Markdown",
        }
    }
}

pub fn interpret(text: &str, kind: InterpreterKind) -> Vec<Block> {
    let markdown = match kind {
        InterpreterKind::Plain => false,
        InterpreterKind::Markdown => true,
        InterpreterKind::Auto => detect_markdown(text),
    };
    let mut blocks = if markdown {
        interpret_markdown(text)
    } else {
        interpret_plain(text)
    };
    for b in &mut blocks {
        b.text = collapse_spaces(&b.text);
    }
    blocks.retain(|b| !b.text.is_empty());
    blocks
}

/// Collapse runs of spaces, trim each line, drop empty lines.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let mut last_space = true;
        let start = out.len();
        for c in line.chars() {
            if c == ' ' {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(c);
                last_space = false;
            }
        }
        while out.len() > start && out.ends_with(' ') {
            out.pop();
        }
        if out.len() > start {
            out.push('\n');
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Plain text
// ---------------------------------------------------------------------------

fn interpret_plain(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(Block {
                    role: Role::Paragraph,
                    text: std::mem::take(&mut current),
                });
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        blocks.push(Block {
            role: Role::Paragraph,
            text: current,
        });
    }
    blocks
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// Cheap heuristic: does this look like Markdown source?
///
/// Deliberately conservative — prose that merely contains a `*` or a `#`
/// should stay plain text.
pub fn detect_markdown(text: &str) -> bool {
    let mut score = 0u32;
    let mut in_fence = false;
    for line in text.lines().take(400) {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            score += 2;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            score += 3;
        }
        if t.starts_with("> ") {
            score += 2;
        }
        if (t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")) && t.len() > 2 {
            score += 1;
        }
        if t.starts_with("- [ ] ") || t.starts_with("- [x] ") {
            score += 2;
        }
        if t.contains("](") && t.contains('[') {
            score += 2;
        }
        if t.matches("**").count() >= 2 || t.matches("__").count() >= 2 {
            score += 2;
        }
        if t.matches('`').count() >= 2 {
            score += 1;
        }
        if t.starts_with('|') && t.ends_with('|') && t.len() > 2 {
            score += 1;
        }
    }
    score >= 3
}

fn interpret_markdown(text: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut blocks: Vec<Block> = Vec::new();
    let mut buf = String::new();
    // Container roles (quote, list item) override the paragraph role inside.
    let mut containers: Vec<Role> = Vec::new();
    let mut leaf: Option<Role> = None;
    let mut list_counters: Vec<Option<u64>> = Vec::new();
    let mut pending_item_prefix: Option<String> = None;
    let mut in_image = false;

    fn flush(blocks: &mut Vec<Block>, buf: &mut String, role: Role) {
        let text = buf.trim();
        if !text.is_empty() {
            blocks.push(Block {
                role,
                text: text.to_string(),
            });
        }
        buf.clear();
    }

    let current_role = |containers: &Vec<Role>, leaf: Option<Role>| -> Role {
        match leaf {
            Some(Role::Heading) => Role::Heading,
            Some(Role::Code) => Role::Code,
            _ => containers.last().copied().unwrap_or(Role::Paragraph),
        }
    };

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    leaf = Some(Role::Paragraph);
                }
                Tag::Heading { .. } => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    leaf = Some(Role::Heading);
                }
                Tag::BlockQuote(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    containers.push(Role::Quote);
                }
                Tag::CodeBlock(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    leaf = Some(Role::Code);
                }
                Tag::List(start) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    list_counters.push(start);
                }
                Tag::Item => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    // Quotes keep their role even for lists inside them.
                    if containers.last() != Some(&Role::Quote) {
                        containers.push(Role::ListItem);
                    } else {
                        containers.push(Role::Quote);
                    }
                    if let Some(Some(n)) = list_counters.last_mut() {
                        pending_item_prefix = Some(format!("{n}. "));
                        *n += 1;
                    }
                }
                Tag::Image { .. } => {
                    in_image = true;
                    buf.push_str("Image: ");
                }
                Tag::TableCell => {}
                Tag::TableHead | Tag::TableRow => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    leaf = None;
                }
                TagEnd::Heading(_) => {
                    flush(&mut blocks, &mut buf, Role::Heading);
                    leaf = None;
                }
                TagEnd::CodeBlock => {
                    flush(&mut blocks, &mut buf, Role::Code);
                    leaf = None;
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    containers.pop();
                }
                TagEnd::Item => {
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                    containers.pop();
                    pending_item_prefix = None;
                }
                TagEnd::List(_) => {
                    list_counters.pop();
                }
                TagEnd::Image => {
                    in_image = false;
                    buf.push_str(". ");
                }
                TagEnd::TableCell => buf.push_str(", "),
                TagEnd::TableHead | TagEnd::TableRow => {
                    let trimmed = buf.trim_end_matches([',', ' ']).to_string();
                    buf.clear();
                    buf.push_str(&trimmed);
                    buf.push('.');
                    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
                }
                _ => {}
            },
            Event::Text(t) | Event::Code(t) => {
                if let Some(prefix) = pending_item_prefix.take() {
                    buf.push_str(&prefix);
                }
                buf.push_str(&t);
            }
            // Raw HTML inside Markdown is *text* to us, never markup: it flows
            // through the angle bracket policy like everything else.
            Event::Html(t) | Event::InlineHtml(t) => {
                buf.push_str(&t);
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => buf.push_str(&t),
            Event::SoftBreak => buf.push(' '),
            Event::HardBreak => buf.push('\n'),
            Event::Rule => {
                flush(&mut blocks, &mut buf, current_role(&containers, leaf));
            }
            Event::TaskListMarker(done) => {
                buf.push_str(if done { "Done: " } else { "To do: " });
            }
            Event::FootnoteReference(_) => {}
        }
        let _ = in_image;
    }
    flush(&mut blocks, &mut buf, current_role(&containers, leaf));
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(text: &str) -> Vec<(Role, String)> {
        interpret(text, InterpreterKind::Markdown)
            .into_iter()
            .map(|b| (b.role, b.text))
            .collect()
    }

    #[test]
    fn plain_paragraphs() {
        let b = interpret("One\ntwo\n\n\nThree", InterpreterKind::Plain);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].text, "One\ntwo");
        assert_eq!(b[1].text, "Three");
    }

    #[test]
    fn plain_never_treats_markup() {
        let b = interpret("# not a heading\n> not a quote", InterpreterKind::Plain);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].role, Role::Paragraph);
    }

    #[test]
    fn markdown_blocks() {
        let b = md("# Title\n\nSome *emphasis* and `code` and [a link](http://x.y).\n\n> Quoted\n> text\n\n- one\n- two\n\n```rust\nlet a = 1;\n```\n");
        assert_eq!(b[0], (Role::Heading, "Title".into()));
        assert_eq!(
            b[1],
            (Role::Paragraph, "Some emphasis and code and a link.".into())
        );
        assert_eq!(b[2], (Role::Quote, "Quoted text".into()));
        assert_eq!(b[3], (Role::ListItem, "one".into()));
        assert_eq!(b[4], (Role::ListItem, "two".into()));
        assert_eq!(b[5], (Role::Code, "let a = 1;".into()));
    }

    #[test]
    fn ordered_list_numbers_and_tasks() {
        let b = md("1. first\n2. second\n\n- [x] shipped\n- [ ] pending\n");
        assert_eq!(b[0].1, "1. first");
        assert_eq!(b[1].1, "2. second");
        assert_eq!(b[2].1, "Done: shipped");
        assert_eq!(b[3].1, "To do: pending");
    }

    #[test]
    fn html_in_markdown_is_kept_as_text() {
        let b = md("Before <script>alert(1)</script> after\n");
        assert!(b[0].1.contains("<script>"), "{:?}", b);
        assert!(b[0].1.contains("alert(1)"));
    }

    #[test]
    fn detection() {
        assert!(detect_markdown("# Hello\n\nworld"));
        assert!(detect_markdown("see [docs](http://a.b) and **this**"));
        assert!(!detect_markdown("Just a sentence. And another one."));
        assert!(!detect_markdown("Issue #42 is 2 * 3 * 4"));
        assert!(!detect_markdown("- a single dash line"));
    }

    #[test]
    fn table_rows() {
        let b = md("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(b[0].1, "a, b.");
        assert_eq!(b[1].1, "1, 2.");
    }
}
