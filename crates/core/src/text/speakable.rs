//! Make a sentence safe and audible for any engine.
//!
//! Most speech engines silently drop `<` and `>` or — worse — try to read
//! what is between them as markup. After this pass the spoken text contains
//! **no angle brackets at all**: they have been turned into words (or removed,
//! if the user asked for that), so no engine can mistake user text for SSML,
//! SAPI XML or HTML.

use super::Role;
use regex::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// How `<` and `>` are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AngleBrackets {
    /// `a < b` → "a less than b", `<div>` → "div tag",
    /// `Vec<String>` → "Vec of String".
    #[default]
    Smart,
    /// Every bracket is read literally: "less than" / "greater than".
    Literal,
    /// Things that look like HTML tags are removed; other brackets are read.
    StripTags,
    /// Brackets are replaced by a pause; what is inside them is still read.
    Silent,
}

impl AngleBrackets {
    pub const ALL: [AngleBrackets; 4] = [
        AngleBrackets::Smart,
        AngleBrackets::Literal,
        AngleBrackets::StripTags,
        AngleBrackets::Silent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AngleBrackets::Smart => "Smart",
            AngleBrackets::Literal => "Literal",
            AngleBrackets::StripTags => "Remove HTML tags",
            AngleBrackets::Silent => "Silent",
        }
    }
}

/// How URLs are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlMode {
    /// "example.com link"
    #[default]
    Domain,
    /// The whole URL, as the engine sees fit.
    Full,
    /// Just "link".
    Skip,
}

impl UrlMode {
    pub const ALL: [UrlMode; 3] = [UrlMode::Domain, UrlMode::Full, UrlMode::Skip];

    pub fn label(self) -> &'static str {
        match self {
            UrlMode::Domain => "Domain only",
            UrlMode::Full => "Full address",
            UrlMode::Skip => "Just say \"link\"",
        }
    }
}

/// A user-defined replacement applied before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Substitution {
    /// Text (or regular expression if `regex` is true) to look for.
    pub pattern: String,
    /// Replacement. With `regex = true`, `$1` etc. refer to capture groups.
    pub replacement: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    /// Limit the rule to some roles. Empty means all roles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<Role>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeakableOptions {
    pub angle_brackets: AngleBrackets,
    pub urls: UrlMode,
    pub substitutions: Vec<Substitution>,
}

/// Compiled form of [`SpeakableOptions`].
pub(crate) struct Speakable {
    angle_brackets: AngleBrackets,
    urls: UrlMode,
    substitutions: Vec<(Regex, String, Vec<Role>)>,
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<?\b((?:https?://|www\.)[^\s<>"')\]]+)>?"#).unwrap());
static RULER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(={4,}|-{4,}|_{4,}|\*{4,}|#{4,}|~{4,})").unwrap());
static LEADING_QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*>)+\s?").unwrap());
static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--(.*?)-->").unwrap());
static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<(/?)([A-Za-z][A-Za-z0-9:-]*)(\s+[^<>]*?)?\s*(/?)>"#).unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" +([,.;:!?])").unwrap());

impl Speakable {
    pub(crate) fn new(opts: &SpeakableOptions) -> Self {
        let mut substitutions = Vec::new();
        for s in &opts.substitutions {
            if s.pattern.is_empty() {
                continue;
            }
            let source = if s.regex {
                s.pattern.clone()
            } else {
                regex::escape(&s.pattern)
            };
            match RegexBuilder::new(&source)
                .case_insensitive(!s.case_sensitive)
                .size_limit(1 << 20)
                .build()
            {
                Ok(re) => {
                    // Literal rules must not have `$` interpreted.
                    let replacement = if s.regex {
                        s.replacement.clone()
                    } else {
                        s.replacement.replace('$', "$$")
                    };
                    substitutions.push((re, replacement, s.roles.clone()));
                }
                Err(err) => log::warn!("ignoring substitution {:?}: {err}", s.pattern),
            }
        }
        Self {
            angle_brackets: opts.angle_brackets,
            urls: opts.urls,
            substitutions,
        }
    }

    pub(crate) fn apply(&self, sentence: &str, role: Role) -> String {
        let mut text = sentence.to_string();

        // 1. URLs (before brackets so that <https://autolinks> work).
        text = URL
            .replace_all(&text, |c: &Captures| {
                // Sentence punctuation right after a URL is not part of it.
                let url = c[1].trim_end_matches(['.', ',', ';', ':', '!', '?']);
                let trailing = &c[1][url.len()..];
                match self.urls {
                    UrlMode::Full => format!("{url}{trailing}"),
                    UrlMode::Skip => format!(" link{trailing} "),
                    UrlMode::Domain => format!(" {} link{trailing} ", domain_of(url)),
                }
            })
            .into_owned();

        // 2. User substitutions see the text as written.
        for (re, replacement, roles) in &self.substitutions {
            if roles.is_empty() || roles.contains(&role) {
                text = re.replace_all(&text, replacement.as_str()).into_owned();
            }
        }

        // 3. Decorative rulers ("=====", "-----") are noise.
        text = RULER.replace_all(&text, " ").into_owned();

        // 4. Angle brackets.
        text = angle_brackets(&text, self.angle_brackets);
        debug_assert!(!text.contains(['<', '>']));

        // 5. Tidy up.
        let text = SPACES.replace_all(&text, " ");
        let text = SPACE_BEFORE_PUNCT.replace_all(&text, "$1");
        text.trim().to_string()
    }
}

/// Convenience wrapper used by tests and one-off callers.
#[cfg(test)]
pub(crate) fn make_speakable(sentence: &str, role: Role, opts: &SpeakableOptions) -> String {
    Speakable::new(opts).apply(sentence, role)
}

fn domain_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    host.trim_start_matches("www.")
        .trim_end_matches('.')
        .to_string()
}

fn angle_brackets(text: &str, mode: AngleBrackets) -> String {
    if !text.contains(['<', '>']) {
        return text.to_string();
    }
    match mode {
        AngleBrackets::Literal => literal(text),
        AngleBrackets::Silent => text.replace(['<', '>'], " "),
        AngleBrackets::StripTags => {
            let t = COMMENT.replace_all(text, " ");
            let t = TAG.replace_all(&t, " ");
            smart(&t)
        }
        AngleBrackets::Smart => smart(text),
    }
}

fn literal(text: &str) -> String {
    text.replace('<', " less than ")
        .replace('>', " greater than ")
}

fn smart(text: &str) -> String {
    // Email / forum style quoting at the start of a line.
    let text = LEADING_QUOTE.replace(text, "");
    // Multi-character operators first.
    let mut t = text
        .replace("<=>", " compared with ")
        .replace("<->", " both ways arrow ")
        .replace("<=", " less than or equal to ")
        .replace(">=", " greater than or equal to ")
        .replace("=>", " arrow ")
        .replace("->", " arrow ")
        .replace("<-", " left arrow ")
        .replace("<<", " shift left ")
        .replace("</>", " code ");
    t = COMMENT
        .replace_all(&t, |c: &Captures| format!(" comment: {} ", c[1].trim()))
        .into_owned();
    t = generics(&t);
    t = replace_tags(&t);
    // `>>` only after generics, so `Vec<Vec<u8>>` is not read as a shift.
    t = t.replace(">>", " shift right ");
    literal(&t)
}

/// `<div class="x">` → "div tag", `</div>` → "closing div tag".
///
/// A tag must not directly follow a word character; that case is a generic
/// (`Vec<String>`) or a comparison without spaces (`a<b`).
fn replace_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in TAG.captures_iter(text) {
        let whole = m.get(0).unwrap();
        let prev = text[..whole.start()].chars().next_back();
        if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        out.push_str(&text[last..whole.start()]);
        let name = &m[2];
        if &m[1] == "/" {
            out.push_str(&format!(" closing {name} tag "));
        } else {
            out.push_str(&format!(" {name} tag "));
        }
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// `Vec<String>` → "Vec of String", `HashMap<K, Vec<u8>>` → "HashMap of K, Vec of u8".
fn generics(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    // Stack of open generic brackets we have converted.
    let mut depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            let prev_is_ident = i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            let next_ok = chars.get(i + 1).is_some_and(|n| {
                n.is_alphanumeric() || matches!(n, '_' | '&' | '\'' | '[' | '(' | '*')
            });
            if prev_is_ident && next_ok && generic_closes(&chars, i) {
                out.push_str(" of ");
                depth += 1;
                i += 1;
                continue;
            }
        } else if c == '>' && depth > 0 {
            depth -= 1;
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Is the `<` at `open` closed by a matching `>` with only type-like
/// characters in between?
fn generic_closes(chars: &[char], open: usize) -> bool {
    let mut depth = 0i32;
    for (n, &c) in chars[open..].iter().enumerate() {
        if n > 120 {
            return false;
        }
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            c if c.is_alphanumeric() => {}
            '_' | ',' | ' ' | ':' | '&' | '\'' | '[' | ']' | '(' | ')' | '*' | '?' | '.' | ';' => {}
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smart(s: &str) -> String {
        make_speakable(s, Role::Paragraph, &SpeakableOptions::default())
    }

    fn with(mode: AngleBrackets, s: &str) -> String {
        make_speakable(
            s,
            Role::Paragraph,
            &SpeakableOptions {
                angle_brackets: mode,
                ..Default::default()
            },
        )
    }

    #[test]
    fn comparisons() {
        assert_eq!(smart("if a < b then"), "if a less than b then");
        assert_eq!(smart("x >= 3"), "x greater than or equal to 3");
        assert_eq!(smart("a -> b"), "a arrow b");
    }

    #[test]
    fn tags() {
        assert_eq!(smart("Use <div class=\"x\"> here"), "Use div tag here");
        assert_eq!(smart("</div>"), "closing div tag");
        assert_eq!(smart("a <br/> b"), "a br tag b");
    }

    #[test]
    fn script_injection_is_just_words() {
        let s = smart("<script>alert('xss')</script>");
        assert_eq!(s, "script tag alert('xss') closing script tag");
        for mode in AngleBrackets::ALL {
            let s = with(
                mode,
                "<speak><prosody rate=\"x-fast\">hi</prosody></speak> <",
            );
            assert!(!s.contains('<') && !s.contains('>'), "{mode:?}: {s}");
            assert!(s.contains("hi"));
        }
    }

    #[test]
    fn generics_read_naturally() {
        assert_eq!(smart("Vec<String>"), "Vec of String");
        assert_eq!(
            smart("HashMap<String, Vec<u8>> works"),
            "HashMap of String, Vec of u8 works"
        );
        // Not a generic: unbalanced.
        assert_eq!(smart("a<b"), "a less than b");
    }

    #[test]
    fn other_modes() {
        assert_eq!(
            with(AngleBrackets::Literal, "<b>"),
            "less than b greater than"
        );
        assert_eq!(
            with(AngleBrackets::StripTags, "a <b>bold</b> c"),
            "a bold c"
        );
        assert_eq!(with(AngleBrackets::Silent, "a <b> c"), "a b c");
    }

    #[test]
    fn email_quote_prefix() {
        assert_eq!(smart("> > quoted reply"), "quoted reply");
    }

    #[test]
    fn urls() {
        assert_eq!(
            smart("See https://www.example.com/a/b?c=d for more."),
            "See example.com link for more."
        );
        assert_eq!(smart("<https://rust-lang.org>"), "rust-lang.org link");
        assert_eq!(smart("Docs: https://docs.rs/regex."), "Docs: docs.rs link.");
        let full = make_speakable(
            "at http://a.b/c now",
            Role::Paragraph,
            &SpeakableOptions {
                urls: UrlMode::Full,
                ..Default::default()
            },
        );
        assert_eq!(full, "at http://a.b/c now");
    }

    #[test]
    fn substitutions() {
        let opts = SpeakableOptions {
            substitutions: vec![
                Substitution {
                    pattern: "GPUI".into(),
                    replacement: "G P U I".into(),
                    regex: false,
                    case_sensitive: true,
                    roles: vec![],
                },
                Substitution {
                    pattern: r"v(\d+)\.(\d+)".into(),
                    replacement: "version $1 point $2".into(),
                    regex: true,
                    case_sensitive: false,
                    roles: vec![Role::Heading],
                },
                Substitution {
                    pattern: "(unclosed".into(),
                    replacement: "x".into(),
                    regex: true,
                    case_sensitive: false,
                    roles: vec![],
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            make_speakable("GPUI v1.2", Role::Paragraph, &opts),
            "G P U I v1.2"
        );
        assert_eq!(
            make_speakable("GPUI v1.2", Role::Heading, &opts),
            "G P U I version 1 point 2"
        );
    }

    #[test]
    fn rulers_vanish() {
        assert_eq!(smart("Title ====== end"), "Title end");
    }
}
