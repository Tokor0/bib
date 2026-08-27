//! ISBN lookup: OpenLibrary and Google Books.
//!
//! Both are patchy in complementary ways — OpenLibrary on publisher and date,
//! Google Books on authors — so they are merged rather than chosen between.
//! That is why both sit in the default provider order.

use super::search::{SearchProvider, SearchQuery, encode};
use super::{Http, MetadataProvider, ProviderError};
use crate::formats::csl::{CslDate, CslItem, CslName, Flexible};
use crate::identify::patterns::Identifier;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

fn person(display: &str) -> CslName {
    match display.trim().rsplit_once(' ') {
        Some((given, family)) => CslName {
            family: Some(family.to_owned()),
            given: Some(given.to_owned()),
            ..CslName::default()
        },
        None => CslName {
            literal: Some(display.trim().to_owned()),
            ..CslName::default()
        },
    }
}

// ------------------------------------------------------------ OpenLibrary

const OL_NAME: &str = "openlibrary";

pub struct OpenLibrary {
    base: String,
}

impl OpenLibrary {
    pub fn new() -> Self {
        Self {
            base: "https://openlibrary.org".to_owned(),
        }
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for OpenLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Deserialize)]
struct OlBook {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    authors: Vec<OlNamed>,
    #[serde(default)]
    publishers: Vec<OlNamed>,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    publish_places: Vec<OlNamed>,
}

#[derive(Debug, Deserialize)]
struct OlNamed {
    #[serde(default)]
    name: Option<String>,
}

impl MetadataProvider for OpenLibrary {
    fn name(&self) -> &'static str {
        OL_NAME
    }

    fn supports(&self, id: &Identifier) -> bool {
        matches!(id, Identifier::Isbn(_))
    }

    fn fetch(&self, http: &Http, id: &Identifier) -> Result<CslItem, ProviderError> {
        let Identifier::Isbn(isbn) = id else {
            return Err(ProviderError::Unsupported);
        };
        let url = format!(
            "{}/api/books?bibkeys=ISBN:{isbn}&jscmd=data&format=json",
            self.base
        );
        let response: BTreeMap<String, OlBook> = http.get_json(
            OL_NAME,
            &url,
            "application/json",
            Duration::from_millis(200),
        )?;

        // An unknown ISBN returns `{}` with HTTP 200 rather than a 404.
        let book = response
            .into_values()
            .next()
            .ok_or(ProviderError::NotFound)?;

        let title = match (&book.title, &book.subtitle) {
            (Some(title), Some(subtitle)) => Some(format!("{title}: {subtitle}")),
            (Some(title), None) => Some(title.clone()),
            _ => None,
        };

        Ok(CslItem {
            kind: Some("book".into()),
            title: title.map(Flexible::Text),
            author: book
                .authors
                .iter()
                .filter_map(|a| a.name.as_deref())
                .map(person)
                .collect(),
            publisher: book
                .publishers
                .first()
                .and_then(|p| p.name.clone())
                .map(Flexible::Text),
            publisher_place: book
                .publish_places
                .first()
                .and_then(|p| p.name.clone())
                .map(Flexible::Text),
            issued: book.publish_date.map(|raw| CslDate {
                raw: Some(raw),
                ..CslDate::default()
            }),
            isbn: Some(Flexible::Text(isbn.clone())),
            source: OL_NAME.to_owned(),
            ..CslItem::default()
        })
    }
}

// ------------------------------------------------------------ Google Books

const GB_NAME: &str = "google-books";

pub struct GoogleBooks {
    base: String,
}

impl GoogleBooks {
    pub fn new() -> Self {
        Self {
            base: "https://www.googleapis.com".to_owned(),
        }
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for GoogleBooks {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct GbResponse {
    #[serde(default)]
    items: Vec<GbItem>,
}

#[derive(Debug, Deserialize)]
struct GbItem {
    #[serde(default)]
    #[serde(rename = "volumeInfo")]
    volume_info: GbVolume,
}

#[derive(Debug, Default, Deserialize)]
struct GbVolume {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    language: Option<String>,
    /// Search results carry no ISBN anywhere else, and without one a result
    /// can be shown but never added.
    #[serde(default, rename = "industryIdentifiers")]
    industry_identifiers: Vec<GbIndustryId>,
}

#[derive(Debug, Deserialize)]
struct GbIndustryId {
    #[serde(default)]
    identifier: String,
}

impl MetadataProvider for GoogleBooks {
    fn name(&self) -> &'static str {
        GB_NAME
    }

    fn supports(&self, id: &Identifier) -> bool {
        matches!(id, Identifier::Isbn(_))
    }

    fn fetch(&self, http: &Http, id: &Identifier) -> Result<CslItem, ProviderError> {
        let Identifier::Isbn(isbn) = id else {
            return Err(ProviderError::Unsupported);
        };
        let url = format!("{}/books/v1/volumes?q=isbn:{isbn}", self.base);
        let response: GbResponse = http.get_json(
            GB_NAME,
            &url,
            "application/json",
            Duration::from_millis(200),
        )?;

        let volume = response
            .items
            .into_iter()
            .next()
            .ok_or(ProviderError::NotFound)?
            .volume_info;

        let title = match (&volume.title, &volume.subtitle) {
            (Some(title), Some(subtitle)) => Some(format!("{title}: {subtitle}")),
            (Some(title), None) => Some(title.clone()),
            _ => None,
        };

        Ok(CslItem {
            kind: Some("book".into()),
            title: title.map(Flexible::Text),
            author: volume.authors.iter().map(|a| person(a)).collect(),
            publisher: volume.publisher.map(Flexible::Text),
            issued: volume.published_date.map(|raw| CslDate {
                raw: Some(raw),
                ..CslDate::default()
            }),
            abstract_: volume.description,
            language: volume.language,
            isbn: Some(Flexible::Text(isbn.clone())),
            source: GB_NAME.to_owned(),
            ..CslItem::default()
        })
    }
}

// -------------------------------------------------------------- book search

#[derive(Debug, Deserialize)]
struct OlSearchResponse {
    #[serde(default)]
    docs: Vec<OlSearchDoc>,
}

#[derive(Debug, Deserialize)]
struct OlSearchDoc {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author_name: Vec<String>,
    #[serde(default)]
    first_publish_year: Option<i64>,
    #[serde(default)]
    publisher: Vec<String>,
    #[serde(default)]
    isbn: Vec<String>,
}

impl SearchProvider for OpenLibrary {
    fn name(&self) -> &'static str {
        OL_NAME
    }

    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError> {
        let mut url = format!(
            "{}/search.json?title={}&limit={}&fields=title,author_name,first_publish_year,publisher,isbn",
            self.base,
            encode(&query.text),
            query.limit.clamp(1, 50)
        );
        if let Some(author) = &query.author {
            url.push_str(&format!("&author={}", encode(author)));
        }

        let response: OlSearchResponse =
            http.get_json(OL_NAME, &url, "application/json", self.rate_limit())?;

        Ok(response
            .docs
            .into_iter()
            .map(|doc| CslItem {
                kind: Some("book".into()),
                title: doc.title.map(Flexible::Text),
                author: doc.author_name.iter().map(|a| person(a)).collect(),
                publisher: doc.publisher.first().cloned().map(Flexible::Text),
                issued: doc.first_publish_year.map(|year| CslDate {
                    raw: Some(year.to_string()),
                    ..CslDate::default()
                }),
                // A work has many editions and therefore many ISBNs; the first
                // that validates is enough to identify the work for a lookup.
                isbn: doc
                    .isbn
                    .iter()
                    .find_map(|i| crate::identify::patterns::normalize_isbn(i))
                    .map(Flexible::Text),
                source: OL_NAME.to_owned(),
                ..CslItem::default()
            })
            .collect())
    }
}

impl SearchProvider for GoogleBooks {
    fn name(&self) -> &'static str {
        GB_NAME
    }

    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError> {
        // Without an API key this answers 429 more often than not, so it is a
        // bonus source: its failures must stay quiet rather than look broken.
        let mut terms = format!("intitle:{}", query.text);
        if let Some(author) = &query.author {
            terms.push_str(&format!("+inauthor:{author}"));
        }
        let url = format!(
            "{}/books/v1/volumes?q={}&maxResults={}",
            self.base,
            encode(&terms),
            query.limit.clamp(1, 40)
        );

        let response: GbResponse =
            http.get_json(GB_NAME, &url, "application/json", self.rate_limit())?;

        Ok(response
            .items
            .into_iter()
            .map(|item| to_csl(item.volume_info))
            .collect())
    }
}

/// A Google Books volume as a CSL record. Shared by `fetch` and `search`.
fn to_csl(volume: GbVolume) -> CslItem {
    let title = match (&volume.title, &volume.subtitle) {
        (Some(title), Some(subtitle)) => Some(format!("{title}: {subtitle}")),
        (Some(title), None) => Some(title.clone()),
        _ => None,
    };
    CslItem {
        kind: Some("book".into()),
        title: title.map(Flexible::Text),
        author: volume.authors.iter().map(|a| person(a)).collect(),
        publisher: volume.publisher.map(Flexible::Text),
        issued: volume.published_date.map(|raw| CslDate {
            raw: Some(raw),
            ..CslDate::default()
        }),
        abstract_: volume.description,
        language: volume.language,
        isbn: volume
            .industry_identifiers
            .iter()
            .find_map(|i| crate::identify::patterns::normalize_isbn(&i.identifier))
            .map(Flexible::Text),
        source: GB_NAME.to_owned(),
        ..CslItem::default()
    }
}
