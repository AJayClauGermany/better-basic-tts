//! Sentence segmentation.
//!
//! Sentences are the unit of navigation (previous / next) and of synthesis,
//! so the splitter errs on the side of *not* splitting in doubtful cases
//! ("Dr. Smith", "e.g. this", "3.14", "J. K. Rowling").

/// Sentences longer than this are cut at a clause boundary so that skipping
/// and speed changes stay responsive and engines never receive huge inputs.
const MAX_SENTENCE_CHARS: usize = 320;

/// A line shorter than this that is followed by a line break is taken to be a
/// deliberate line (title, list entry, verse) rather than hard-wrapped prose.
const SHORT_LINE_CHARS: usize = 60;

const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "cf", "no",
    "fig", "eq", "vol", "approx", "inc", "ltd", "co", "corp", "dept", "est", "gen", "gov", "hon",
    "rev", "sgt", "col", "capt", "lt", "mt", "jan", "feb", "mar", "apr", "jun", "jul", "aug",
    "sep", "sept", "oct", "nov", "dec", "a.m", "p.m", "u.s", "u.k", "ph.d", "al",
];

pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for unit in join_wrapped_lines(text) {
        for s in split_unit(&unit) {
            for piece in split_long(&s) {
                let piece = piece.trim();
                if !piece.is_empty() {
                    out.push(piece.to_string());
                }
            }
        }
    }
    out
}

/// Decide for each line break whether it is a soft wrap (join with a space)
/// or a real boundary.
fn join_wrapped_lines(text: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if current.is_empty() {
            current.push_str(line);
        } else {
            current.push(' ');
            current.push_str(line);
        }
        let ends_sentence = line
            .trim_end_matches(['"', '\'', ')', ']', '”', '’', '»'])
            .ends_with(['.', '!', '?', ':', '…', '。', '！', '？']);
        if ends_sentence || line.chars().count() < SHORT_LINE_CHARS {
            units.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

fn is_closer(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '»' | '*' | '_'
    )
}

fn split_unit(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if !is_terminator(c) {
            i += 1;
            continue;
        }
        // Swallow runs of terminators ("?!", "...") and trailing closers.
        let mut end = i + 1;
        while end < chars.len() && is_terminator(chars[end]) {
            end += 1;
        }
        while end < chars.len() && is_closer(chars[end]) {
            end += 1;
        }
        let cjk = matches!(c, '。' | '！' | '？');
        let at_end = end >= chars.len();
        let followed_by_space = !at_end && chars[end].is_whitespace();
        if !(at_end || followed_by_space || cjk) {
            i = end;
            continue;
        }
        if c == '.' && end == i + 1 && !at_end && !should_break_at_period(&chars, start, i, end) {
            i = end;
            continue;
        }
        // An ellipsis followed by a lower case word is a pause, not an end.
        let dots_only = chars[i..end].iter().all(|&c| c == '.' || c == '…');
        if dots_only && (end > i + 1 || c == '…') {
            let next = chars[end..].iter().find(|c| !c.is_whitespace());
            if next.is_some_and(|c| c.is_lowercase()) {
                i = end;
                continue;
            }
        }
        out.push(chars[start..end].iter().collect::<String>());
        start = end;
        i = end;
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect::<String>());
    }
    out
}

/// `i` is the index of a single '.', `end` the index right after it.
fn should_break_at_period(chars: &[char], start: usize, i: usize, end: usize) -> bool {
    // The word before the period.
    let mut w = i;
    while w > start && !chars[w - 1].is_whitespace() {
        w -= 1;
    }
    let word: String = chars[w..i].iter().collect();
    let word_lc = word
        .trim_start_matches(['(', '[', '"', '\'', '“'])
        .to_lowercase();

    // What follows.
    let mut n = end;
    while n < chars.len() && chars[n].is_whitespace() {
        n += 1;
    }
    let next = chars.get(n).copied();

    if ABBREVIATIONS.contains(&word_lc.as_str()) {
        return false;
    }
    // Single-letter initials: "J. K. Rowling".
    if word_lc.chars().count() == 1 && word.chars().all(|c| c.is_uppercase()) {
        return false;
    }
    // List numbering at the start of a sentence: "1. first".
    if w == start && !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match next {
        // "this is v2. and then" — lower case continuation means no break.
        Some(c) if c.is_lowercase() => false,
        _ => true,
    }
}

/// Cut an over-long sentence at clause boundaries, then at spaces.
fn split_long(sentence: &str) -> Vec<String> {
    if sentence.chars().count() <= MAX_SENTENCE_CHARS {
        return vec![sentence.to_string()];
    }
    let chars: Vec<char> = sentence.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while chars.len() - start > MAX_SENTENCE_CHARS {
        let window_end = start + MAX_SENTENCE_CHARS;
        let min = start + MAX_SENTENCE_CHARS / 3;
        let mut cut = None;
        for j in (min..window_end).rev() {
            if matches!(chars[j], ';' | ',' | ':' | '—' | '–' | '，' | '；')
                && chars.get(j + 1).is_none_or(|c| c.is_whitespace())
            {
                cut = Some(j + 1);
                break;
            }
        }
        if cut.is_none() {
            for j in (min..window_end).rev() {
                if chars[j].is_whitespace() {
                    cut = Some(j + 1);
                    break;
                }
            }
        }
        let cut = cut.unwrap_or(window_end);
        out.push(chars[start..cut].iter().collect::<String>());
        start = cut;
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect::<String>());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(
            split_sentences("Hello world. How are you? Fine!"),
            vec!["Hello world.", "How are you?", "Fine!"]
        );
    }

    #[test]
    fn abbreviations_and_numbers() {
        assert_eq!(
            split_sentences("Dr. Smith paid $3.50 to J. K. Rowling, e.g. yesterday. Done."),
            vec![
                "Dr. Smith paid $3.50 to J. K. Rowling, e.g. yesterday.",
                "Done."
            ]
        );
    }

    #[test]
    fn quotes_and_ellipsis() {
        assert_eq!(
            split_sentences("She said \"Stop.\" Then... nothing. Really?!"),
            vec!["She said \"Stop.\"", "Then... nothing.", "Really?!"]
        );
    }

    #[test]
    fn file_names_and_urls_do_not_split() {
        assert_eq!(
            split_sentences("Open main.rs and visit example.com/a.b today."),
            vec!["Open main.rs and visit example.com/a.b today."]
        );
    }

    #[test]
    fn short_lines_are_boundaries_long_lines_wrap() {
        assert_eq!(
            split_sentences("Title\nSubtitle"),
            vec!["Title", "Subtitle"]
        );
        let wrapped = "This is a long line of prose that was hard wrapped by an editor at eighty\ncolumns and continues here.";
        assert_eq!(split_sentences(wrapped).len(), 1);
    }

    #[test]
    fn long_sentences_are_cut() {
        let long = "word ".repeat(200);
        let parts = split_sentences(&long);
        assert!(parts.len() >= 3);
        assert!(parts
            .iter()
            .all(|p| p.chars().count() <= MAX_SENTENCE_CHARS));
        assert_eq!(parts.join(" ").split_whitespace().count(), 200);
    }

    #[test]
    fn cjk() {
        assert_eq!(split_sentences("你好。再见！"), vec!["你好。", "再见！"]);
    }

    #[test]
    fn numbered_item_keeps_number() {
        assert_eq!(split_sentences("1. First thing"), vec!["1. First thing"]);
    }
}
