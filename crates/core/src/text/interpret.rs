//! Interpreters: turn text into blocks with roles.
//!
//! This is the main extension point for "how should this text be read".
//! The built-in interpreters live here; an external program can replace or
//! post-process their output without recompiling anything (see
//! [`super::plugin`]). Everything downstream (voices per role, pauses,
//! announcements, sentence navigation) only looks at the role names.

use super::Role;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub role: Role,
    pub text: String,
}

impl Block {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
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

    pub fn key(self) -> &'static str {
        match self {
            InterpreterKind::Auto => "auto",
            InterpreterKind::Plain => "plain",
            InterpreterKind::Markdown => "markdown",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// In which order the cells of a table are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableOrder {
    /// Column by column: a column's heading, then its cells from top to
    /// bottom, then the next column to the right.
    #[default]
    Columns,
    /// Row by row, left to right, the way the table is written.
    Rows,
}

impl TableOrder {
    pub const ALL: [TableOrder; 2] = [TableOrder::Columns, TableOrder::Rows];

    pub fn label(self) -> &'static str {
        match self {
            TableOrder::Columns => "Column by column",
            TableOrder::Rows => "Row by row",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            TableOrder::Columns => "columns",
            TableOrder::Rows => "rows",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MarkdownOptions {
    pub tables: TableOrder,
}

pub fn interpret(text: &str, kind: InterpreterKind, markdown: &MarkdownOptions) -> Vec<Block> {
    let is_markdown = match kind {
        InterpreterKind::Plain => false,
        InterpreterKind::Markdown => true,
        InterpreterKind::Auto => detect_markdown(text),
    };
    tidy(if is_markdown {
        interpret_markdown(text, markdown)
    } else {
        interpret_plain(text)
    })
}

/// Normalise whitespace in blocks and drop the empty ones. Also applied to
/// what plugins return.
pub(crate) fn tidy(mut blocks: Vec<Block>) -> Vec<Block> {
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
                blocks.push(Block::new(Role::paragraph(), std::mem::take(&mut current)));
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        blocks.push(Block::new(Role::paragraph(), current));
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

/// What kind of block the text being collected belongs to.
#[derive(Clone, PartialEq)]
enum Leaf {
    Paragraph,
    Heading(Role),
    Code(Role),
}

fn heading_role(level: HeadingLevel) -> Role {
    let n = match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    };
    Role::heading().specific(&n.to_string())
}

/// `code.python` for a block fenced with "```python", plain `code` otherwise.
fn code_role(kind: &CodeBlockKind) -> Role {
    let language = match kind {
        CodeBlockKind::Fenced(info) => info
            .split([' ', ',', '{', '\t'])
            .next()
            .unwrap_or_default()
            .trim_start_matches('.'),
        CodeBlockKind::Indented => "",
    };
    let language: String = language
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+' | '#'))
        .collect::<String>()
        .replace('+', "p")
        .replace('#', "sharp");
    if language.is_empty() {
        Role::code()
    } else {
        Role::code().specific(&language)
    }
}

/// End a cell with a full stop so that each cell is its own sentence, however
/// long it is.
fn as_sentence(cell: &str) -> String {
    let cell = cell.trim();
    if cell.is_empty() || cell.ends_with(['.', '!', '?', ':', ';', '…']) {
        cell.to_string()
    } else {
        format!("{cell}.")
    }
}

fn table_blocks(rows: Vec<Vec<String>>, has_header: bool, order: TableOrder) -> Vec<Block> {
    let table = Role::new(Role::TABLE);
    let (header, cell, row) = (
        table.specific("header"),
        table.specific("cell"),
        table.specific("row"),
    );
    let mut blocks = Vec::new();
    match order {
        TableOrder::Rows => {
            for (i, cells) in rows.iter().enumerate() {
                let text = cells
                    .iter()
                    .map(|c| c.trim())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                let role = if i == 0 && has_header { &header } else { &row };
                blocks.push(Block::new(role.clone(), as_sentence(&text)));
            }
        }
        TableOrder::Columns => {
            let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
            for column in 0..columns {
                let mut cells = rows.iter().map(|r| r.get(column).map(String::as_str));
                if has_header {
                    if let Some(Some(heading)) = cells.next() {
                        blocks.push(Block::new(header.clone(), as_sentence(heading)));
                    }
                }
                let text = cells
                    .flatten()
                    .map(as_sentence)
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                blocks.push(Block::new(cell.clone(), text));
            }
        }
    }
    blocks
}

fn interpret_markdown(text: &str, markdown: &MarkdownOptions) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut blocks: Vec<Block> = Vec::new();
    let mut buf = String::new();
    // Container roles (quote, list item) override the paragraph role inside.
    let mut containers: Vec<Role> = Vec::new();
    let mut leaf: Option<Leaf> = None;
    let mut list_counters: Vec<Option<u64>> = Vec::new();
    let mut pending_item_prefix: Option<String> = None;
    // The table being collected: rows of cells, the first being the header.
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut table_has_header = false;

    fn flush(blocks: &mut Vec<Block>, buf: &mut String, role: Role) {
        let text = buf.trim();
        if !text.is_empty() {
            blocks.push(Block::new(role, text));
        }
        buf.clear();
    }

    let current_role = |containers: &Vec<Role>, leaf: &Option<Leaf>| -> Role {
        match leaf {
            Some(Leaf::Heading(role)) | Some(Leaf::Code(role)) => role.clone(),
            _ => containers.last().cloned().unwrap_or_else(Role::paragraph),
        }
    };

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    leaf = Some(Leaf::Paragraph);
                }
                Tag::Heading { level, .. } => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    leaf = Some(Leaf::Heading(heading_role(level)));
                }
                Tag::BlockQuote(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    containers.push(Role::quote());
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    leaf = Some(Leaf::Code(code_role(&kind)));
                }
                Tag::List(start) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    list_counters.push(start);
                }
                Tag::Item => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    // Quotes keep their role even for lists inside them.
                    if containers.last().is_some_and(|r| r.is_a(Role::QUOTE)) {
                        containers.push(Role::quote());
                    } else {
                        containers.push(Role::list_item());
                    }
                    if let Some(Some(n)) = list_counters.last_mut() {
                        pending_item_prefix = Some(format!("{n}. "));
                        *n += 1;
                    }
                }
                Tag::Image { .. } => buf.push_str("Image: "),
                Tag::Table(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    table = Some(Vec::new());
                    table_has_header = false;
                }
                Tag::TableHead => {
                    table_has_header = true;
                    if let Some(rows) = &mut table {
                        rows.push(Vec::new());
                    }
                }
                Tag::TableRow => {
                    if let Some(rows) = &mut table {
                        rows.push(Vec::new());
                    }
                }
                Tag::TableCell => buf.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    leaf = None;
                }
                TagEnd::Heading(_) | TagEnd::CodeBlock => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    leaf = None;
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    containers.pop();
                }
                TagEnd::Item => {
                    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
                    containers.pop();
                    pending_item_prefix = None;
                }
                TagEnd::List(_) => {
                    list_counters.pop();
                }
                TagEnd::Image => buf.push_str(". "),
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut buf);
                    if let Some(row) = table.as_mut().and_then(|rows| rows.last_mut()) {
                        row.push(cell.trim().to_string());
                    }
                }
                TagEnd::Table => {
                    if let Some(rows) = table.take() {
                        blocks.extend(table_blocks(rows, table_has_header, markdown.tables));
                    }
                    buf.clear();
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
                flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
            }
            Event::TaskListMarker(done) => {
                buf.push_str(if done { "Done: " } else { "To do: " });
            }
            Event::FootnoteReference(_) => {}
        }
    }
    flush(&mut blocks, &mut buf, current_role(&containers, &leaf));
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(text: &str, kind: InterpreterKind, tables: TableOrder) -> Vec<(String, String)> {
        interpret(text, kind, &MarkdownOptions { tables })
            .into_iter()
            .map(|b| (b.role.to_string(), b.text))
            .collect()
    }

    fn md(text: &str) -> Vec<(String, String)> {
        with(text, InterpreterKind::Markdown, TableOrder::default())
    }

    fn pair(role: &str, text: &str) -> (String, String) {
        (role.to_string(), text.to_string())
    }

    #[test]
    fn plain_paragraphs() {
        let b = with(
            "One\ntwo\n\n\nThree",
            InterpreterKind::Plain,
            TableOrder::Rows,
        );
        assert_eq!(
            b,
            vec![pair("paragraph", "One\ntwo"), pair("paragraph", "Three")]
        );
    }

    #[test]
    fn plain_never_treats_markup() {
        let b = with(
            "# not a heading\n> not a quote",
            InterpreterKind::Plain,
            TableOrder::Rows,
        );
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].0, "paragraph");
    }

    #[test]
    fn markdown_blocks() {
        let b = md("# Title\n\nSome *emphasis* and `code` and [a link](http://x.y).\n\n> Quoted\n> text\n\n- one\n- two\n\n```rust\nlet a = 1;\n```\n\n## Sub\n\n    indented code\n");
        assert_eq!(
            b,
            vec![
                pair("heading.1", "Title"),
                pair("paragraph", "Some emphasis and code and a link."),
                pair("quote", "Quoted text"),
                pair("list_item", "one"),
                pair("list_item", "two"),
                pair("code.rust", "let a = 1;"),
                pair("heading.2", "Sub"),
                pair("code", "indented code"),
            ]
        );
    }

    #[test]
    fn code_languages_become_roles() {
        let role = |fence: &str| md(&format!("```{fence}\nx\n```\n"))[0].0.clone();
        assert_eq!(role("python"), "code.python");
        assert_eq!(role("JavaScript title=\"a.js\""), "code.javascript");
        assert_eq!(role("c++"), "code.cpp");
        assert_eq!(role("c#"), "code.csharp");
        assert_eq!(role("{.haskell}"), "code");
        assert_eq!(role(""), "code");
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

    const TABLE: &str = "Before.\n\n| Name | Age | City |\n|---|---|---|\n| Ada | 36 | London |\n| Linus | 55 |  |\n\nAfter.\n";

    #[test]
    fn tables_row_by_row() {
        let b = with(TABLE, InterpreterKind::Markdown, TableOrder::Rows);
        assert_eq!(
            b,
            vec![
                pair("paragraph", "Before."),
                pair("table.header", "Name, Age, City."),
                pair("table.row", "Ada, 36, London."),
                pair("table.row", "Linus, 55."),
                pair("paragraph", "After."),
            ]
        );
    }

    #[test]
    fn tables_column_by_column() {
        let b = with(TABLE, InterpreterKind::Markdown, TableOrder::Columns);
        assert_eq!(
            b,
            vec![
                pair("paragraph", "Before."),
                pair("table.header", "Name."),
                pair("table.cell", "Ada.\nLinus."),
                pair("table.header", "Age."),
                pair("table.cell", "36.\n55."),
                pair("table.header", "City."),
                // The empty cell is not read.
                pair("table.cell", "London."),
                pair("paragraph", "After."),
            ]
        );
    }

    #[test]
    fn table_cells_keep_inline_text() {
        let b = with(
            "| A | B |\n|---|---|\n| `x < y` and **bold** | [link](http://e.x) |\n",
            InterpreterKind::Markdown,
            TableOrder::Columns,
        );
        assert_eq!(b[1], pair("table.cell", "x < y and bold."));
        assert_eq!(b[3], pair("table.cell", "link."));
    }
}
