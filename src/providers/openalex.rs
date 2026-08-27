//! OpenAlex — consulted after Crossref, for what Crossref is weak at.
//!
//! Its author records are disambiguated (ORCIDs, deduplicated institutions)
//! where Crossref often carries initials only, and it reports open-access
//! locations. It is not a substitute for Crossref: its own type vocabulary is
//! coarser, which is why it sits second and only fills gaps.

use super::search::{SearchProvider, SearchQuery, encode};
use super::{Http, MetadataProvider, ProviderError};
use crate::formats::csl::{CslDate, CslItem, CslName, Flexible};
use crate::identify::patterns::Identifier;
use serde::Deserialize;
use std::time::Duration;

const NAME: &str = "openalex";

pub struct OpenAlex {
    base: String,
}

impl OpenAlex {
    pub fn new() -> Self {
        Self {
            base: "https://api.openalex.org".to_owned(),
        }
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for OpenAlex {
    fn default() -> Self {
        Self::new()
    }
}

/// OpenAlex's own shape, mapped onto CSL below rather than consumed directly.
#[derive(Debug, Deserialize)]
struct Work {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publication_date: Option<String>,
    #[serde(default)]
    publication_year: Option<i64>,
    #[serde(default)]
    doi: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    authorships: Vec<Authorship>,
    #[serde(default)]
    biblio: Biblio,
    #[serde(default)]
    primary_location: Option<Location>,
    #[serde(default)]
    cited_by_count: Option<i64>,
    #[serde(default)]
    open_access: Option<OpenAccess>,
    #[serde(default)]
    best_oa_location: Option<Location>,
}

#[derive(Debug, Deserialize)]
struct OpenAccess {
    #[serde(default)]
    oa_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Authorship {
    #[serde(default)]
    author: Author,
}

#[derive(Debug, Default, Deserialize)]
struct Author {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Biblio {
    #[serde(default)]
    volume: Option<String>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    first_page: Option<String>,
    #[serde(default)]
    last_page: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Location {
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    pdf_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Source {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    issn_l: Option<String>,
    #[serde(default)]
    host_organization_name: Option<String>,
}

/// OpenAlex gives display names, not structured ones. Splitting on the last
/// space is wrong for particles and suffixes, so anything ambiguous is left as
/// a literal rather than guessed at.
fn name(display: &str) -> CslName {
    let trimmed = display.trim();
    match trimmed.rsplit_once(' ') {
        Some((given, family)) if !given.contains(' ') || given.split(' ').count() <= 3 => CslName {
            family: Some(family.to_owned()),
            given: Some(given.to_owned()),
            ..CslName::default()
        },
        _ => CslName {
            literal: Some(trimmed.to_owned()),
            ..CslName::default()
        },
    }
}

impl MetadataProvider for OpenAlex {
    fn name(&self) -> &'static str {
        NAME
    }

    fn supports(&self, id: &Identifier) -> bool {
        matches!(id, Identifier::Doi(_))
    }

    fn fetch(&self, http: &Http, id: &Identifier) -> Result<CslItem, ProviderError> {
        let Identifier::Doi(doi) = id else {
            return Err(ProviderError::Unsupported);
        };
        let url = format!("{}/works/doi:{doi}", self.base);
        let work: Work =
            http.get_json(NAME, &url, "application/json", Duration::from_millis(200))?;
        Ok(to_csl(work))
    }
}

/// OpenAlex's own shape onto CSL. Shared by `fetch` and `search`, which return
/// the identical structure and must not drift apart.
fn to_csl(work: Work) -> CslItem {
    let container = work
        .primary_location
        .as_ref()
        .and_then(|l| l.source.as_ref());

    let page = match (&work.biblio.first_page, &work.biblio.last_page) {
        (Some(first), Some(last)) => Some(format!("{first}-{last}")),
        (Some(first), None) => Some(first.clone()),
        _ => None,
    };

    let date = work
        .publication_date
        .clone()
        .or_else(|| work.publication_year.map(|y| y.to_string()));

    CslItem {
        kind: Some(match work.kind.as_deref() {
            Some("article") | None => "article-journal".into(),
            Some("book") => "book".into(),
            Some("book-chapter") => "chapter".into(),
            Some("dissertation") => "thesis".into(),
            Some("preprint") => "manuscript".into(),
            Some(other) => other.to_owned(),
        }),
        title: work.title.map(Flexible::Text),
        container_title: container
            .and_then(|s| s.display_name.clone())
            .map(Flexible::Text),
        author: work
            .authorships
            .iter()
            .filter_map(|a| a.author.display_name.as_deref())
            .map(name)
            .collect(),
        issued: date.map(|raw| CslDate {
            raw: Some(raw),
            ..CslDate::default()
        }),
        volume: work.biblio.volume.map(Flexible::Text),
        issue: work.biblio.issue.map(Flexible::Text),
        page: page.map(Flexible::Text),
        publisher: container
            .and_then(|s| s.host_organization_name.clone())
            .map(Flexible::Text),
        doi: work.doi.map(|d| {
            d.trim_start_matches("https://doi.org/")
                .to_ascii_lowercase()
        }),
        issn: container.and_then(|s| s.issn_l.clone()).map(Flexible::Text),
        language: work.language,
        cited_by: work.cited_by_count,
        // `pdf_url` first: `oa_url` is often a landing page rather than a
        // document. Neither is trusted — `fetch` checks the bytes.
        oa_url: work
            .best_oa_location
            .as_ref()
            .and_then(|l| l.pdf_url.clone())
            .or_else(|| work.open_access.as_ref().and_then(|o| o.oa_url.clone())),
        source: NAME.to_owned(),
        ..CslItem::default()
    }
}

#[derive(Debug, Deserialize)]
struct WorkList {
    #[serde(default)]
    results: Vec<Work>,
}

impl SearchProvider for OpenAlex {
    fn name(&self) -> &'static str {
        NAME
    }

    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError> {
        // `filter=title.search:` and not plain `search=`: the latter searches
        // *full text* and returns 2.9M hits for the same handful of words.
        let mut filters = vec![format!("title.search:{}", query.text)];
        if let Some(year) = query.year {
            filters.push(format!("publication_year:{year}"));
        }
        let mut url = format!(
            "{}/works?filter={}&per-page={}",
            self.base,
            encode(&filters.join(",")),
            query.limit.clamp(1, 50)
        );
        if let Some(mailto) = http.mailto() {
            url.push_str(&format!("&mailto={}", encode(mailto)));
        }

        let list: WorkList = http.get_json(NAME, &url, "application/json", self.rate_limit())?;
        Ok(list.results.into_iter().map(to_csl).collect())
    }
}
