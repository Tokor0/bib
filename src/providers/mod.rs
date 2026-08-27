//! Fetching authoritative metadata for an identifier.
//!
//! Synchronous, deliberately. The workload is two or three strictly sequential
//! requests per document — resolve, then optionally enrich — and arXiv's terms
//! cap us at one request every three seconds regardless. Paying for an async
//! runtime and boxed futures to serialize requests would be pure cost, so this
//! uses blocking `ureq`, which also keeps `rustls` + `webpki-roots` as the only
//! TLS story: no `openssl-sys`, no `pkg-config`, and no dependency on a system
//! CA bundle.

pub mod arxiv;
pub mod books;
pub mod crossref;
pub mod fetch;
pub mod openalex;
pub mod search;

use crate::config::ProvidersConfig;
use crate::formats::csl::CslItem;
use crate::identify::patterns::Identifier;
use crate::util::fnv1a_str;
use serde::de::DeserializeOwned;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum ProviderError {
    /// The provider has no record of this identifier. Not an error for the
    /// pipeline: the next provider gets a turn.
    NotFound,
    /// The identifier is not one this provider can answer.
    Unsupported,
    RateLimited,
    Status(u16),
    Network(String),
    Parse(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no record found"),
            Self::Unsupported => write!(f, "identifier not supported"),
            Self::RateLimited => write!(f, "rate limited"),
            Self::Status(code) => write!(f, "HTTP {code}"),
            Self::Network(message) => write!(f, "network error: {message}"),
            Self::Parse(message) => write!(f, "could not parse response: {message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub trait MetadataProvider {
    fn name(&self) -> &'static str;
    fn supports(&self, id: &Identifier) -> bool;
    fn fetch(&self, http: &Http, id: &Identifier) -> Result<CslItem, ProviderError>;
}

/// Shared HTTP access: caching, rate limiting, and one place that knows how to
/// turn a response into an error.
pub struct Http {
    agent: ureq::Agent,
    cache: Option<PathBuf>,
    /// Contact address for Crossref's polite pool. Only ever sent when the user
    /// has explicitly configured it.
    mailto: Option<String>,
    last_call: Mutex<BTreeMap<&'static str, Instant>>,
    offline: bool,
}

impl Http {
    pub fn new() -> Self {
        Self {
            agent: ureq::Agent::new_with_defaults(),
            cache: None,
            mailto: None,
            last_call: Mutex::new(BTreeMap::new()),
            offline: false,
        }
    }

    /// Cap how long any single request may take.
    ///
    /// Separate from the search budget below: this stops one hung connection
    /// from consuming the whole budget, so the remaining providers still get a
    /// turn.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .build(),
        );
        self
    }

    pub fn with_cache(mut self, dir: PathBuf) -> Self {
        self.cache = Some(dir);
        self
    }

    pub fn with_mailto(mut self, mailto: Option<String>) -> Self {
        self.mailto = mailto;
        self
    }

    /// Answer only from cache, never touching the network.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    pub fn mailto(&self) -> Option<&str> {
        self.mailto.as_deref()
    }

    /// GET `url`, returning the body as text.
    ///
    /// `rate` is the minimum interval between calls to `provider`; arXiv's
    /// terms of use require one request per three seconds, and honouring that
    /// is not optional.
    pub fn get(
        &self,
        provider: &'static str,
        url: &str,
        accept: &str,
        rate: Duration,
    ) -> Result<String, ProviderError> {
        let cache_path = self.cache_path(provider, url, accept);
        if let Some(path) = &cache_path
            && let Ok(cached) = std::fs::read_to_string(path)
        {
            return Ok(cached);
        }
        if self.offline {
            return Err(ProviderError::Network("offline".into()));
        }

        self.wait_for(provider, rate);

        let user_agent = match &self.mailto {
            Some(mailto) => format!(
                "bib/{} (https://github.com/; mailto:{mailto})",
                env!("CARGO_PKG_VERSION")
            ),
            None => format!("bib/{}", env!("CARGO_PKG_VERSION")),
        };

        let response = self
            .agent
            .get(url)
            .header("Accept", accept)
            .header("User-Agent", &user_agent)
            .call();

        let mut response = match response {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(404 | 410)) => return Err(ProviderError::NotFound),
            Err(ureq::Error::StatusCode(429)) => return Err(ProviderError::RateLimited),
            Err(ureq::Error::StatusCode(code)) => return Err(ProviderError::Status(code)),
            Err(e) => return Err(ProviderError::Network(e.to_string())),
        };

        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if let Some(path) = cache_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // A cache that cannot be written costs time, not correctness.
            let _ = std::fs::write(&path, &body);
        }
        Ok(body)
    }

    /// A streaming reader for a response body.
    ///
    /// Unlike [`Self::get`] this is never cached: a PDF belongs in the library
    /// directory, not in the HTTP cache, and streaming lets the caller reject a
    /// non-PDF or an oversized body without buffering it first.
    pub fn get_reader(
        &self,
        url: &str,
        accept: &str,
        timeout: Duration,
    ) -> Result<Box<dyn std::io::Read + Send + Sync>, ProviderError> {
        if self.offline {
            return Err(ProviderError::Network("offline".into()));
        }
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .build(),
        );
        let response = agent
            .get(url)
            .header("Accept", accept)
            // Our own name, deliberately. Presenting as a browser to get past
            // an interstitial would be circumventing an access control, not
            // fetching an open-access document.
            .header("User-Agent", &format!("bib/{}", env!("CARGO_PKG_VERSION")))
            .call();

        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(404 | 410)) => return Err(ProviderError::NotFound),
            Err(ureq::Error::StatusCode(429)) => return Err(ProviderError::RateLimited),
            Err(ureq::Error::StatusCode(code)) => return Err(ProviderError::Status(code)),
            Err(e) => return Err(ProviderError::Network(e.to_string())),
        };
        Ok(Box::new(response.into_body().into_reader()))
    }

    pub fn get_json<T: DeserializeOwned>(
        &self,
        provider: &'static str,
        url: &str,
        accept: &str,
        rate: Duration,
    ) -> Result<T, ProviderError> {
        let body = self.get(provider, url, accept, rate)?;
        serde_json::from_str(&body).map_err(|e| ProviderError::Parse(e.to_string()))
    }

    fn cache_path(&self, provider: &str, url: &str, accept: &str) -> Option<PathBuf> {
        let dir = self.cache.as_ref()?;
        // The accept header is part of the key: the same URL serves CSL-JSON
        // and BibTeX depending on it.
        let key = fnv1a_str(&format!("{provider}|{url}|{accept}"));
        Some(dir.join(format!("{provider}-{key:016x}.txt")))
    }

    fn wait_for(&self, provider: &'static str, rate: Duration) {
        if rate.is_zero() {
            return;
        }
        let mut last = self.last_call.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = last.get(provider) {
            let elapsed = previous.elapsed();
            if elapsed < rate {
                std::thread::sleep(rate - elapsed);
            }
        }
        last.insert(provider, Instant::now());
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------- merge

/// A merged record plus the provenance of every field.
#[derive(Debug, Default)]
pub struct Merged {
    pub body: Value,
    /// Field name to the provider that supplied it, recorded into
    /// `x-bib.provenance` so `bib update` can re-fetch selectively and a wrong
    /// value can be traced to its source.
    pub provenance: BTreeMap<String, String>,
    pub consulted: Vec<String>,
}

/// Merge provider results field by field, earlier providers winning.
///
/// Merging happens on the hayagriva body rather than on CSL, so provenance keys
/// are the same names that appear in `info.yml` — which is what makes them
/// useful to a human reading the file.
pub fn merge(items: &[CslItem]) -> Merged {
    let mut out = Mapping::new();
    let mut provenance = BTreeMap::new();
    let mut consulted = Vec::new();

    for item in items {
        if !item.source.is_empty() && !consulted.contains(&item.source) {
            consulted.push(item.source.clone());
        }
        let Value::Mapping(body) = crate::formats::csl::to_body(item) else {
            continue;
        };
        for (key, value) in body {
            let Some(name) = key.as_str() else { continue };
            // `type` always has a value, so letting a later provider overwrite
            // it would mean the last provider wins rather than the best one.
            if out.contains_key(&key) {
                continue;
            }
            out.insert(key.clone(), value);
            if !item.source.is_empty() {
                provenance.insert(name.to_owned(), item.source.clone());
            }
        }
    }

    Merged {
        body: Value::Mapping(out),
        provenance,
        consulted,
    }
}

/// The providers to consult, in configured order.
///
/// A provider can be disabled or pointed at another host without changing the
/// order, which is what makes `[providers.crossref] base_url = …` work for a
/// mirror, a proxy, or a test double.
pub fn registry(config: &ProvidersConfig) -> Vec<Box<dyn MetadataProvider>> {
    config
        .order
        .iter()
        .filter(|name| {
            config
                .tuning
                .get(name.as_str())
                .is_none_or(|tuning| tuning.enabled)
        })
        .filter_map(|name| {
            let base = config
                .tuning
                .get(name.as_str())
                .and_then(|t| t.base_url.clone());
            build(name, base)
        })
        .collect()
}

fn build(name: &str, base: Option<String>) -> Option<Box<dyn MetadataProvider>> {
    Some(match (name, base) {
        ("crossref", Some(base)) => Box::new(crossref::Crossref::with_base(base)),
        ("crossref", None) => Box::new(crossref::Crossref::new()),
        ("openalex", Some(base)) => Box::new(openalex::OpenAlex::with_base(base)),
        ("openalex", None) => Box::new(openalex::OpenAlex::new()),
        ("arxiv", Some(base)) => Box::new(arxiv::ArXiv::with_base(base)),
        ("arxiv", None) => Box::new(arxiv::ArXiv::new()),
        ("openlibrary", Some(base)) => Box::new(books::OpenLibrary::with_base(base)),
        ("openlibrary", None) => Box::new(books::OpenLibrary::new()),
        ("google-books", Some(base)) => Box::new(books::GoogleBooks::with_base(base)),
        ("google-books", None) => Box::new(books::GoogleBooks::new()),
        // An unknown name in the order is ignored rather than fatal: a config
        // written for a newer version must not break every command.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(source: &str, json: &str) -> CslItem {
        let mut item: CslItem = serde_json::from_str(json).unwrap();
        item.source = source.to_owned();
        item
    }

    #[test]
    fn earlier_providers_win_field_by_field() {
        let merged = merge(&[
            item(
                "crossref",
                r#"{"type":"article-journal","title":"Crossref Title"}"#,
            ),
            item(
                "openalex",
                r#"{"type":"article-journal","title":"OpenAlex Title",
                    "abstract":"Only OpenAlex has this"}"#,
            ),
        ]);
        let body = merged.body.as_mapping().unwrap();
        assert_eq!(body["title"].as_str(), Some("Crossref Title"));
        // …but a field the first provider lacked is filled by the second.
        assert_eq!(body["abstract"].as_str(), Some("Only OpenAlex has this"));
    }

    #[test]
    fn provenance_records_where_each_field_came_from() {
        let merged = merge(&[
            item("crossref", r#"{"type":"article-journal","title":"T"}"#),
            item("openalex", r#"{"type":"article-journal","abstract":"A"}"#),
        ]);
        assert_eq!(merged.provenance["title"], "crossref");
        assert_eq!(merged.provenance["abstract"], "openalex");
        assert_eq!(merged.consulted, ["crossref", "openalex"]);
    }

    /// `type` is always present, so without the first-wins rule the last
    /// provider consulted would silently decide the entry type.
    #[test]
    fn the_first_provider_decides_the_entry_type() {
        let merged = merge(&[
            item("crossref", r#"{"type":"paper-conference","title":"T"}"#),
            item("openalex", r#"{"type":"article-journal","title":"T"}"#),
        ]);
        assert_eq!(merged.body["type"].as_str(), Some("article"));
        assert_eq!(
            merged.body["parent"]["type"].as_str(),
            Some("proceedings"),
            "the conference container should survive the merge"
        );
    }

    #[test]
    fn merging_nothing_yields_an_empty_body() {
        let merged = merge(&[]);
        assert!(merged.body.as_mapping().unwrap().is_empty());
        assert!(merged.provenance.is_empty());
    }

    #[test]
    fn the_registry_ignores_unknown_provider_names() {
        let providers = registry(&ProvidersConfig {
            order: vec!["crossref".into(), "not-a-provider".into()],
            ..ProvidersConfig::default()
        });
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name(), "crossref");
    }
}

/// The search providers to consult, in configured order.
pub fn search_registry(config: &ProvidersConfig) -> Vec<Box<dyn search::SearchProvider>> {
    config
        .order
        .iter()
        .filter(|name| {
            config
                .tuning
                .get(name.as_str())
                .is_none_or(|tuning| tuning.enabled)
        })
        .filter_map(|name| {
            let base = config
                .tuning
                .get(name.as_str())
                .and_then(|t| t.base_url.clone());
            build_search(name, base)
        })
        .collect()
}

fn build_search(name: &str, base: Option<String>) -> Option<Box<dyn search::SearchProvider>> {
    Some(match (name, base) {
        ("crossref", Some(base)) => Box::new(crossref::Crossref::with_base(base)),
        ("crossref", None) => Box::new(crossref::Crossref::new()),
        ("openalex", Some(base)) => Box::new(openalex::OpenAlex::with_base(base)),
        ("openalex", None) => Box::new(openalex::OpenAlex::new()),
        ("arxiv", Some(base)) => Box::new(arxiv::ArXiv::with_base(base)),
        ("arxiv", None) => Box::new(arxiv::ArXiv::new()),
        ("openlibrary", Some(base)) => Box::new(books::OpenLibrary::with_base(base)),
        ("openlibrary", None) => Box::new(books::OpenLibrary::new()),
        ("google-books", Some(base)) => Box::new(books::GoogleBooks::with_base(base)),
        ("google-books", None) => Box::new(books::GoogleBooks::new()),
        _ => return None,
    })
}

/// What a search run produced, including what it gave up on.
#[derive(Debug, Default)]
pub struct SearchRun {
    pub results: Vec<CslItem>,
    /// One line per provider, for stderr. Never stdout: a caller parsing
    /// `--json` must not have to filter our progress out of its payload.
    pub notes: Vec<String>,
    /// True when the budget ran out with providers still unqueried.
    pub partial: bool,
}

/// Query every provider that is configured, within a time budget.
///
/// **Partial results beat hanging.** A launcher debounces at a few hundred
/// milliseconds and cannot wait on five services; when the budget is spent the
/// remaining providers are skipped and what has arrived is returned, with the
/// skipped ones named in `notes`.
pub fn search_all(
    http: &Http,
    config: &ProvidersConfig,
    query: &search::SearchQuery,
    budget: Duration,
) -> SearchRun {
    let started = Instant::now();
    let mut run = SearchRun::default();

    for provider in search_registry(config) {
        if started.elapsed() >= budget {
            run.notes
                .push(format!("{}: skipped, time budget spent", provider.name()));
            run.partial = true;
            continue;
        }
        match provider.search(http, query) {
            Ok(items) => {
                run.notes
                    .push(format!("{}: {} result(s)", provider.name(), items.len()));
                run.results.extend(items);
            }
            // A provider with nothing to say is normal, not a failure.
            Err(ProviderError::NotFound) => {
                run.notes.push(format!("{}: no results", provider.name()));
            }
            Err(e) => {
                run.notes.push(format!("{}: {e}", provider.name()));
            }
        }
    }
    run
}
