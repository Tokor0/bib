//! Typst label validity.
//!
//! A cite key is only useful if the user can write `@key` in their document,
//! and that is a stricter condition than "valid YAML mapping key" or "valid
//! hayagriva entry" — both of which accept arbitrary strings. Typst's lexer
//! (`typst-syntax/src/lexer.rs`) applies one predicate to both `<label>` and
//! `@ref`:
//!
//! ```text
//! fn is_valid_in_label_literal(c: char) -> bool {
//!     is_id_continue(c) || matches!(c, ':' | '.')
//! }
//! // is_id_continue(c) = is_xid_continue(c) || c == '_' || c == '-'
//! ```
//!
//! and `ref_marker()` then *un-eats* trailing `.` and `:` so that `@key.` at
//! the end of a sentence resolves to `key`. A key ending in either character
//! is therefore unreferenceable as written: `@smith2020jr.` silently looks up
//! `smith2020jr`, which does not exist.
//!
//! There is no restriction on the first character — labels are not
//! identifiers, so `@1905einstein` is fine.

use std::fmt;
use unicode_normalization::UnicodeNormalization;

/// Characters Typst accepts inside a label literal.
///
/// Note this is *not* [`char::is_alphanumeric`], which additionally admits the
/// `No`/`Nl` categories (`²`, `½`, `①`). Those are not `XID_Continue`, so a key
/// containing one is truncated by Typst's lexer at that character.
pub fn is_valid_char(c: char) -> bool {
    unicode_ident::is_xid_continue(c) || matches!(c, '_' | '-' | ':' | '.')
}

/// Why a string cannot be used as a Typst label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelProblem {
    Empty,
    /// A character Typst's lexer stops at, truncating the reference.
    InvalidChar {
        index: usize,
        ch: char,
    },
    /// `.` or `:` at the end, which `ref_marker` strips from the reference.
    TrailingPunctuation {
        ch: char,
    },
}

impl fmt::Display for LabelProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "label is empty"),
            Self::InvalidChar { index, ch } => write!(
                f,
                "{ch:?} at byte {index} is not valid in a Typst label; \
                 `@key` would stop reading there"
            ),
            Self::TrailingPunctuation { ch } => write!(
                f,
                "trailing {ch:?} is stripped from `@key`, so the reference \
                 resolves to a different, nonexistent key"
            ),
        }
    }
}

/// Check that `label` can be written as `@label` in Typst markup.
pub fn validate(label: &str) -> Result<(), LabelProblem> {
    if label.is_empty() {
        return Err(LabelProblem::Empty);
    }
    if let Some((index, ch)) = label.char_indices().find(|(_, c)| !is_valid_char(*c)) {
        return Err(LabelProblem::InvalidChar { index, ch });
    }
    // Checked last: a key of only dots is better reported as invalid-trailing
    // than as anything else, and an earlier invalid char is the bigger problem.
    match label.chars().next_back() {
        Some(ch @ ('.' | ':')) => Err(LabelProblem::TrailingPunctuation { ch }),
        _ => Ok(()),
    }
}

pub fn is_valid(label: &str) -> bool {
    validate(label).is_ok()
}

/// Coerce `raw` into a valid Typst label, capped at `max_chars` characters.
///
/// The result is always NFC. **Typst compares labels bytewise**, so a
/// decomposed key is unreferenceable by anyone typing normally — editors emit
/// NFC, and `@müller2020` in NFC does not match a stored `mu\u{308}ller2020`,
/// with an error message that prints the two identically. Composing here means
/// `[citekey] normalize` chooses what gets *folded* (NFKD still turns `²` into
/// `2` and `ﬁ` into `fi`) while the key that lands on disk stays typeable.
///
/// Truncation happens after composing, so the cap counts characters as a
/// reader sees them, and before the trailing-punctuation trim, because the cap
/// is what puts a `.` at the end in the first place. May return an empty
/// string if nothing survives; callers decide what that means.
pub fn sanitize(raw: &str, max_chars: usize) -> String {
    let kept: String = raw
        .chars()
        .filter(|c| is_valid_char(*c))
        .nfc()
        .take(max_chars)
        .collect();
    kept.trim_end_matches(['.', ':']).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_keys() {
        for key in [
            "einstein1905electrodynamics",
            "knuth1997art",
            "smith-jones_2020",
            "a",
            "1905einstein", // labels are not identifiers: digits may lead
            "müller2020",   // XID_Continue covers unicode letters
            "o\u{308}2020", // NFD: combining diaeresis is Mn, so XID_Continue
            "rfc:8446",     // interior colon is fine
            "v1.2",         // interior period is fine
        ] {
            assert!(is_valid(key), "{key:?} should be a valid Typst label");
        }
    }

    #[test]
    fn rejects_alphanumeric_but_not_xid_continue() {
        // The exact divergence between char::is_alphanumeric and XID_Continue:
        // these are all `is_alphanumeric() == true`.
        for ch in ['²', '½', '①'] {
            assert!(ch.is_alphanumeric(), "{ch:?} premise: is_alphanumeric");
            assert!(!is_valid_char(ch), "{ch:?} must not pass the Typst rule");
        }
        assert_eq!(
            validate("r²statistics"),
            Err(LabelProblem::InvalidChar { index: 1, ch: '²' })
        );
    }

    #[test]
    fn rejects_trailing_punctuation() {
        assert_eq!(
            validate("kubrick1964dr."),
            Err(LabelProblem::TrailingPunctuation { ch: '.' })
        );
        assert_eq!(
            validate("rfc:"),
            Err(LabelProblem::TrailingPunctuation { ch: ':' })
        );
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert_eq!(validate(""), Err(LabelProblem::Empty));
        assert!(!is_valid_char(' '));
        assert!(!is_valid("a b"));
    }

    #[test]
    fn sanitize_trims_after_truncating() {
        // The cap is what exposes the '.', so the trim has to come second.
        assert_eq!(sanitize("kubrick1964dr.strangelove", 14), "kubrick1964dr");
        assert_eq!(sanitize("r²statistics", 64), "rstatistics");
        assert_eq!(sanitize("a b\tc", 64), "abc");
        assert_eq!(sanitize("...", 64), "");
    }

    #[test]
    fn sanitize_output_is_always_valid_or_empty() {
        for raw in ["r²statistics", "kubrick1964dr.", "...", "a b", "müller"] {
            let out = sanitize(raw, 48);
            assert!(
                out.is_empty() || is_valid(&out),
                "sanitize({raw:?}) = {out:?}, which is not a valid label"
            );
        }
    }

    /// Typst compares labels bytewise, so a decomposed key cannot be cited by
    /// a user whose editor emits NFC — and the resulting error prints the two
    /// spellings identically. Verified against the real binary in
    /// `tests/typst.rs`; this pins the invariant cheaply.
    #[test]
    fn sanitize_always_composes() {
        let decomposed = "mu\u{308}ller2020";
        assert_eq!(sanitize(decomposed, 64), "m\u{fc}ller2020");
        // Idempotent: already-composed input is untouched.
        assert_eq!(sanitize("m\u{fc}ller2020", 64), "m\u{fc}ller2020");
    }

    /// Composing before truncating means `max_chars` counts characters the way
    /// a reader does, not base-plus-mark pairs.
    #[test]
    fn the_length_cap_counts_composed_characters() {
        assert_eq!(sanitize("mu\u{308}ller", 3), "m\u{fc}l");
    }
}
