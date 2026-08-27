//! Working out what a PDF actually is.
//!
//! `bib add paper.pdf` should not require pasting a DOI, so the pipeline tries
//! progressively more expensive sources until one produces an identifier it can
//! validate. The ordering is not just about cost: the cheap sources are also the
//! trustworthy ones, because they are metadata the publisher wrote rather than
//! text that happens to sit on the page.

pub mod backend;
pub mod layout;
pub mod parse;
pub mod patterns;
pub mod repair;
pub mod score;

use crate::config::PdfConfig;
use backend::{Op, PdfBackend, PdfError};
use patterns::Identifier;
use repair::Views;
use score::{Candidate, Confidence, Tier};
use std::path::Path;

/// Everything the pipeline learned, including what it failed at.
#[derive(Debug, Default)]
pub struct Identification {
    /// Best first. Empty is a normal outcome, not an error.
    pub candidates: Vec<Candidate>,
    /// Title recovered from metadata, for the search fallback when no
    /// identifier is found.
    pub title: Option<String>,
    /// One line per tier that ran, for `--explain`. Failures are recorded here
    /// rather than raised, since a missing tier is not a failed identification.
    pub notes: Vec<String>,
}

impl Identification {
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }

    /// Whether the metadata tiers already settled it.
    fn is_conclusive(&self) -> bool {
        self.candidates
            .first()
            .is_some_and(|c| c.confidence >= Confidence::High)
    }
}

/// Run the identification pipeline over `pdf`.
pub fn identify(backend: &dyn PdfBackend, pdf: &Path, config: &PdfConfig) -> Identification {
    let mut found = Identification::default();
    let mut candidates = Vec::new();

    // Tier 1 — the filename. Free, and frequently right for arXiv downloads.
    candidates.extend(from_filename(pdf));

    // Tiers 2–4 cost three `pdfinfo` spawns between them and always all run:
    // cross-tier agreement is the strongest signal available, and stopping at
    // the first hit throws it away.
    // `pdfinfo` twice: once plain for the computed fields (`Pages` above all),
    // once with `-custom` for publisher-defined keys, which is where a DOI
    // often hides. `-custom` prints only the dictionary, so neither call is
    // redundant.
    let mut info = match backend.run(pdf, &Op::Info) {
        Ok(text) => parse::info(&text),
        Err(e) => {
            found.notes.push(note(Tier::DocInfo, &e));
            Default::default()
        }
    };
    let page_count = parse::page_count(&info).unwrap_or(1);
    match backend.run(pdf, &Op::InfoCustom) {
        Ok(text) => info.extend(parse::info(&text)),
        Err(e) => found.notes.push(note(Tier::DocInfo, &e)),
    }
    found.notes.push(format!(
        "doc-info: {page_count} page(s), {} key(s)",
        info.len()
    ));
    candidates.extend(from_info(&info));
    found.title = info.get("Title").filter(|t| !t.is_empty()).cloned();

    match backend.run(pdf, &Op::Urls { last: page_count }) {
        Ok(text) => {
            let urls = parse::urls(&text);
            let hits = from_annotations(&urls);
            found.notes.push(format!(
                "link-annotations: {} link(s), {} candidate(s)",
                urls.len(),
                hits.len()
            ));
            candidates.extend(hits);
        }
        Err(e) => found.notes.push(note(Tier::Annotations, &e)),
    }

    match backend.run(pdf, &Op::Xmp) {
        Ok(text) => {
            let xmp = parse::xmp(&text);
            if found.title.is_none() {
                found.title = xmp.get("title").cloned();
            }
            candidates.extend(from_xmp(&xmp));
        }
        Err(e) => found.notes.push(note(Tier::Xmp, &e)),
    }

    found.candidates = score::rank(candidates.clone());
    if found.is_conclusive() {
        found
            .notes
            .push("metadata tiers were conclusive; text was not read".into());
        return found;
    }

    // Text tiers, in order, stopping once something validates.
    for tier in text_tiers(page_count, config) {
        let (op, kind) = tier;
        match backend.run(pdf, &op) {
            Ok(text) => {
                let before = candidates.len();
                candidates.extend(from_text(&text, kind, page_count));
                found.notes.push(format!(
                    "{kind}: {} candidate(s)",
                    candidates.len() - before
                ));
            }
            Err(e) => found.notes.push(note(kind, &e)),
        }
        found.candidates = score::rank(candidates.clone());
        if found.is_conclusive() {
            return found;
        }
    }

    found.candidates = score::rank(candidates);

    // The title feeds the search fallback, and the info dictionary is empty on
    // every paper built with pdfTeX — so when metadata gave us nothing, read it
    // off the page geometry instead.
    if found.title.is_none() {
        match backend.run(pdf, &Op::BBox { page: 1 }) {
            Ok(xhtml) => {
                found.title = layout::title(&layout::parse(&xhtml));
                if let Some(title) = &found.title {
                    found.notes.push(format!("layout: title \"{title}\""));
                }
            }
            Err(e) => found.notes.push(note(Tier::FirstPage, &e)),
        }
    }
    found
}

fn note(tier: Tier, error: &PdfError) -> String {
    format!("{tier}: {error}")
}

/// Which text tiers to run, and in what order.
fn text_tiers(page_count: usize, config: &PdfConfig) -> Vec<(Op, Tier)> {
    let last_scanned = page_count.min(config.max_scan_pages).max(1);
    let mut tiers = vec![(
        Op::Text {
            first: 1,
            last: 1,
            layout: true,
        },
        Tier::FirstPage,
    )];
    if page_count > 1 {
        tiers.push((
            Op::Text {
                first: page_count,
                last: page_count,
                layout: false,
            },
            Tier::LastPage,
        ));
        // Books put their ISBN on the copyright page, which is never page 1.
        tiers.push((
            Op::Text {
                first: 1,
                last: page_count.min(6),
                layout: false,
            },
            Tier::FrontMatter,
        ));
    }
    tiers.push((
        Op::Text {
            first: 1,
            last: last_scanned,
            layout: false,
        },
        Tier::FullScan,
    ));
    tiers
}

// ------------------------------------------------------------------- tiers

fn from_filename(pdf: &Path) -> Vec<Candidate> {
    let Some(stem) = pdf.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    // Filenames cannot contain `/`, so a DOI in one is conventionally written
    // with the slash replaced. Try both readings.
    let candidates = [stem.to_owned(), stem.replacen('_', "/", 1)];
    for text in candidates {
        if let Some(id) = patterns::parse_identifier(&text) {
            return vec![Candidate {
                id,
                tier: Tier::Filename,
                confidence: Confidence::High,
                context: stem.to_owned(),
            }];
        }
    }
    Vec::new()
}

/// A `doi.org` link target is as close to ground truth as this gets: the
/// publisher put it there, and it points at the document itself.
fn from_annotations(urls: &[(usize, String)]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (page, uri) in urls {
        let lowered = uri.to_ascii_lowercase();
        let is_resolver = lowered.contains("doi.org/") || lowered.contains("arxiv.org/");
        if !is_resolver {
            continue;
        }
        if let Some(id) = patterns::parse_identifier(uri) {
            // A resolver link on page 1 is the document's own; deeper in, it is
            // more likely a reference the author linked.
            let confidence = if *page == 1 {
                Confidence::Certain
            } else {
                Confidence::Low
            };
            found.push(Candidate {
                id,
                tier: Tier::Annotations,
                confidence,
                context: format!("page {page}: {uri}"),
            });
        }
    }
    found
}

/// Keys named after an identifier are authoritative; a DOI merely *mentioned*
/// in the subject line is a good guess.
fn from_info(info: &std::collections::BTreeMap<String, String>) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (key, value) in info {
        let lowered = key.to_ascii_lowercase();
        let named =
            lowered.contains("doi") || lowered.contains("arxiv") || lowered.contains("isbn");
        let confidence = if named {
            Confidence::High
        } else {
            Confidence::Medium
        };
        for id in scan(value) {
            found.push(Candidate {
                id,
                tier: Tier::DocInfo,
                confidence,
                context: format!("{key}: {value}"),
            });
        }
    }
    found
}

fn from_xmp(xmp: &std::collections::BTreeMap<String, String>) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (key, value) in xmp {
        // `prism:doi`, `pdfx:doi`, `crossmark:doi` all reduce to `doi` here.
        let confidence = match key.as_str() {
            "doi" | "arxiv" | "isbn" | "identifier" => Confidence::Certain,
            _ => Confidence::Medium,
        };
        for id in scan(value) {
            found.push(Candidate {
                id,
                tier: Tier::Xmp,
                confidence,
                context: format!("{key}: {value}"),
            });
        }
    }
    found
}

fn from_text(raw: &str, tier: Tier, page_count: usize) -> Vec<Candidate> {
    let pages: Vec<String> = parse::pages(raw).iter().map(|p| (*p).to_owned()).collect();

    // A DOI in a running header or footer repeats; a cited one appears once.
    let mut found = Vec::new();
    if pages.len() >= 3 {
        for doi in score::repeated_across_pages(&pages, 3) {
            found.push(Candidate {
                id: Identifier::Doi(doi.clone()),
                tier,
                confidence: Confidence::High,
                context: format!("repeated across pages ({doi})"),
            });
        }
    }

    // Everything after the references heading describes other works.
    let body = score::before_references(raw);
    let views = Views::new(body);

    for view in views.all() {
        for doi in patterns::find_dois(view) {
            found.push(Candidate {
                id: Identifier::Doi(doi.clone()),
                tier,
                confidence: text_confidence(tier, page_count),
                context: snippet(view, &doi),
            });
        }
        for arxiv in patterns::find_arxiv(view) {
            found.push(Candidate {
                id: Identifier::ArXiv(arxiv.clone()),
                tier,
                // The `arXiv:` marker is required for this match, and arXiv
                // stamps its own papers — so this is stronger than a loose DOI.
                confidence: Confidence::High,
                context: snippet(view, &arxiv),
            });
        }
        if matches!(tier, Tier::FrontMatter | Tier::FullScan | Tier::Ocr) {
            for isbn in patterns::find_isbns(view) {
                found.push(Candidate {
                    id: Identifier::Isbn(isbn.clone()),
                    tier,
                    // The checksum already rejected the plausible impostors.
                    confidence: Confidence::High,
                    context: snippet(view, &isbn),
                });
            }
        }
    }
    found
}

/// A single-page document has no references section to confuse us; a long one
/// scanned in full is the weakest evidence available.
fn text_confidence(tier: Tier, page_count: usize) -> Confidence {
    match tier {
        Tier::FirstPage if page_count == 1 => Confidence::High,
        Tier::FirstPage => Confidence::Medium,
        Tier::LastPage | Tier::FrontMatter => Confidence::Medium,
        _ => Confidence::Low,
    }
}

/// Any identifier inside an arbitrary string.
fn scan(value: &str) -> Vec<Identifier> {
    let mut found: Vec<Identifier> = patterns::find_dois(value)
        .into_iter()
        .map(Identifier::Doi)
        .collect();
    found.extend(
        patterns::find_arxiv(value)
            .into_iter()
            .map(Identifier::ArXiv),
    );
    found.extend(
        patterns::find_isbns(value)
            .into_iter()
            .map(Identifier::Isbn),
    );
    found
}

/// Surrounding text, so `--explain` shows where a candidate came from.
fn snippet(haystack: &str, needle: &str) -> String {
    let Some(at) = haystack.find(needle) else {
        return needle.to_owned();
    };
    let start = haystack[..at]
        .char_indices()
        .rev()
        .nth(30)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = haystack[at..]
        .char_indices()
        .nth(needle.chars().count() + 30)
        .map(|(i, _)| at + i)
        .unwrap_or(haystack.len());
    haystack[start..end]
        .replace(['\n', '\r'], " ")
        .trim()
        .to_owned()
}
