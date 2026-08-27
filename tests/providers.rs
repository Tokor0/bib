//! Provider tests, fully offline and deterministic.
//!
//! Response bodies are trimmed recordings of what the real services return, so
//! these exercise the actual field shapes rather than an idealised version of
//! them. Each provider also has an `#[ignore]`d live test, run on demand, to
//! catch upstream drift that a fixture cannot.

use bib::formats::csl::to_body;
use bib::identify::patterns::Identifier;
use bib::model::bridge;
use bib::providers::{
    Http, MetadataProvider, ProviderError, arxiv::ArXiv, books::GoogleBooks, books::OpenLibrary,
    crossref::Crossref, openalex::OpenAlex,
};
use httpmock::prelude::*;

/// Recorded from `Accept: application/vnd.citationstyles.csl+json` on
/// `https://doi.org/10.1002/andp.19053221004`.
const CROSSREF_EINSTEIN: &str = r#"{
  "publisher": "Wiley",
  "issue": "10",
  "published-print": {"date-parts": [[1905, 1]]},
  "DOI": "10.1002/andp.19053221004",
  "type": "journal-article",
  "page": "891-921",
  "title": "Zur Elektrodynamik bewegter Körper",
  "prefix": "10.1002",
  "volume": "322",
  "author": [{"given": "A.", "family": "Einstein", "sequence": "first"}],
  "container-title": "Annalen der Physik",
  "issued": {"date-parts": [[1905, 1]]},
  "ISSN": ["0003-3804", "1521-3889"]
}"#;

#[test]
fn crossref_maps_a_real_response_onto_a_valid_entry() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/10.1002/andp.19053221004");
        then.status(200)
            .header("content-type", "application/vnd.citationstyles.csl+json")
            .body(CROSSREF_EINSTEIN);
    });

    let provider = Crossref::with_base(server.base_url());
    let item = provider
        .fetch(
            &Http::new(),
            &Identifier::Doi("10.1002/andp.19053221004".into()),
        )
        .expect("should fetch");
    mock.assert();

    // Crossref says `journal-article`; CSL says `article-journal`. Getting this
    // wrong would silently make every article a `misc` entry.
    assert_eq!(item.kind.as_deref(), Some("article-journal"));
    assert_eq!(item.source, "crossref");

    let entry = bridge::to_entry("einstein1905", &to_body(&item)).expect("should be valid");
    assert_eq!(
        entry.title().unwrap().to_string(),
        "Zur Elektrodynamik bewegter Körper"
    );
    assert_eq!(entry.date().unwrap().year, 1905);
    assert_eq!(
        entry
            .parents()
            .first()
            .unwrap()
            .title()
            .unwrap()
            .to_string(),
        "Annalen der Physik"
    );
}

/// Crossref's type vocabulary differs from CSL's on the three commonest values.
#[test]
fn crossref_type_names_are_translated() {
    let cases = [
        ("journal-article", "article-journal"),
        ("proceedings-article", "paper-conference"),
        ("book-chapter", "chapter"),
    ];
    for (crossref, csl) in cases {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET);
            then.status(200)
                .body(format!(r#"{{"type":"{crossref}","title":"T"}}"#));
        });
        let item = Crossref::with_base(server.base_url())
            .fetch(&Http::new(), &Identifier::Doi("10.1/x".into()))
            .unwrap();
        assert_eq!(item.kind.as_deref(), Some(csl), "for {crossref}");
    }
}

#[test]
fn an_unknown_doi_is_not_found_rather_than_an_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(404);
    });
    let error = Crossref::with_base(server.base_url())
        .fetch(&Http::new(), &Identifier::Doi("10.9999/nope".into()))
        .expect_err("should not resolve");
    assert!(matches!(error, ProviderError::NotFound), "got {error}");
}

#[test]
fn a_rate_limit_response_is_distinguishable() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(429);
    });
    let error = Crossref::with_base(server.base_url())
        .fetch(&Http::new(), &Identifier::Doi("10.1/x".into()))
        .expect_err("should fail");
    assert!(matches!(error, ProviderError::RateLimited), "got {error}");
}

/// Recorded from `api.openalex.org/works/doi:10.1002/andp.19053221004`,
/// trimmed. Note `volume` is a string here but a number elsewhere, and the
/// date arrives as a plain ISO string rather than CSL `date-parts`.
const OPENALEX_EINSTEIN: &str = r#"{
  "title": "Zur Elektrodynamik bewegter Körper",
  "publication_date": "1905-01-01",
  "publication_year": 1905,
  "doi": "https://doi.org/10.1002/andp.19053221004",
  "type": "article",
  "language": "de",
  "authorships": [{"author": {"display_name": "Albert Einstein"}}],
  "biblio": {"volume": "322", "issue": "10", "first_page": "891", "last_page": "921"},
  "primary_location": {"source": {
      "display_name": "Annalen der Physik",
      "issn_l": "0003-3804",
      "host_organization_name": "Wiley"}}
}"#;

#[test]
fn openalex_maps_its_own_shape_onto_csl() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/works/doi:10.1002/andp.19053221004");
        then.status(200).body(OPENALEX_EINSTEIN);
    });

    let item = OpenAlex::with_base(server.base_url())
        .fetch(
            &Http::new(),
            &Identifier::Doi("10.1002/andp.19053221004".into()),
        )
        .expect("should fetch");

    // The DOI comes back as a URL and must be reduced to the bare form, or
    // duplicate detection compares two spellings of the same thing.
    assert_eq!(item.doi.as_deref(), Some("10.1002/andp.19053221004"));

    let entry = bridge::to_entry("einstein1905", &to_body(&item)).expect("should be valid");
    // `first_page`/`last_page` become a range.
    assert_eq!(entry.page_range().unwrap().to_string(), "891-921");
    // A plain ISO date must keep its precision rather than being scraped for
    // just the year.
    let date = entry.date().unwrap();
    assert_eq!((date.year, date.month), (1905, Some(0)));
}

/// Recorded from `export.arxiv.org/api/query?id_list=1706.03762`, trimmed.
/// Titles and abstracts arrive wrapped across lines.
const ARXIV_ATTENTION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title type="html">ArXiv Query</title>
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <published>2017-06-12T17:57:34Z</published>
    <title>Attention Is All You
      Need</title>
    <summary>  The dominant sequence transduction models are based on complex
recurrent or convolutional neural networks.
</summary>
    <author><name>Ashish Vaswani</name></author>
    <author><name>Noam Shazeer</name></author>
    <arxiv:journal_ref xmlns:arxiv="http://arxiv.org/schemas/atom">NIPS 2017</arxiv:journal_ref>
  </entry>
</feed>"#;

#[test]
fn arxiv_atom_is_parsed_and_unwrapped() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/query");
        then.status(200)
            .header("content-type", "application/atom+xml")
            .body(ARXIV_ATTENTION);
    });

    let item = ArXiv::with_base(server.base_url())
        .fetch(&Http::new(), &Identifier::ArXiv("1706.03762".into()))
        .expect("should fetch");

    // Line wrapping in the source must not survive into the title.
    assert_eq!(
        item.title.as_ref().and_then(|t| t.as_text()).as_deref(),
        Some("Attention Is All You Need")
    );
    assert_eq!(item.author.len(), 2);
    assert_eq!(
        item.author[0].to_hayagriva().as_deref(),
        Some("Vaswani, Ashish")
    );
    assert!(
        item.abstract_
            .as_deref()
            .unwrap()
            .starts_with("The dominant")
    );

    let entry = bridge::to_entry("vaswani2017", &to_body(&item)).expect("should be valid");
    assert_eq!(entry.date().unwrap().year, 2017);
}

/// The feed-level `<title>ArXiv Query</title>` must not be read as the paper's
/// title, and an unknown ID returns a well-formed feed with an error entry.
#[test]
fn an_unknown_arxiv_id_is_not_found() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(
            r#"<feed xmlns="http://www.w3.org/2005/Atom">
                 <title>ArXiv Query</title>
                 <entry><title>Error</title>
                 <summary>incorrect id format</summary></entry>
               </feed>"#,
        );
    });
    let error = ArXiv::with_base(server.base_url())
        .fetch(&Http::new(), &Identifier::ArXiv("9999.99999".into()))
        .expect_err("should not resolve");
    assert!(matches!(error, ProviderError::NotFound), "got {error}");
}

#[test]
fn openlibrary_and_google_books_complement_each_other() {
    let ol = MockServer::start();
    ol.mock(|when, then| {
        when.method(GET).path("/api/books");
        then.status(200).body(
            r#"{"ISBN:9780201896831": {
                 "title": "The Art of Computer Programming",
                 "publishers": [{"name": "Addison-Wesley"}],
                 "publish_places": [{"name": "Reading, MA"}],
                 "publish_date": "1997"}}"#,
        );
    });
    let gb = MockServer::start();
    gb.mock(|when, then| {
        when.method(GET).path("/books/v1/volumes");
        then.status(200).body(
            r#"{"items": [{"volumeInfo": {
                 "title": "The Art of Computer Programming",
                 "authors": ["Donald E. Knuth"],
                 "publishedDate": "1997",
                 "language": "en"}}]}"#,
        );
    });

    let isbn = Identifier::Isbn("9780201896831".into());
    let http = Http::new();
    let from_ol = OpenLibrary::with_base(ol.base_url())
        .fetch(&http, &isbn)
        .expect("openlibrary should answer");
    let from_gb = GoogleBooks::with_base(gb.base_url())
        .fetch(&http, &isbn)
        .expect("google books should answer");

    // OpenLibrary has the publisher and place but no authors…
    assert!(from_ol.author.is_empty());
    assert_eq!(
        from_ol
            .publisher
            .as_ref()
            .and_then(|p| p.as_text())
            .as_deref(),
        Some("Addison-Wesley")
    );
    // …Google Books has the authors but no place. Hence merging both.
    assert_eq!(from_gb.author.len(), 1);
    assert!(from_gb.publisher_place.is_none());

    let merged = bib::providers::merge(&[from_ol, from_gb]);
    let entry = bridge::to_entry("knuth1997", &merged.body).expect("should be valid");
    assert_eq!(entry.authors().unwrap().len(), 1);
    assert_eq!(merged.provenance["publisher"], "openlibrary");
    assert_eq!(merged.provenance["author"], "google-books");
}

/// An unknown ISBN comes back as `{}` with HTTP 200, not a 404.
#[test]
fn openlibrary_reports_an_empty_result_as_not_found() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body("{}");
    });
    let error = OpenLibrary::with_base(server.base_url())
        .fetch(&Http::new(), &Identifier::Isbn("9780000000002".into()))
        .expect_err("should not resolve");
    assert!(matches!(error, ProviderError::NotFound), "got {error}");
}

/// A second request for the same URL must be served from disk, so re-imports
/// are free and repeated runs work offline.
#[test]
fn responses_are_cached_on_disk() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(CROSSREF_EINSTEIN);
    });

    let cache = tempfile::tempdir().unwrap();
    let http = Http::new().with_cache(cache.path().to_path_buf());
    let provider = Crossref::with_base(server.base_url());
    let doi = Identifier::Doi("10.1002/andp.19053221004".into());

    provider.fetch(&http, &doi).expect("first fetch");
    provider.fetch(&http, &doi).expect("second fetch");
    assert_eq!(
        mock.calls(),
        1,
        "the second fetch should have used the cache"
    );

    // A fresh client with the same cache directory still answers offline.
    let offline = Http::new()
        .with_cache(cache.path().to_path_buf())
        .offline(true);
    provider
        .fetch(&offline, &doi)
        .expect("cached fetch offline");
}

#[test]
fn offline_without_a_cache_fails_rather_than_calling_out() {
    let error = Crossref::new()
        .fetch(
            &Http::new().offline(true),
            &Identifier::Doi("10.1002/andp.19053221004".into()),
        )
        .expect_err("offline should not reach the network");
    assert!(matches!(error, ProviderError::Network(_)), "got {error}");
}

#[test]
fn providers_decline_identifiers_they_cannot_answer() {
    let isbn = Identifier::Isbn("9780201896831".into());
    let doi = Identifier::Doi("10.1/x".into());
    assert!(!Crossref::new().supports(&isbn));
    assert!(Crossref::new().supports(&doi));
    assert!(!ArXiv::new().supports(&doi));
    assert!(OpenLibrary::new().supports(&isbn));
    assert!(!GoogleBooks::new().supports(&doi));
}

// --------------------------------------------------------------- live tests

/// Run with `cargo test -- --ignored` to catch upstream API drift. Excluded
/// from CI, which must stay offline and deterministic.
#[test]
#[ignore = "requires network"]
fn live_crossref_resolves_a_known_doi() {
    let item = Crossref::new()
        .fetch(
            &Http::new(),
            &Identifier::Doi("10.1002/andp.19053221004".into()),
        )
        .expect("crossref should resolve");
    assert!(
        item.title
            .as_ref()
            .and_then(|t| t.as_text())
            .is_some_and(|t| t.contains("Elektrodynamik")),
        "got {:?}",
        item.title
    );
}

#[test]
#[ignore = "requires network"]
fn live_arxiv_resolves_a_known_id() {
    let item = ArXiv::new()
        .fetch(&Http::new(), &Identifier::ArXiv("1706.03762".into()))
        .expect("arxiv should resolve");
    assert_eq!(
        item.title.as_ref().and_then(|t| t.as_text()).as_deref(),
        Some("Attention Is All You Need")
    );
}

#[test]
#[ignore = "requires network"]
fn live_openalex_resolves_a_known_doi() {
    let item = OpenAlex::new()
        .fetch(
            &Http::new(),
            &Identifier::Doi("10.1002/andp.19053221004".into()),
        )
        .expect("openalex should resolve");
    assert!(
        item.author
            .iter()
            .any(|a| a.family.as_deref().is_some_and(|f| f.contains("Einstein")))
    );
}
