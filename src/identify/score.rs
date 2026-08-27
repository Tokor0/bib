//! Ranking candidates, and the rules that suppress the common wrong answer.
//!
//! The dominant false positive in this problem is not a malformed DOI — it is a
//! *correctly formed* DOI belonging to a work the document cites. Tier order
//! does most of the work; the rules here do the rest.

use super::patterns::Identifier;
use std::collections::BTreeMap;
use std::fmt;

/// Where a candidate came from. Ordered cheapest and most trustworthy first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Filename,
    Annotations,
    DocInfo,
    Xmp,
    FirstPage,
    LastPage,
    FrontMatter,
    FullScan,
    Ocr,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Self::Filename => "filename",
            Self::Annotations => "link-annotations",
            Self::DocInfo => "doc-info",
            Self::Xmp => "xmp",
            Self::FirstPage => "first-page",
            Self::LastPage => "last-page",
            Self::FrontMatter => "front-matter",
            Self::FullScan => "full-scan",
            Self::Ocr => "ocr",
        }
    }

    /// Tiers 1–4 need no text extraction, so they always all run.
    pub fn is_metadata(self) -> bool {
        matches!(
            self,
            Self::Filename | Self::Annotations | Self::DocInfo | Self::Xmp
        )
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Low,
    Medium,
    High,
    /// Near ground truth: a `doi.org` link target, a publisher's own XMP field,
    /// or agreement between two independent tiers.
    Certain,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Certain => "certain",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: Identifier,
    pub tier: Tier,
    pub confidence: Confidence,
    /// Where it was seen, for `bib identify --explain`.
    pub context: String,
}

/// Rank candidates and fold duplicates together.
///
/// Agreement between tiers is the strongest signal available and the reason
/// tiers 1–4 always all run: two independent sources naming the same
/// identifier promotes it to [`Confidence::Certain`], which no single
/// heuristic earns on its own.
pub fn rank(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut tiers_by_id: BTreeMap<Identifier, Vec<Tier>> = BTreeMap::new();
    for candidate in &candidates {
        tiers_by_id
            .entry(candidate.id.clone())
            .or_default()
            .push(candidate.tier);
    }

    for candidate in &mut candidates {
        let tiers = &tiers_by_id[&candidate.id];
        let distinct: std::collections::BTreeSet<_> = tiers.iter().collect();
        if distinct.len() > 1 {
            candidate.confidence = Confidence::Certain;
        }
    }

    // Keep the best-evidenced occurrence of each identifier.
    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then(a.tier.cmp(&b.tier))
            .then(a.id.cmp(&b.id))
    });
    let mut seen = std::collections::BTreeSet::new();
    candidates.retain(|c| seen.insert(c.id.clone()));
    candidates
}

/// Cut text at the references heading.
///
/// Everything after it describes *other* works, so scanning it for the
/// document's own identifier is how a bibliography manager ends up filing a
/// paper under one of its citations.
pub fn before_references(text: &str) -> &str {
    for (offset, line) in line_offsets(text) {
        let trimmed = line.trim().trim_end_matches(':').to_ascii_lowercase();
        let heading =
            trimmed.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ');
        if matches!(
            heading,
            "references"
                | "reference"
                | "bibliography"
                | "works cited"
                | "literature cited"
                | "literatur"
                | "literaturverzeichnis"
        ) {
            return &text[..offset];
        }
    }
    text
}

fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = offset;
        offset += line.len();
        (start, line)
    })
}

/// Identifiers appearing on at least `threshold` distinct pages.
///
/// A DOI in a running header or footer repeats on every page; a cited DOI
/// appears once. This is the cheapest strong signal in the text tiers.
pub fn repeated_across_pages(pages: &[String], threshold: usize) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for page in pages {
        for doi in super::patterns::find_dois(page) {
            *counts.entry(doi).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .map(|(doi, _)| doi)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, tier: Tier, confidence: Confidence) -> Candidate {
        Candidate {
            id: Identifier::Doi(id.into()),
            tier,
            confidence,
            context: String::new(),
        }
    }

    #[test]
    fn agreement_between_tiers_promotes_to_certain() {
        let ranked = rank(vec![
            candidate("10.1/a", Tier::DocInfo, Confidence::Medium),
            candidate("10.1/a", Tier::FirstPage, Confidence::Medium),
        ]);
        assert_eq!(ranked.len(), 1, "duplicates should fold together");
        assert_eq!(ranked[0].confidence, Confidence::Certain);
    }

    /// One tier finding the same thing twice is not corroboration.
    #[test]
    fn repetition_within_one_tier_does_not_promote() {
        let ranked = rank(vec![
            candidate("10.1/a", Tier::FullScan, Confidence::Low),
            candidate("10.1/a", Tier::FullScan, Confidence::Low),
        ]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].confidence, Confidence::Low);
    }

    #[test]
    fn higher_confidence_ranks_first_then_earlier_tiers() {
        let ranked = rank(vec![
            candidate("10.1/low", Tier::FullScan, Confidence::Low),
            candidate("10.1/certain", Tier::Annotations, Confidence::Certain),
            candidate("10.1/high", Tier::DocInfo, Confidence::High),
        ]);
        let ids: Vec<&str> = ranked.iter().map(|c| c.id.value()).collect();
        assert_eq!(ids, ["10.1/certain", "10.1/high", "10.1/low"]);
    }

    #[test]
    fn text_is_cut_at_the_references_heading() {
        let text = "Body mentions 10.1/own.\n\nReferences\n\n[1] 10.1/cited\n";
        let kept = before_references(text);
        assert!(kept.contains("10.1/own"));
        assert!(!kept.contains("10.1/cited"));
    }

    /// Headings are numbered and capitalised in all sorts of ways.
    #[test]
    fn reference_headings_are_recognised_in_common_forms() {
        for heading in [
            "References",
            "REFERENCES",
            "  references  ",
            "5. References",
            "Bibliography",
            "Works Cited",
            "Literaturverzeichnis",
        ] {
            let text = format!("body\n{heading}\ncited 10.1/x\n");
            assert!(
                !before_references(&text).contains("10.1/x"),
                "{heading:?} was not recognised"
            );
        }
    }

    /// "Reference frame" is not a references section.
    #[test]
    fn a_line_merely_containing_the_word_is_not_a_heading() {
        let text = "In the reference frame of the observer 10.1/x\n";
        assert!(before_references(text).contains("10.1/x"));
    }

    #[test]
    fn a_doi_in_a_running_footer_is_detected() {
        let pages: Vec<String> = (0..5)
            .map(|i| format!("page {i} content\n10.1002/andp.19053221004\n"))
            .collect();
        assert_eq!(
            repeated_across_pages(&pages, 3),
            ["10.1002/andp.19053221004"]
        );
    }

    #[test]
    fn a_doi_cited_once_is_not_a_running_footer() {
        let pages = vec![
            "body\n".to_owned(),
            "see 10.1/cited\n".to_owned(),
            "more\n".to_owned(),
        ];
        assert!(repeated_across_pages(&pages, 3).is_empty());
    }
}
