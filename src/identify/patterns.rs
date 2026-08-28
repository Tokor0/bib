//! Recognising and validating DOIs, arXiv IDs and ISBNs.
//!
//! Matching is the easy half. The hard half is rejecting things that look like
//! identifiers and are not — a page of digits contains plenty of ten-digit runs
//! — which is what the checksum and shape rules here are for.

use regex::Regex;
use std::fmt;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Identifier {
    /// Stored lowercased: DOIs are case-insensitive by specification.
    Doi(String),
    /// Without any `arXiv:` prefix; version suffix preserved when present.
    ArXiv(String),
    /// Normalised to ISBN-13, digits only.
    Isbn(String),
    Pmid(String),
}

impl Identifier {
    /// Parse a DOI, returning `None` if the text is not one.
    pub fn parse_doi(raw: &str) -> Option<Self> {
        normalize_doi(raw).map(Self::Doi)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Doi(_) => "doi",
            Self::ArXiv(_) => "arxiv",
            Self::Isbn(_) => "isbn",
            Self::Pmid(_) => "pmid",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Doi(v) | Self::ArXiv(v) | Self::Isbn(v) | Self::Pmid(v) => v,
        }
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind(), self.value())
    }
}

/// Parse `doi:10.x`, `arxiv:2301.12345`, `isbn:978…`, or a bare identifier.
pub fn parse_identifier(input: &str) -> Option<Identifier> {
    let trimmed = input.trim();
    if let Some((prefix, rest)) = trimmed.split_once(':') {
        let rest = rest.trim();
        match prefix.to_ascii_lowercase().as_str() {
            "doi" => return normalize_doi(rest).map(Identifier::Doi),
            "arxiv" => return normalize_arxiv(rest).map(Identifier::ArXiv),
            "isbn" => return normalize_isbn(rest).map(Identifier::Isbn),
            "pmid" => {
                return rest
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| Identifier::Pmid(rest.to_owned()));
            }
            _ => {}
        }
    }
    // A bare value: try each shape, most specific first.
    if let Some(doi) = normalize_doi(trimmed) {
        return Some(Identifier::Doi(doi));
    }
    if let Some(arxiv) = normalize_arxiv(trimmed) {
        return Some(Identifier::ArXiv(arxiv));
    }
    normalize_isbn(trimmed).map(Identifier::Isbn)
}

// --------------------------------------------------------------------- DOI

static DOI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"10\.\d{4,9}/[-._;()/:A-Za-z0-9<>\[\]]+").expect("DOI pattern should compile")
});

/// Every DOI in `text`, in order of appearance, deduplicated.
pub fn find_dois(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for m in DOI.find_iter(text) {
        let doi = trim_doi(m.as_str());
        if doi.len() > 8 && !found.contains(&doi) {
            found.push(doi);
        }
    }
    found
}

pub fn normalize_doi(raw: &str) -> Option<String> {
    // Accept a full URL, which is how DOIs appear in link annotations.
    let stripped = raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("dx.")
        .trim_start_matches("doi.org/")
        .trim_start_matches("www.doi.org/");
    let m = DOI.find(stripped)?;
    // Anchored: a DOI must start where the text does, or this is a substring
    // of something else and the caller should be scanning, not normalising.
    if m.start() != 0 {
        return None;
    }
    let doi = trim_doi(m.as_str());
    (doi.len() > 8).then_some(doi)
}

/// Strip punctuation a sentence left attached to the DOI.
///
/// Parentheses need care: they are legal *inside* DOIs, so a closing one is
/// only dropped when it has no opener — otherwise `10.1000/foo(bar)` loses its
/// tail.
fn trim_doi(raw: &str) -> String {
    let mut doi = raw.to_ascii_lowercase();
    while let Some(last) = doi.chars().last() {
        let drop = match last {
            '.' | ',' | ';' | ':' | '"' | '\'' | '-' => true,
            ')' => doi.matches('(').count() < doi.matches(')').count(),
            ']' => doi.matches('[').count() < doi.matches(']').count(),
            '>' => doi.matches('<').count() < doi.matches('>').count(),
            _ => false,
        };
        if !drop {
            break;
        }
        doi.pop();
    }
    doi
}

// ------------------------------------------------------------------- arXiv

/// With an explicit `arXiv:` marker — the reliable form.
static ARXIV_MARKED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)arxiv[:\s]?\s*((?:\d{4}\.\d{4,5}(?:v\d+)?)|(?:[a-z][a-z-]+(?:\.[A-Z]{2})?/\d{7}(?:v\d+)?))")
        .expect("arXiv pattern should compile")
});

/// The bare identifier shape, with no marker.
static ARXIV_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(\d{4}\.\d{4,5}(?:v\d+)?)|([a-z][a-z-]+(?:\.[A-Z]{2})?/\d{7}(?:v\d+)?))$")
        .expect("bare arXiv pattern should compile")
});

/// arXiv IDs carrying an explicit marker.
///
/// Only the marked form is scanned in body text: `2301.12345` on its own is
/// indistinguishable from a section number, a price or a date, and treating
/// every such run as an arXiv ID produces far more wrong answers than right
/// ones. The bare form is accepted only from filenames and explicit input,
/// where the user has effectively vouched for it.
pub fn find_arxiv(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for caps in ARXIV_MARKED.captures_iter(text) {
        let id = caps[1].to_owned();
        if !found.contains(&id) {
            found.push(id);
        }
    }
    found
}

/// An arXiv link in any of the forms the site serves.
///
/// `/abs/` is the landing page, `/pdf/` the file itself, and records carry
/// whichever one their source happened to store — so the host and path shape
/// are matched rather than a fixed prefix list, and the ID is validated
/// afterwards like any other.
static ARXIV_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:https?://)?(?:www\.|export\.)?arxiv\.org/(?:abs|pdf|format)/(.+?)(?:\.pdf)?/?$",
    )
    .expect("arXiv URL pattern should compile")
});

/// The arXiv ID a URL points at, whatever form the link takes.
///
/// Separate from [`normalize_arxiv`] because the caller's question is
/// different: not "is this an identifier" but "does this link name a paper we
/// can ask arXiv for directly". A record's stored URL is nearly always the
/// `/abs/` landing page, which is HTML; the PDF is one path segment away, and
/// this is what finds it.
pub fn arxiv_in_url(url: &str) -> Option<String> {
    let id = ARXIV_URL.captures(url.trim())?.get(1)?.as_str();
    ARXIV_BARE.is_match(id).then(|| id.to_owned())
}

pub fn normalize_arxiv(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(id) = arxiv_in_url(trimmed) {
        return Some(id);
    }
    let without_prefix = trimmed
        .trim_start_matches("arXiv:")
        .trim_start_matches("arxiv:")
        .trim_end_matches(".pdf")
        .trim();
    ARXIV_BARE
        .is_match(without_prefix)
        .then(|| without_prefix.to_owned())
}

// -------------------------------------------------------------------- ISBN

/// Candidate ISBN shapes: 10 or 13 characters of digits, hyphens and spaces.
static ISBN_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:isbn[-—: ]*(?:1[03][-: ]*)?)?((?:97[89][- ]?)?[\dX][\dX
 -]{8,20}[\dX])\b",
    )
    .expect("ISBN pattern should compile")
});

/// Every checksum-valid ISBN in `text`, normalised to ISBN-13.
///
/// The checksum is doing nearly all the work: bare digit runs of the right
/// length are common on a copyright page, and roughly nine in ten fail it.
pub fn find_isbns(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for caps in ISBN_CANDIDATE.captures_iter(text) {
        if let Some(isbn) = normalize_isbn(&caps[1])
            && !found.contains(&isbn)
        {
            found.push(isbn);
        }
    }
    found
}

/// Validate and convert to ISBN-13. `None` if the checksum fails.
pub fn normalize_isbn(raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, 'X' | 'x'))
        .collect();
    match digits.len() {
        10 if isbn10_checksum_ok(&digits) => Some(isbn10_to_13(&digits)),
        13 if isbn13_checksum_ok(&digits) => Some(digits),
        _ => None,
    }
}

/// Weighted sum mod 11, weights 10 down to 1, with `X` meaning 10.
fn isbn10_checksum_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().enumerate() {
        let value = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'X' | 'x' if i == 9 => 10,
            _ => return false,
        };
        sum += value * (10 - i as u32);
    }
    sum % 11 == 0
}

/// Weighted sum mod 10, weights alternating 1 and 3.
fn isbn13_checksum_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().enumerate() {
        let Some(value) = c.to_digit(10) else {
            return false;
        };
        sum += value * if i % 2 == 0 { 1 } else { 3 };
    }
    sum % 10 == 0
}

fn isbn10_to_13(isbn10: &str) -> String {
    let body: String = format!("978{}", &isbn10[..9]);
    let mut sum = 0u32;
    for (i, c) in body.chars().enumerate() {
        sum += c.to_digit(10).unwrap_or(0) * if i % 2 == 0 { 1 } else { 3 };
    }
    let check = (10 - (sum % 10)) % 10;
    format!("{body}{check}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dois_are_found_and_lowercased() {
        let text = "Available at DOI 10.1002/ANDP.19053221004 online.";
        assert_eq!(find_dois(text), ["10.1002/andp.19053221004"]);
    }

    /// A DOI at the end of a sentence picks up the full stop.
    #[test]
    fn trailing_sentence_punctuation_is_stripped() {
        assert_eq!(find_dois("see 10.1000/xyz123."), ["10.1000/xyz123"]);
        assert_eq!(find_dois("(10.1000/xyz123),"), ["10.1000/xyz123"]);
    }

    /// Parentheses are legal inside DOIs, so a balanced pair must survive.
    #[test]
    fn balanced_parentheses_stay_in_the_doi() {
        assert_eq!(
            find_dois("10.1000/foo(bar)baz and more"),
            ["10.1000/foo(bar)baz"]
        );
        assert_eq!(find_dois("(see 10.1000/plain)"), ["10.1000/plain"]);
    }

    #[test]
    fn doi_urls_normalise_to_the_bare_doi() {
        for url in [
            "https://doi.org/10.1002/andp.19053221004",
            "http://dx.doi.org/10.1002/andp.19053221004",
            "10.1002/andp.19053221004",
        ] {
            assert_eq!(
                normalize_doi(url).as_deref(),
                Some("10.1002/andp.19053221004"),
                "normalising {url}"
            );
        }
    }

    #[test]
    fn arxiv_needs_a_marker_in_body_text() {
        assert_eq!(find_arxiv("arXiv:2301.12345v2 [cs.CL]"), ["2301.12345v2"]);
        assert_eq!(find_arxiv("arXiv: 2301.12345"), ["2301.12345"]);
        assert_eq!(find_arxiv("math-ph/0703012 alone"), Vec::<String>::new());
        // Old-style identifiers, with the marker.
        assert_eq!(find_arxiv("arXiv:math.GT/0309136"), ["math.GT/0309136"]);
    }

    /// A bare `2301.12345` in body text is far more often a section number or a
    /// price than an arXiv ID.
    #[test]
    fn bare_numbers_are_not_treated_as_arxiv_ids() {
        assert!(find_arxiv("see section 2301.12345 for details").is_empty());
        // …but the same string is accepted where a human vouched for it.
        assert_eq!(normalize_arxiv("2301.12345").as_deref(), Some("2301.12345"));
    }

    #[test]
    fn arxiv_urls_and_filenames_normalise() {
        assert_eq!(
            normalize_arxiv("https://arxiv.org/abs/2301.12345").as_deref(),
            Some("2301.12345")
        );
        assert_eq!(
            normalize_arxiv("arXiv:2301.12345v3").as_deref(),
            Some("2301.12345v3")
        );
    }

    /// Every shape a stored URL actually takes. `http://arxiv.org/abs/…` is the
    /// one that matters most: it is what papis and Zotero record, and it is a
    /// landing page, so a fetch that trusts it downloads HTML.
    #[test]
    fn an_arxiv_link_yields_its_id_whatever_form_it_takes() {
        for url in [
            "http://arxiv.org/abs/0802.1919",
            "https://arxiv.org/abs/0802.1919",
            "https://www.arxiv.org/abs/0802.1919",
            "https://export.arxiv.org/abs/0802.1919",
            "https://arxiv.org/pdf/0802.1919",
            "https://arxiv.org/pdf/0802.1919.pdf",
            "https://arxiv.org/abs/0802.1919/",
        ] {
            assert_eq!(arxiv_in_url(url).as_deref(), Some("0802.1919"), "{url}");
        }
        // The version is part of the ID and must survive.
        assert_eq!(
            arxiv_in_url("https://arxiv.org/abs/2405.00781v2").as_deref(),
            Some("2405.00781v2")
        );
        // Old-style identifiers carry a slash of their own.
        assert_eq!(
            arxiv_in_url("https://arxiv.org/abs/math.GT/0309136").as_deref(),
            Some("math.GT/0309136")
        );
    }

    #[test]
    fn a_link_that_is_not_an_arxiv_paper_yields_nothing() {
        assert!(arxiv_in_url("https://arxiv.org/list/quant-ph/new").is_none());
        assert!(arxiv_in_url("https://example.org/abs/2301.12345").is_none());
        assert!(arxiv_in_url("https://doi.org/10.1007/s00220-009-0873-6").is_none());
    }

    #[test]
    fn isbn10_checksums_are_verified() {
        // Real ISBN-10s.
        assert!(normalize_isbn("0201896834").is_some());
        assert!(normalize_isbn("0-201-89683-4").is_some());
        assert!(normalize_isbn("080442957X").is_some());
        // One digit changed.
        assert!(normalize_isbn("0201896835").is_none());
        assert!(normalize_isbn("0201896844").is_none());
    }

    #[test]
    fn isbn13_checksums_are_verified() {
        assert!(normalize_isbn("9780201896831").is_some());
        assert!(normalize_isbn("978-0-201-89683-1").is_some());
        assert!(normalize_isbn("9780201896832").is_none());
    }

    /// ISBN-10 is normalised to 13 so lookups and duplicate checks compare one
    /// representation rather than two.
    #[test]
    fn isbn10_converts_to_isbn13() {
        assert_eq!(
            normalize_isbn("0201896834").as_deref(),
            Some("9780201896831")
        );
    }

    /// The point of the checksum: a copyright page is full of digit runs.
    #[test]
    fn random_digit_runs_are_rejected() {
        let page = "Printed 1997. 1234567890 0987654321 5551234567 \
                    Library of Congress 97-12345";
        assert!(find_isbns(page).is_empty(), "found {:?}", find_isbns(page));
    }

    #[test]
    fn isbns_are_found_on_a_copyright_page() {
        let page = "First published 1997\nISBN 0-201-89683-4 (v. 1)\nPrinted in the USA";
        assert_eq!(find_isbns(page), ["9780201896831"]);
    }

    #[test]
    fn prefixed_input_selects_the_kind() {
        assert_eq!(
            parse_identifier("doi:10.1002/andp.19053221004"),
            Some(Identifier::Doi("10.1002/andp.19053221004".into()))
        );
        assert_eq!(
            parse_identifier("arxiv:2301.12345"),
            Some(Identifier::ArXiv("2301.12345".into()))
        );
        assert_eq!(
            parse_identifier("isbn:0201896834"),
            Some(Identifier::Isbn("9780201896831".into()))
        );
        assert_eq!(parse_identifier("nonsense"), None);
    }
}
