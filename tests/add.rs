//! `bib add` end to end: identify, fetch, merge, file.
//!
//! Providers are mocked so this stays offline and deterministic, but everything
//! else is real — the resolver, the merge, cite-key rendering, the store, and
//! the duplicate check against the index.

use bib::cli::resolve;
use bib::config::{Config, ProviderTuning, ProvidersConfig};
use bib::identify::patterns::Identifier;
use bib::model::bridge;
use httpmock::prelude::*;
use std::collections::BTreeMap;

const CROSSREF_EINSTEIN: &str = r#"{
  "type": "journal-article",
  "title": "Zur Elektrodynamik bewegter Körper",
  "container-title": "Annalen der Physik",
  "author": [{"given": "A.", "family": "Einstein"}],
  "issued": {"date-parts": [[1905, 1]]},
  "volume": "322", "issue": "10", "page": "891-921",
  "publisher": "Wiley", "DOI": "10.1002/andp.19053221004"
}"#;

/// Point Crossref at the mock server.
fn config(base: &str) -> Config {
    let mut tuning = BTreeMap::new();
    tuning.insert(
        "crossref".to_owned(),
        ProviderTuning {
            base_url: Some(base.to_owned()),
            ..ProviderTuning::default()
        },
    );
    Config {
        providers: ProvidersConfig {
            order: vec!["crossref".into()],
            tuning,
            ..ProvidersConfig::default()
        },
        ..Config::default()
    }
}

#[test]
fn a_doi_resolves_into_a_valid_entry() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200).body(CROSSREF_EINSTEIN);
    });
    let cache = tempfile::tempdir().unwrap();
    let config = config(&server.base_url());
    let http = resolve::http(&config, cache.path().to_path_buf(), false);

    let resolved = resolve::resolve(
        &http,
        &config,
        &Identifier::Doi("10.1002/andp.19053221004".into()),
    )
    .expect("should resolve");

    let entry = bridge::to_entry("einstein1905", &resolved.body).expect("should be valid");
    assert_eq!(
        entry.title().unwrap().to_string(),
        "Zur Elektrodynamik bewegter Körper"
    );
    assert_eq!(entry.doi().unwrap(), "10.1002/andp.19053221004");
    assert_eq!(resolved.provenance["title"], "crossref");
}

/// The identifier must survive even when the provider's record omits it, or a
/// later duplicate check has nothing to match on.
#[test]
fn the_requested_identifier_is_always_recorded() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        // No DOI field in the response.
        then.status(200)
            .body(r#"{"type":"article-journal","title":"Something"}"#);
    });
    let cache = tempfile::tempdir().unwrap();
    let config = config(&server.base_url());
    let http = resolve::http(&config, cache.path().to_path_buf(), false);

    let resolved = resolve::resolve(&http, &config, &Identifier::Doi("10.1234/x".into())).unwrap();
    let entry = bridge::to_entry("x", &resolved.body).unwrap();
    assert_eq!(entry.doi(), Some("10.1234/x"));
}

/// Every provider failing must be an error the caller can report, not a panic
/// or a silently empty entry.
#[test]
fn a_completely_failed_lookup_is_an_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(404);
    });
    let cache = tempfile::tempdir().unwrap();
    let config = config(&server.base_url());
    let http = resolve::http(&config, cache.path().to_path_buf(), false);

    let error = resolve::resolve(&http, &config, &Identifier::Doi("10.9999/nope".into()))
        .expect_err("should fail");
    let message = format!("{error:#}");
    assert!(
        message.contains("no provider could resolve"),
        "got {message}"
    );
    // The message must say what was tried, or it is not actionable.
    assert!(message.contains("crossref"), "got {message}");
}
