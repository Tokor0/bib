//! Downloading documents, and refusing what is not one.
//!
//! The central case is not a network error. It is a publisher answering **HTTP
//! 200 with `content-type: application/pdf`** and a body that is an HTML
//! consent page — verified live against a Wiley `pdfdirect` URL that OpenAlex
//! reports as `pdf_url`. Trusting the status code or the header means writing
//! HTML into `paper.pdf`.

use bib::identify::patterns::Identifier;
use bib::providers::Http;
use bib::providers::fetch::{FetchError, PdfSource, candidates, download, download_first};
use httpmock::prelude::*;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A minimal but genuine PDF header.
fn pdf_bytes(payload_len: usize) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.resize(bytes.len() + payload_len, b'x');
    bytes.extend_from_slice(b"\n%%EOF\n");
    bytes
}

#[test]
fn a_real_pdf_is_saved() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/paper.pdf");
        then.status(200)
            .header("content-type", "application/pdf")
            .body(pdf_bytes(4096));
    });

    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    download(
        &Http::new(),
        &server.url("/paper.pdf"),
        &dest,
        10 * 1024 * 1024,
        TIMEOUT,
    )
    .expect("a real PDF should download");

    let saved = std::fs::read(&dest).unwrap();
    assert!(saved.starts_with(b"%PDF-"));
    assert_eq!(saved.len(), pdf_bytes(4096).len());
}

/// The motivating case, reproduced exactly.
#[test]
fn an_html_interstitial_served_as_a_pdf_is_rejected() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200)
            // Both of these lie. Only the bytes tell the truth.
            .header("content-type", "application/pdf")
            .body("<!DOCTYPE html><html><body>Please accept cookies</body></html>");
    });

    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    let error = download(&Http::new(), &server.url("/x.pdf"), &dest, 1 << 20, TIMEOUT)
        .expect_err("HTML must not be accepted as a PDF");

    assert!(
        matches!(&error, FetchError::NotAPdf { looked_like, .. } if looked_like == "an HTML page"),
        "got {error}"
    );
    // Nothing may be left behind, or a later run would treat it as an
    // attachment.
    assert!(!dest.exists(), "a file was written for a rejected download");
}

#[test]
fn an_oversized_body_is_refused_and_leaves_nothing_behind() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(pdf_bytes(512 * 1024));
    });

    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    let error = download(
        &Http::new(),
        &server.url("/big.pdf"),
        &dest,
        64 * 1024,
        TIMEOUT,
    )
    .expect_err("an oversized download must be refused");

    assert!(matches!(error, FetchError::TooLarge { .. }), "got {error}");
    assert!(!dest.exists());
}

#[test]
fn a_missing_document_is_reported_not_saved() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(404);
    });
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    assert!(
        download(
            &Http::new(),
            &server.url("/gone.pdf"),
            &dest,
            1 << 20,
            TIMEOUT
        )
        .is_err()
    );
    assert!(!dest.exists());
}

/// Publisher links fail far more often than they succeed, so the first working
/// source must be found rather than the first source attempted.
#[test]
fn the_first_source_that_yields_a_pdf_wins() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/interstitial");
        then.status(200)
            .header("content-type", "application/pdf")
            .body("<!DOCTYPE html><html></html>");
    });
    server.mock(|when, then| {
        when.method(GET).path("/real.pdf");
        then.status(200).body(pdf_bytes(128));
    });

    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    let sources = vec![
        PdfSource {
            url: server.url("/interstitial"),
            origin: "openalex",
        },
        PdfSource {
            url: server.url("/real.pdf"),
            origin: "url",
        },
    ];

    let used = download_first(&Http::new(), &sources, &dest, 1 << 20, TIMEOUT)
        .expect("the second source should succeed");
    assert_eq!(used.origin, "url");
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF-"));
}

/// Every source failing is reported with all the reasons, not just the last:
/// "no PDF" and "the publisher blocked us" are different problems.
#[test]
fn every_failure_is_reported_when_no_source_works() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    let temp = tempfile::tempdir().unwrap();
    let sources = vec![
        PdfSource {
            url: server.url("/a"),
            origin: "openalex",
        },
        PdfSource {
            url: server.url("/b"),
            origin: "url",
        },
    ];
    let failures = download_first(
        &Http::new(),
        &sources,
        &temp.path().join("p.pdf"),
        1 << 20,
        TIMEOUT,
    )
    .expect_err("both should fail");
    assert_eq!(failures.len(), 2);
}

#[test]
fn offline_never_reaches_the_network() {
    let temp = tempfile::tempdir().unwrap();
    let error = download(
        &Http::new().offline(true),
        "https://example.org/x.pdf",
        &temp.path().join("p.pdf"),
        1 << 20,
        TIMEOUT,
    )
    .expect_err("offline must not fetch");
    assert!(matches!(error, FetchError::Http { .. }), "got {error}");
}

#[test]
fn arxiv_is_preferred_over_a_publisher_link() {
    let sources = candidates(
        Some(&Identifier::ArXiv("1706.03762".into())),
        Some("https://onlinelibrary.example/pdfdirect/10.1/x"),
        None,
    );
    assert_eq!(sources[0].url, "https://arxiv.org/pdf/1706.03762");
}

/// A live check that the constraint above is real, not theoretical.
#[test]
#[ignore = "requires network"]
fn live_arxiv_serves_a_real_pdf() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    download(
        &Http::new(),
        "https://arxiv.org/pdf/1706.03762",
        &dest,
        50 * 1024 * 1024,
        Duration::from_secs(60),
    )
    .expect("arxiv should serve a PDF");
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF-"));
}
