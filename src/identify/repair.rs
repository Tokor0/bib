//! Undoing what PDF text extraction does to identifiers.
//!
//! Extracted text breaks naive matching in ways that are individually small and
//! collectively fatal: a DOI split across a line break, a ligature swallowing
//! two characters, a soft hyphen that renders as nothing. Everything here is
//! applied before any pattern runs.

/// Normalize the characters that most often corrupt an identifier.
pub fn repair(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            // Ligatures. `ﬁ` inside `10.1002/andp.19053221004` is unlikely, but
            // they are everywhere in titles, which feed the search fallback.
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬀ' => out.push_str("ff"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            // Soft hyphen and zero-width characters render as nothing, so they
            // must not survive into a match.
            '\u{00ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' => {}
            // Various spaces, normalised so whitespace handling stays simple.
            '\u{00a0}' | '\u{2007}' | '\u{202f}' | '\u{2009}' | '\u{200a}' => out.push(' '),
            // Typographic dashes, which publishers use in page ranges and which
            // break numeric parsing downstream.
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' => out.push('-'),
            _ => out.push(c),
        }
    }
    out
}

/// Three readings of the same page, all searched.
///
/// Cheap to produce and each catches something the others miss, so there is no
/// reason to choose between them.
#[derive(Debug, Clone)]
pub struct Views {
    /// Character-repaired, otherwise as extracted.
    pub repaired: String,
    /// Hyphenated line breaks joined, for identifiers split across lines.
    pub joined: String,
    /// All whitespace removed.
    ///
    /// This is the one that catches arXiv's rotated left-margin stamp, which
    /// `pdftotext` frequently emits one character per line.
    pub squeezed: String,
}

impl Views {
    pub fn new(raw: &str) -> Self {
        let repaired = repair(raw);
        let joined = join_broken_lines(&repaired);
        let squeezed: String = repaired.chars().filter(|c| !c.is_whitespace()).collect();
        Self {
            repaired,
            joined,
            squeezed,
        }
    }

    /// Every view, for running a pattern across all of them.
    pub fn all(&self) -> [&str; 3] {
        [&self.repaired, &self.joined, &self.squeezed]
    }
}

/// Join `-\n` (hyphenated break) and, separately, bare `\n` inside what looks
/// like a continuing identifier.
fn join_broken_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' {
            // `-\n` with optional spaces around the newline.
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\r') {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\n' {
                let mut k = j + 1;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                // Only rejoin when the next line continues a word: a hyphen at
                // the end of "peer-\nreviewed" is a break, one before a capital
                // or a digit at the start of a new sentence usually is not.
                if k < chars.len() && chars[k].is_lowercase() {
                    i = k;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ligatures_become_their_letters() {
        assert_eq!(repair("classiﬁcation"), "classification");
        assert_eq!(repair("eﬀective"), "effective");
        assert_eq!(repair("diﬃcult"), "difficult");
    }

    #[test]
    fn invisible_characters_are_removed_not_replaced() {
        // A soft hyphen inside a DOI must vanish, or the DOI does not match.
        assert_eq!(repair("10.1002/an\u{00ad}dp"), "10.1002/andp");
        assert_eq!(repair("a\u{200b}b"), "ab");
    }

    #[test]
    fn exotic_spaces_and_dashes_are_normalised() {
        assert_eq!(repair("891\u{2013}921"), "891-921");
        assert_eq!(repair("a\u{00a0}b"), "a b");
    }

    #[test]
    fn hyphenated_line_breaks_are_rejoined() {
        let views = Views::new("peer-\nreviewed");
        assert_eq!(views.joined, "peerreviewed");
    }

    /// A hyphen that ends a line but is not a word break must stay, or ranges
    /// and compound identifiers get mangled.
    #[test]
    fn a_hyphen_before_a_new_sentence_is_kept() {
        let views = Views::new("pages 891-\nSee also");
        assert!(views.joined.contains("891-"), "got {:?}", views.joined);
    }

    /// The whole point of the squeezed view: arXiv stamps come out vertical.
    #[test]
    fn the_squeezed_view_reassembles_vertical_text() {
        let vertical = "a\nr\nX\ni\nv\n:\n2\n3\n0\n1\n.\n1\n2\n3\n4\n5";
        let views = Views::new(vertical);
        assert_eq!(views.squeezed, "arXiv:2301.12345");
    }
}
