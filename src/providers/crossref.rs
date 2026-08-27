//! DOI content negotiation.
//!
//! `https://doi.org/{doi}` with a CSL-JSON `Accept` header is routed by the DOI
//! system to whichever registration agency owns the prefix — Crossref, DataCite
//! or mEDRA — so one code path covers all of them. That is why this is the
//! first provider consulted: it answers for essentially every DOI in existence,
//! not just Crossref's.

use super::search::{SearchProvider, SearchQuery, encode};
use super::{Http, MetadataProvider, ProviderError};
use crate::formats::csl::CslItem;
use crate::identify::patterns::Identifier;
use serde::Deserialize;
use std::time::Duration;

pub const CSL_JSON: &str = "application/vnd.citationstyles.csl+json";

/// Used by both trait impls below; `NAME` would be ambiguous between
/// `MetadataProvider` and `SearchProvider`.
const NAME: &str = "crossref";

pub struct Crossref {
    /// Content negotiation host, normally `doi.org`.
    base: String,
    /// Search host. A *different* service from the resolver above, so it needs
    /// its own base rather than sharing one.
    api_base: String,
}

impl Crossref {
    pub fn new() -> Self {
        Self {
            base: "https://doi.org".to_owned(),
            api_base: "https://api.crossref.org".to_owned(),
        }
    }

    /// Point both endpoints at one host. Tests use it; so could a mirror.
    pub fn with_base(base: impl Into<String>) -> Self {
        let base = base.into();
        Self {
            api_base: base.clone(),
            base,
        }
    }
}

impl Default for Crossref {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataProvider for Crossref {
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
        let url = format!("{}/{doi}", self.base);
        let mut item: CslItem = http.get_json(NAME, &url, CSL_JSON, Duration::from_millis(200))?;

        item.source = NAME.to_owned();
        normalize_type(&mut item);
        // The response echoes the DOI, but not always in the case we asked for.
        if item.doi.is_none() {
            item.doi = Some(doi.clone());
        }
        Ok(item)
    }
}

/// Crossref's search API, which is a different service from the DOI resolver
/// above and lives on a different host.
#[derive(Debug, Deserialize)]
struct WorkList {
    message: WorkListMessage,
}

#[derive(Debug, Deserialize)]
struct WorkListMessage {
    #[serde(default)]
    items: Vec<CslItem>,
}

impl SearchProvider for Crossref {
    fn name(&self) -> &'static str {
        NAME
    }

    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError> {
        // `query.title` for a title, `query.bibliographic` for a whole citation
        // string. The distinction is not cosmetic: `query.bibliographic` on the
        // bare words "attention is all you need" returns three papers that
        // merely quote the phrase and not the actual one, while `query.title`
        // returns it first.
        let field = if query.citation_like {
            "query.bibliographic"
        } else {
            "query.title"
        };
        let mut url = format!(
            "{}/works?{field}={}&rows={}",
            self.api_base,
            encode(&query.text),
            query.limit.clamp(1, 50)
        );
        if let Some(author) = &query.author {
            url.push_str(&format!("&query.author={}", encode(author)));
        }
        if let Some(year) = query.year {
            url.push_str(&format!(
                "&filter=from-pub-date:{year}-01-01,until-pub-date:{year}-12-31"
            ));
        }
        // The polite pool wants the contact address as a parameter too, but
        // only when the user configured one.
        if let Some(mailto) = http.mailto() {
            url.push_str(&format!("&mailto={}", encode(mailto)));
        }

        let list: WorkList = http.get_json(NAME, &url, "application/json", self.rate_limit())?;
        Ok(list
            .message
            .items
            .into_iter()
            .map(|mut item| {
                item.source = NAME.to_owned();
                normalize_type(&mut item);
                item
            })
            .collect())
    }
}

/// Crossref's `type` vocabulary differs from CSL's on the three commonest
/// values. Untranslated, every journal article would import as `misc`.
fn normalize_type(item: &mut CslItem) {
    item.kind = Some(match item.kind.as_deref() {
        Some("journal-article") => "article-journal".to_owned(),
        Some("proceedings-article") => "paper-conference".to_owned(),
        Some("book-chapter") => "chapter".to_owned(),
        Some(other) => other.to_owned(),
        None => "article-journal".to_owned(),
    });
}
