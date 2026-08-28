//! Downloading documents, and refusing what is not one.
//!
//! The central case is not a network error. It is a publisher answering **HTTP
//! 200 with `content-type: application/pdf`** and a body that is an HTML
//! consent page — verified live against a Wiley `pdfdirect` URL that OpenAlex
//! reports as `pdf_url`. Trusting the status code or the header means writing
//! HTML into `paper.pdf`.

use bib::providers::Http;
use bib::providers::fetch::{FetchError, Limits, PdfSource, candidates, download, download_first};
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
        Limits::new(10 * 1024 * 1024, TIMEOUT),
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
    let error = download(
        &Http::new(),
        &server.url("/x.pdf"),
        &dest,
        Limits::new(1 << 20, TIMEOUT),
    )
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
        Limits::new(64 * 1024, TIMEOUT),
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
            Limits::new(1 << 20, TIMEOUT)
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

    let used = download_first(&Http::new(), &sources, &dest, Limits::new(1 << 20, TIMEOUT))
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
        Limits::new(1 << 20, TIMEOUT),
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
        Limits::new(1 << 20, TIMEOUT),
    )
    .expect_err("offline must not fetch");
    assert!(matches!(error, FetchError::Http { .. }), "got {error}");
}

/// Fetching a library is dozens of requests to one host in a row, and arXiv
/// asks callers to leave a gap between them. The gap is per host and per
/// client, so it is the `Http` that has to remember it.
#[test]
fn downloads_to_one_host_are_paced() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(pdf_bytes(64));
    });

    let temp = tempfile::tempdir().unwrap();
    let http = Http::new();
    let limits = Limits::new(1 << 20, TIMEOUT).paced(Duration::from_millis(300));

    let started = std::time::Instant::now();
    for n in 0..3 {
        download(
            &http,
            &server.url(format!("/{n}.pdf")),
            &temp.path().join(format!("{n}.pdf")),
            limits,
        )
        .expect("each download should succeed");
    }

    // Three requests, two gaps: the first is never delayed.
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "downloads were not paced: {:?}",
        started.elapsed()
    );
}

/// Politeness to one service must not slow down another.
#[test]
fn a_different_host_does_not_wait() {
    let a = MockServer::start();
    let b = MockServer::start();
    for server in [&a, &b] {
        server.mock(|when, then| {
            when.method(GET);
            then.status(200).body(pdf_bytes(64));
        });
    }

    let temp = tempfile::tempdir().unwrap();
    let http = Http::new();
    let limits = Limits::new(1 << 20, TIMEOUT).paced(Duration::from_secs(30));

    let started = std::time::Instant::now();
    download(&http, &a.url("/a.pdf"), &temp.path().join("a.pdf"), limits).unwrap();
    download(&http, &b.url("/b.pdf"), &temp.path().join("b.pdf"), limits).unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a second host waited on the first: {:?}",
        started.elapsed()
    );
}

#[test]
fn arxiv_is_preferred_over_a_publisher_link() {
    let sources = candidates(
        Some("1706.03762"),
        Some("https://onlinelibrary.example/pdfdirect/10.1/x"),
        None,
    );
    assert_eq!(sources[0].url, "https://arxiv.org/pdf/1706.03762");
}

/// The failure that sent a whole imported library home empty-handed: the record
/// carries its *journal* DOI and links to the arXiv landing page, so every URL
/// on offer is HTML — while the PDF is one path segment away.
#[test]
fn a_published_paper_still_reaches_its_preprint_pdf() {
    let sources = candidates(None, None, Some("http://arxiv.org/abs/0802.1919"));
    assert_eq!(sources[0].url, "https://arxiv.org/pdf/0802.1919");
    assert_eq!(sources[0].origin, "url");
}

/// End to end over HTTP: a landing page that is not a PDF, and the rewritten
/// address that is. Only the rewrite gets a file onto disk.
#[test]
fn the_rewritten_address_is_what_actually_downloads() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/abs/0802.1919");
        then.status(200)
            .header("content-type", "text/html")
            .body("<!DOCTYPE html><html><title>[0802.1919] Random Quantum Circuits</title>");
    });
    server.mock(|when, then| {
        when.method(GET).path("/pdf/0802.1919");
        then.status(200).body(pdf_bytes(256));
    });

    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("paper.pdf");
    let sources = vec![
        PdfSource {
            url: server.url("/pdf/0802.1919"),
            origin: "url",
        },
        PdfSource {
            url: server.url("/abs/0802.1919"),
            origin: "url",
        },
    ];

    let used = download_first(&Http::new(), &sources, &dest, Limits::new(1 << 20, TIMEOUT))
        .expect("the rewritten address should serve a PDF");
    assert!(used.url.ends_with("/pdf/0802.1919"));
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF-"));
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
        Limits::new(50 * 1024 * 1024, Duration::from_secs(60)),
    )
    .expect("arxiv should serve a PDF");
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF-"));
}
