//! The identification pipeline against the *real* poppler binaries.
//!
//! `tests/identify.rs` replays recorded output, which makes it fast and
//! hermetic but means it can only ever confirm what the fixtures assert. That
//! is not hypothetical: the fixtures happily encoded a `Pages:` line in
//! `pdfinfo -custom` output, which that invocation does not actually print — so
//! page count silently read as 1 and every tier that needs the document length
//! was dead. Only running the real tool finds that class of mistake.
//!
//! The PDF is generated with `typst` rather than committed, so there is no
//! binary fixture in the repository and no question about redistributing a
//! publisher's file.

use bib::config::PdfConfig;
use bib::identify::backend::{Op, PdfBackend, PdfError, Poppler};
use bib::identify::{identify, parse};
use std::path::{Path, PathBuf};
use std::process::Command;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success() || o.status.code() == Some(99))
}

/// Skip locally, fail in CI — the flake check sets the guard so a missing
/// binary cannot turn this into a silent pass.
fn require_tools() -> bool {
    let available = have("pdfinfo") && have("pdftotext") && typst_available();
    if !available {
        assert!(
            std::env::var_os("BIBTEST_REQUIRE_POPPLER").is_none(),
            "BIBTEST_REQUIRE_POPPLER is set but poppler or typst is missing"
        );
        eprintln!("skipping: poppler or typst is not on PATH");
    }
    available
}

fn typst_available() -> bool {
    Command::new("typst")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Three pages, a DOI in a running footer, and a real link annotation.
const DOCUMENT: &str = r#"
#set page(numbering: "1", footer: [doi:10.1234/running.footer])
#set document(title: "A Generated Test Document")

= A Generated Test Document

#link("https://doi.org/10.5555/link.annotation")[Publisher record]

Body text on page one.

#pagebreak()
Page two body.

#pagebreak()
Page three body.
"#;

fn build_pdf(dir: &Path) -> PathBuf {
    let source = dir.join("doc.typ");
    let pdf = dir.join("doc.pdf");
    std::fs::write(&source, DOCUMENT).unwrap();
    let output = Command::new("typst")
        .args(["compile", source.to_str().unwrap(), pdf.to_str().unwrap()])
        .output()
        .expect("typst should run");
    assert!(
        output.status.success(),
        "typst failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    pdf
}

/// The regression that motivated this file: `Pages` must survive the round trip
/// from the real `pdfinfo` through our parser.
#[test]
fn page_count_is_read_from_real_pdfinfo_output() {
    if !require_tools() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let pdf = build_pdf(temp.path());
    let backend = Poppler::new(PdfConfig::default());

    let text = backend.run(&pdf, &Op::Info).expect("pdfinfo should run");
    let info = parse::info(&text);
    assert_eq!(
        parse::page_count(&info),
        Some(3),
        "page count not parsed from:\n{text}"
    );

    // …and `-custom` must not be relied on for it, which is the actual bug.
    let custom = backend
        .run(&pdf, &Op::InfoCustom)
        .expect("pdfinfo -custom should run");
    assert!(
        parse::page_count(&parse::info(&custom)).is_none(),
        "`-custom` unexpectedly reports Pages; the two-call split may be \
         unnecessary, but check before removing it:\n{custom}"
    );
}

#[test]
fn link_annotations_are_parsed_from_real_output() {
    if !require_tools() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let pdf = build_pdf(temp.path());
    let backend = Poppler::new(PdfConfig::default());

    let text = backend
        .run(&pdf, &Op::Urls { last: 3 })
        .expect("pdfinfo -url should run");
    let urls = parse::urls(&text);
    assert!(
        urls.iter()
            .any(|(page, uri)| *page == 1 && uri.contains("doi.org/10.5555/link.annotation")),
        "expected the link on page 1, got {urls:?} from:\n{text}"
    );
}

/// End to end with real tools: the pipeline should reach the link annotation
/// and rank it top.
#[test]
fn the_pipeline_identifies_a_real_pdf() {
    if !require_tools() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let pdf = build_pdf(temp.path());

    let found = identify(
        &Poppler::new(PdfConfig::default()),
        &pdf,
        &PdfConfig::default(),
    );
    let best = found
        .best()
        .unwrap_or_else(|| panic!("nothing identified; notes: {:?}", found.notes));
    assert_eq!(best.id.value(), "10.5555/link.annotation");
    assert_eq!(found.title.as_deref(), Some("A Generated Test Document"));
}

/// A damaged file must degrade to a soft failure, not abort or hang.
#[test]
fn a_truncated_pdf_fails_softly() {
    if !require_tools() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let pdf = build_pdf(temp.path());

    let mut bytes = std::fs::read(&pdf).unwrap();
    bytes.truncate(bytes.len() / 3);
    let broken = temp.path().join("broken.pdf");
    std::fs::write(&broken, bytes).unwrap();

    let config = PdfConfig::default();
    let found = identify(&Poppler::new(config.clone()), &broken, &config);
    assert!(
        !found.notes.is_empty(),
        "a truncated PDF should have produced explanatory notes"
    );

    // Whatever happens, it must be reported rather than raised.
    match Poppler::new(config).run(&broken, &Op::Info) {
        Ok(_) => {}
        Err(PdfError::Failed { .. } | PdfError::Timeout { .. }) => {}
        Err(other) => panic!("unexpected error kind: {other}"),
    }
}

/// A missing binary is a soft failure too, so an absent optional tool degrades
/// rather than aborting an add.
#[test]
fn a_missing_binary_is_reported_not_raised() {
    let config = PdfConfig {
        pdfinfo: Some(PathBuf::from("definitely-not-a-real-binary-xyzzy")),
        ..PdfConfig::default()
    };
    let error = Poppler::new(config)
        .run(Path::new("whatever.pdf"), &Op::Info)
        .expect_err("a missing binary should fail");
    assert!(error.is_tool_missing(), "got {error}");
}
