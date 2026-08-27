//! The identification pipeline, driven entirely by recorded poppler output.
//!
//! No PDFs and no poppler: the `Fixture` backend replays what the real tools
//! would have printed. That is the point of the backend split — these tests
//! exercise the parsers, the tier ordering and the scoring, run in
//! microseconds, and stay hermetic. It also avoids committing publishers' PDFs
//! to the repository.

use bib::config::PdfConfig;
use bib::identify::backend::{Fixture, Op};
use bib::identify::score::{Confidence, Tier};
use bib::identify::{Identification, identify};
use std::path::Path;

fn first_page() -> Op {
    Op::Text {
        first: 1,
        last: 1,
        layout: true,
    }
}

fn run(fixture: Fixture, name: &str) -> Identification {
    identify(&fixture, Path::new(name), &PdfConfig::default())
}

/// Real `pdfinfo` output shape: the computed fields, no custom keys.
const INFO_31_PAGES: &str = "Title:           Zur Elektrodynamik bewegter Körper\nProducer:        Acrobat\nPages:           31\nPage size:       595 x 842 pts\n";

/// The strongest signal: a `doi.org` link the publisher put on page 1.
#[test]
fn a_doi_link_annotation_is_certain() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, INFO_31_PAGES)
            .with(&Op::InfoCustom, "")
            .with(
                &Op::Urls { last: 31 },
                "Page  Type          URL\n   1  Annotation    https://doi.org/10.1002/andp.19053221004\n",
            )
            .with(&Op::Xmp, ""),
        "paper.pdf",
    );

    let best = found.best().expect("should identify");
    assert_eq!(best.id.value(), "10.1002/andp.19053221004");
    assert_eq!(best.tier, Tier::Annotations);
    assert_eq!(best.confidence, Confidence::Certain);
}

/// A resolver link deep in the document is probably a reference the author
/// linked, not the document's own identifier.
#[test]
fn a_doi_link_on_a_later_page_is_weak() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, INFO_31_PAGES)
            .with(&Op::InfoCustom, "")
            .with(
                &Op::Urls { last: 31 },
                "Page  Type          URL\n  17  Annotation    https://doi.org/10.1234/cited\n",
            )
            .with(&Op::Xmp, "")
            .with(&first_page(), "no identifier here\n"),
        "paper.pdf",
    );
    assert_eq!(found.best().unwrap().confidence, Confidence::Low);
}

/// Tiers 1–4 always all run, so two of them agreeing is observable — and that
/// agreement is worth more than either source alone.
#[test]
fn agreement_between_metadata_tiers_is_certain() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Subject: 10.1000/xyz\nPages: 3\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 3 }, "Page  Type          URL\n")
            .with(
                &Op::Xmp,
                r#"<x:xmpmeta><rdf:Description prism:doi="10.1000/xyz"/></x:xmpmeta>"#,
            ),
        "paper.pdf",
    );
    let best = found.best().unwrap();
    assert_eq!(best.id.value(), "10.1000/xyz");
    assert_eq!(best.confidence, Confidence::Certain);
}

#[test]
fn an_arxiv_filename_is_recognised_without_reading_the_pdf() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 12\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 12 }, "")
            .with(&Op::Xmp, ""),
        "2301.12345v2.pdf",
    );
    let best = found.best().expect("filename should identify it");
    assert_eq!(best.id.to_string(), "arxiv:2301.12345v2");
    assert_eq!(best.tier, Tier::Filename);
}

/// DOIs in filenames conventionally replace the slash with an underscore.
#[test]
fn a_doi_filename_with_an_underscore_is_recognised() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 1\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 1 }, "")
            .with(&Op::Xmp, ""),
        "10.1002_andp.19053221004.pdf",
    );
    assert_eq!(found.best().unwrap().id.value(), "10.1002/andp.19053221004");
}

/// The motivating false positive: a paper whose own DOI is nowhere in the
/// metadata, but whose bibliography is full of other people's.
#[test]
fn dois_in_the_references_section_are_not_candidates() {
    let page = "\
Some Paper Title
Author Name

Introduction text with no identifier.

References

[1] Einstein, A. 10.1002/andp.19053221004
[2] Turing, A. 10.1093/mind/LIX.236.433
";
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 1\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 1 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), page),
        "paper.pdf",
    );
    assert!(
        found.candidates.is_empty(),
        "cited DOIs leaked in: {:?}",
        found
            .candidates
            .iter()
            .map(|c| c.id.value())
            .collect::<Vec<_>>()
    );
}

/// The document's own DOI sits in a running footer, so it repeats; the ones it
/// cites appear once each.
#[test]
fn a_doi_repeated_across_pages_outranks_a_cited_one() {
    let mut text = String::new();
    for page in 1..=4 {
        text.push_str(&format!(
            "body of page {page}\nsee also 10.1/other{page}\n10.1234/own.doi\n\u{c}"
        ));
    }
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 4\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 4 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), "page one, nothing useful\n")
            .with(
                &Op::Text {
                    first: 4,
                    last: 4,
                    layout: false,
                },
                "last page\n",
            )
            .with(
                &Op::Text {
                    first: 1,
                    last: 4,
                    layout: false,
                },
                &text,
            ),
        "paper.pdf",
    );

    let best = found.best().expect("should identify");
    assert_eq!(best.id.value(), "10.1234/own.doi");
    assert_eq!(best.confidence, Confidence::High);
}

/// arXiv stamps the identifier down the left margin, rotated; `pdftotext`
/// frequently emits it one character per line.
#[test]
fn a_vertical_arxiv_stamp_is_read() {
    let page = "Attention Is All You Need\n\na\nr\nX\ni\nv\n:\n1\n7\n0\n6\n.\n0\n3\n7\n6\n2\nv\n5\n\nAbstract\n";
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 1\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 1 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), page),
        "downloaded.pdf",
    );
    assert_eq!(found.best().unwrap().id.to_string(), "arxiv:1706.03762v5");
}

/// Books put the ISBN on the copyright page, which is not page 1 and has no
/// DOI anywhere near it.
#[test]
fn a_book_is_identified_by_its_copyright_page_isbn() {
    let front = "\
The Art of Computer Programming
\u{c}
Copyright 1997 by Addison-Wesley

ISBN 0-201-89683-4 (set)
Printed in the United States of America
\u{c}
Contents
";
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 650\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 650 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), "The Art of Computer Programming\n")
            .with(
                &Op::Text {
                    first: 650,
                    last: 650,
                    layout: false,
                },
                "Index\n",
            )
            .with(
                &Op::Text {
                    first: 1,
                    last: 6,
                    layout: false,
                },
                front,
            ),
        "book.pdf",
    );
    let best = found.best().expect("should find the ISBN");
    // Normalised to ISBN-13 so lookups compare one representation.
    assert_eq!(best.id.to_string(), "isbn:9780201896831");
    assert_eq!(best.tier, Tier::FrontMatter);
}

/// A soft hyphen or a line break inside a DOI must not hide it.
#[test]
fn a_doi_broken_by_extraction_is_still_found() {
    let page = "Published online. doi: 10.1002/an\u{00ad}dp.19053221004\n";
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Pages: 1\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 1 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), page),
        "paper.pdf",
    );
    assert_eq!(found.best().unwrap().id.value(), "10.1002/andp.19053221004");
}

/// Once the metadata tiers are conclusive, the expensive tiers must not run —
/// the fixture has no text recorded, so reading it would fail the test.
#[test]
fn conclusive_metadata_skips_text_extraction() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, INFO_31_PAGES)
            .with(&Op::InfoCustom, "")
            .with(
                &Op::Urls { last: 31 },
                "Page  Type          URL\n   1  Annotation    https://doi.org/10.1234/x\n",
            )
            .with(&Op::Xmp, ""),
        "paper.pdf",
    );
    assert!(found.best().is_some());
    assert!(
        found.notes.iter().any(|n| n.contains("text was not read")),
        "notes: {:?}",
        found.notes
    );
}

/// Every tier failing is a normal outcome, reported rather than raised.
#[test]
fn a_pdf_that_yields_nothing_is_not_an_error() {
    let found = run(
        Fixture::new()
            .failing(&Op::Info, "May not be a PDF file")
            .failing(&Op::InfoCustom, "May not be a PDF file")
            .failing(&Op::Urls { last: 1 }, "May not be a PDF file"),
        "broken.pdf",
    );
    assert!(found.candidates.is_empty());
    assert!(
        found.notes.iter().any(|n| n.contains("May not be a PDF")),
        "the failure should be explained: {:?}",
        found.notes
    );
}

/// The title is kept even when no identifier is found: it feeds the
/// search-by-title fallback.
#[test]
fn the_title_survives_for_the_search_fallback() {
    let found = run(
        Fixture::new()
            .with(&Op::Info, "Title:  A Paper With No DOI\nPages: 1\n")
            .with(&Op::InfoCustom, "")
            .with(&Op::Urls { last: 1 }, "")
            .with(&Op::Xmp, "")
            .with(&first_page(), "nothing identifying here\n"),
        "paper.pdf",
    );
    assert!(found.candidates.is_empty());
    assert_eq!(found.title.as_deref(), Some("A Paper With No DOI"));
}
