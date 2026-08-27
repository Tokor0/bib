//! The arXiv API.
//!
//! Atom XML rather than JSON, and **rate limited to one request every three
//! seconds with a single connection** — that is arXiv's stated terms of use,
//! not a tuning parameter.
//!
//! When an arXiv entry carries a published DOI, that DOI is the better record:
//! the journal version has page numbers, a volume and a final title. So this
//! provider reports the DOI it finds, and the caller re-resolves through
//! Crossref, keeping the arXiv ID as a secondary serial number.

use super::search::{SearchProvider, SearchQuery, encode};
use super::{Http, MetadataProvider, ProviderError};
use crate::formats::csl::{CslDate, CslItem, CslName, Flexible};
use crate::identify::patterns::Identifier;
use quick_xml::events::Event;
use std::time::Duration;

/// arXiv's terms of use. Not negotiable.
pub const RATE_LIMIT: Duration = Duration::from_secs(3);

const NAME: &str = "arxiv";

pub struct ArXiv {
    base: String,
}

impl ArXiv {
    pub fn new() -> Self {
        Self {
            base: "https://export.arxiv.org".to_owned(),
        }
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }
}

impl Default for ArXiv {
    fn default() -> Self {
        Self::new()
    }
}

/// What an Atom `<entry>` yields.
#[derive(Debug, Default)]
pub struct AtomEntry {
    pub id: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub published: Option<String>,
    pub authors: Vec<String>,
    pub doi: Option<String>,
    pub journal_ref: Option<String>,
}

/// Parse an arXiv Atom response into every `<entry>` it contains.
///
/// Search returns many; resolution returns one. Both go through here, so the
/// feed-level `<title>ArXiv Query: …</title>` is skipped by exactly the same
/// rule in both cases.
pub fn parse_atom_entries(xml: &str) -> Vec<AtomEntry> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries: Vec<AtomEntry> = Vec::new();
    let mut current: Option<AtomEntry> = None;
    let mut path: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "entry" {
                    current = Some(AtomEntry::default());
                }
                path.push(name);
            }
            Ok(Event::Text(e)) => {
                let Some(entry) = current.as_mut() else {
                    // Outside any <entry>: feed-level metadata, not a result.
                    continue;
                };
                let Ok(raw) = e.decode() else { continue };
                let Ok(text) = quick_xml::escape::unescape(&raw) else {
                    continue;
                };
                let text = collapse(&text);
                if text.is_empty() {
                    continue;
                }
                match path.last().map(String::as_str) {
                    Some("title") => entry.title = Some(text),
                    Some("summary") => entry.summary = Some(text),
                    Some("published") => entry.published = Some(text),
                    Some("id") => entry.id = Some(text),
                    // `<author><name>` — guard on the parent so a feed-level
                    // name cannot be mistaken for an author.
                    Some("name") if path.iter().rev().nth(1).is_some_and(|p| p == "author") => {
                        entry.authors.push(text)
                    }
                    Some("doi") => entry.doi = Some(text),
                    Some("journal_ref") => entry.journal_ref = Some(text),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                if local_name(e.name().as_ref()) == "entry"
                    && let Some(entry) = current.take()
                {
                    entries.push(entry);
                }
                path.pop();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    entries
}

/// The first entry, for identifier resolution.
pub fn parse_atom(xml: &str) -> Option<AtomEntry> {
    parse_atom_entries(xml).into_iter().next()
}

fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    name.rsplit(':')
        .next()
        .unwrap_or(&name)
        .to_ascii_lowercase()
}

/// arXiv wraps titles and abstracts across lines; the breaks are formatting.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split `"Ashish Vaswani"` into given and family.
fn name(display: &str) -> CslName {
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

impl MetadataProvider for ArXiv {
    fn name(&self) -> &'static str {
        NAME
    }

    fn supports(&self, id: &Identifier) -> bool {
        matches!(id, Identifier::ArXiv(_))
    }

    fn fetch(&self, http: &Http, id: &Identifier) -> Result<CslItem, ProviderError> {
        let Identifier::ArXiv(arxiv) = id else {
            return Err(ProviderError::Unsupported);
        };
        let url = format!("{}/api/query?id_list={arxiv}&max_results=1", self.base);
        let xml = http.get(NAME, &url, "application/atom+xml", RATE_LIMIT)?;

        let entry = parse_atom(&xml).ok_or(ProviderError::NotFound)?;
        to_csl(entry, Some(arxiv)).ok_or(ProviderError::NotFound)
    }
}

/// One Atom entry as a CSL record. Shared by `fetch` and `search`.
fn to_csl(entry: AtomEntry, arxiv_id: Option<&str>) -> Option<CslItem> {
    let title = entry.title?;
    // A query for an unknown ID still returns a well-formed feed, with a single
    // entry whose title is literally "Error".
    if title.eq_ignore_ascii_case("Error") {
        return None;
    }
    // `<id>` is the abstract URL; the identifier is its last path segment.
    let id = arxiv_id.map(str::to_owned).or_else(|| {
        entry
            .id
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .map(str::to_owned)
    });

    Some(CslItem {
        // A preprint is a manuscript until a journal version exists; the caller
        // upgrades it if `doi` leads somewhere.
        kind: Some("manuscript".into()),
        title: Some(Flexible::Text(title)),
        author: entry.authors.iter().map(|a| name(a)).collect(),
        issued: entry.published.map(|raw| CslDate {
            raw: Some(raw),
            ..CslDate::default()
        }),
        abstract_: entry.summary,
        doi: entry.doi.map(|d| d.to_ascii_lowercase()),
        note: entry.journal_ref,
        url: id.map(|id| format!("https://arxiv.org/abs/{id}")),
        source: NAME.to_owned(),
        ..CslItem::default()
    })
}

impl SearchProvider for ArXiv {
    fn name(&self) -> &'static str {
        NAME
    }

    /// arXiv's terms of use, not a tuning parameter.
    fn rate_limit(&self) -> Duration {
        RATE_LIMIT
    }

    fn search(&self, http: &Http, query: &SearchQuery) -> Result<Vec<CslItem>, ProviderError> {
        // Fielded search: `ti:` for the title, `au:` for an author. Quoting the
        // title makes it a phrase rather than a bag of words.
        let mut terms = vec![format!("ti:{}", quote(&query.text))];
        if let Some(author) = &query.author {
            terms.push(format!("au:{}", quote(author)));
        }
        let url = format!(
            "{}/api/query?search_query={}&max_results={}",
            self.base,
            encode(&terms.join(" AND ")),
            query.limit.clamp(1, 50)
        );

        let xml = http.get(NAME, &url, "application/atom+xml", self.rate_limit())?;
        Ok(parse_atom_entries(&xml)
            .into_iter()
            .filter_map(|entry| to_csl(entry, None))
            .collect())
    }
}

/// Wrap in double quotes for arXiv's query language, dropping any the user
/// typed so the phrase cannot be terminated early.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', " ").trim())
}
