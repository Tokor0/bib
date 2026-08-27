//! Turning an identifier — or a PDF — into a document body.
//!
//! Shared by `bib add` and `bib update` so the two cannot disagree about how a
//! record is built, which provider won a field, or how a preprint is upgraded
//! to its published version.

use crate::config::Config;
use crate::formats::csl::CslItem;
use crate::identify::patterns::Identifier;
pub use crate::providers::Http;
use crate::providers::{self, Merged, ProviderError};
use anyhow::{Result, bail};
use serde_yaml::Value;

/// Everything learned about one identifier.
#[derive(Debug)]
pub struct Resolved {
    pub body: Value,
    pub provenance: std::collections::BTreeMap<String, String>,
    /// Identifiers to record, including the one asked for. A preprint upgraded
    /// to its journal version keeps both.
    pub identifiers: Vec<Identifier>,
    pub notes: Vec<String>,
}

/// Ask every configured provider that supports `id`, in order, and merge.
pub fn resolve(http: &Http, config: &Config, id: &Identifier) -> Result<Resolved> {
    let mut identifiers = vec![id.clone()];
    let mut items: Vec<CslItem> = Vec::new();
    let mut notes = Vec::new();

    collect(http, config, id, &mut items, &mut notes);

    // An arXiv entry that names a published DOI is a preprint of something
    // better catalogued: the journal record has the volume, the pages and the
    // final title. Resolve that too, and prefer it — while keeping the arXiv ID
    // as a secondary serial number so the file the user has is still findable.
    if let Some(doi) = items
        .iter()
        .find(|i| i.source == "arxiv")
        .and_then(|i| i.doi.clone())
        .filter(|d| !d.trim().is_empty())
        && let Some(published) = Identifier::parse_doi(&doi)
    {
        notes.push(format!("arxiv names a published DOI ({doi}); resolving it"));
        let mut journal = Vec::new();
        collect(http, config, &published, &mut journal, &mut notes);
        if !journal.is_empty() {
            identifiers.push(published);
            // Prepended, so the published record wins field by field.
            journal.extend(items);
            items = journal;
        }
    }

    if items.is_empty() {
        bail!("no provider could resolve {id}\n  {}", notes.join("\n  "));
    }

    let Merged {
        body,
        provenance,
        consulted,
    } = providers::merge(&items);
    notes.push(format!("merged from: {}", consulted.join(", ")));

    Ok(Resolved {
        body: with_identifiers(body, &identifiers),
        provenance,
        identifiers,
        notes,
    })
}

fn collect(
    http: &Http,
    config: &Config,
    id: &Identifier,
    items: &mut Vec<CslItem>,
    notes: &mut Vec<String>,
) {
    for provider in providers::registry(&config.providers) {
        if !provider.supports(id) {
            continue;
        }
        match provider.fetch(http, id) {
            Ok(item) => {
                notes.push(format!("{}: ok", provider.name()));
                items.push(item);
            }
            // Not finding a record is normal; the next provider gets a turn.
            Err(ProviderError::NotFound) => {
                notes.push(format!("{}: no record", provider.name()));
            }
            Err(e) => notes.push(format!("{}: {e}", provider.name())),
        }
    }
}

/// Make sure every identifier we know about is recorded, including ones no
/// provider echoed back.
fn with_identifiers(mut body: Value, identifiers: &[Identifier]) -> Value {
    let Value::Mapping(map) = &mut body else {
        return body;
    };
    let serial = map
        .entry(Value::String("serial-number".into()))
        .or_insert_with(|| Value::Mapping(Default::default()));
    let Value::Mapping(serial) = serial else {
        return body;
    };
    for id in identifiers {
        let key = Value::String(id.kind().to_owned());
        // Never overwrite what a provider supplied: its spelling is canonical.
        serial
            .entry(key)
            .or_insert_with(|| Value::String(id.value().to_owned()));
    }
    body
}

/// Build the HTTP client from configuration.
pub fn http(config: &Config, cache: std::path::PathBuf, offline: bool) -> Http {
    Http::new()
        .with_cache(cache)
        // Only ever sent when the user has configured it: it is their address,
        // and it goes to Crossref's polite pool, nowhere else.
        .with_mailto(config.providers.mailto.clone())
        .offline(offline)
}
