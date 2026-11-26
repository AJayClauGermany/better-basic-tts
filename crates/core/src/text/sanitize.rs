//! First pass over raw input: make it boring, predictable Unicode text.

/// Normalise raw input.
///
/// * line endings become `\n`
/// * control characters (other than newline / tab) are removed — this includes
///   escape sequences that some engines or terminals would interpret
/// * invisible formatting characters (zero-width spaces, bidi overrides, BOM)
///   are removed so they cannot hide text from the eye while it is still spoken
/// * typographic variants of spaces become a plain space
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            // Line / paragraph separators.
            '\u{2028}' => out.push('\n'),
            '\u{2029}' => out.push_str("\n\n"),
            // Odd spaces.
            '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => out.push(' '),
            // Zero-width and bidi control characters, BOM, soft hyphen.
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{00AD}' => {}
            // Object replacement / replacement characters carry no speech.
            '\u{FFFC}' | '\u{FFFD}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_invisibles() {
        assert_eq!(sanitize("a\u{1b}[31mb\u{200b}c\u{feff}"), "a[31mbc");
        assert_eq!(sanitize("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(sanitize("a\u{00a0}b\tc"), "a b c");
    }
}
