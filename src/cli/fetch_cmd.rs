//! `bib fetch` — retrieve the document for entries that have none.
//!
//! Shared with `bib add --fetch` so the two cannot disagree about where a PDF
//! may come from or what counts as one.

use crate::cli::{resolve, update};
use crate::config::{self, Config};
use crate::identify::patterns::{self, Identifier};
use crate::model::Document;
use crate::providers::Http;
use crate::providers::fetch::{self, PdfSource};
use crate::store::Store;
use anyhow::{Result, bail};
use clap::Args;
use hayagriva::Entry;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Cite key, or a query selecting several documents.
    pub target: Vec<String>,

    /// Fetch even for documents that already have an attachment.
    #[arg(long)]
    pub force: bool,

    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: FetchArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let state = loaded.library.state_dir();

    let targets = update::select(&store, &args.target)?;
    if targets.is_empty() {
        bail!("nothing to fetch");
    }

    let http = resolve::http(&loaded.config, state.join("cache/http"), false);
    let mut fetched = 0usize;

    for doc in &targets {
        // `attachments`, not `files`: a `files:` entry naming a file that is
        // not on disk is exactly the document that needs fetching, and a papis
        // import produces a library full of them.
        if !args.force && !doc.attachments().is_empty() {
            eprintln!("{}: already has an attachment", doc.citekey);
            continue;
        }
        match attach(&store, &http, &loaded.config, doc, args.dry_run) {
            Ok(Some(source)) => {
                println!("{}  {}", doc.citekey, source.url);
                fetched += 1;
            }
            Ok(None) => {}
            Err(e) => eprintln!("{}: {e:#}", doc.citekey),
        }
    }

    eprintln!("{fetched} of {} document(s) fetched", targets.len());
    Ok(())
}

/// Try to download a PDF for `doc` and record it.
///
/// Returns the source that worked. A failure is an `Err` the caller reports;
/// it never removes or alters the document.
pub fn attach(
    store: &Store,
    http: &Http,
    config: &Config,
    doc: &Document,
    dry_run: bool,
) -> Result<Option<PdfSource>> {
    let entry = doc.entry()?;
    let arxiv = arxiv_of(&entry);
    let url = entry.url().map(|u| u.value.to_string());
    let oa_url = open_access_url(http, config, &entry, arxiv.is_some());

    let sources = allowed(
        config,
        fetch::candidates(arxiv.as_deref(), oa_url.as_deref(), url.as_deref()),
    );

    if sources.is_empty() {
        bail!("no open-access location is known");
    }
    if dry_run {
        for source in &sources {
            eprintln!("  would try {} ({})", source.url, source.origin);
        }
        return Ok(sources.first().cloned());
    }

    let name = filename(doc, &sources);
    let dest = doc.dir.join(&name);
    let used = fetch::download_first(
        http,
        &sources,
        &dest,
        fetch::Limits::new(config.fetch.max_size, config.fetch.timeout)
            .paced(config.fetch.rate_limit),
    )
    .map_err(|failures| {
        anyhow::anyhow!(
            "{}",
            failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;

    record(store, doc, &name, &used)?;
    Ok(Some(used))
}

/// The entry's arXiv ID, from wherever it was recorded.
///
/// Three places, because three tools disagree: `bib` files it as a serial
/// number, papis and Zotero leave it in the `note` ("arXiv:0802.1919
/// [quant-ph]"), and a great many records have it only as the URL. Consulting
/// just the serial number means a library imported from any of them looks, to
/// the fetcher, like it has no preprint — while every entry in it links to one.
fn arxiv_of(entry: &Entry) -> Option<String> {
    if let Some(id) = entry.arxiv().and_then(patterns::normalize_arxiv) {
        return Some(id);
    }
    if let Some(id) = entry
        .url()
        .and_then(|u| patterns::arxiv_in_url(u.value.as_ref()))
    {
        return Some(id);
    }
    entry
        .note()
        .and_then(|note| patterns::find_arxiv(&note.to_string()).into_iter().next())
}

/// Ask OpenAlex where an open-access copy of this DOI lives.
///
/// The location is not a bibliographic field, so it is nowhere in the record
/// and has to be asked for. Skipped when an arXiv ID is known: that copy is
/// already the best answer, and this is a network round trip. Skipped too when
/// `fetch.sources` does not list `openalex`, which is what that setting is for.
///
/// A failure is not reported: this is one of several ways to find a file, and
/// the ones that follow it are still worth trying.
fn open_access_url(
    http: &Http,
    config: &Config,
    entry: &Entry,
    have_arxiv: bool,
) -> Option<String> {
    if have_arxiv || !config.fetch.sources.iter().any(|s| s == "openalex") {
        return None;
    }
    let doi = entry.doi().and_then(Identifier::parse_doi)?;
    crate::providers::registry(&config.providers)
        .iter()
        .find(|provider| provider.name() == "openalex")?
        .fetch(http, &doi)
        .ok()?
        .oa_url
}

/// Restrict candidate sources to those the configuration permits.
fn allowed(config: &Config, sources: Vec<PdfSource>) -> Vec<PdfSource> {
    sources
        .into_iter()
        .filter(|s| {
            config
                .fetch
                .sources
                .iter()
                .any(|allowed| allowed == s.origin)
        })
        .collect()
}

/// `paper.pdf`, or the arXiv/DOI-derived name when that is more informative.
fn filename(doc: &Document, _sources: &[PdfSource]) -> String {
    let base = doc.citekey.replace(['/', ':'], "-");
    format!("{base}.pdf")
}

/// Add the attachment to `x-bib`, recording where it came from so a re-fetch
/// can be traced and a wrong file can be explained.
fn record(store: &Store, doc: &Document, name: &str, source: &PdfSource) -> Result<()> {
    let mut meta = doc.meta();
    let path = PathBuf::from(name);
    if !meta.files.contains(&path) {
        meta.files.push(path);
    }
    meta.provenance.insert(
        "files".to_owned(),
        format!("{} ({})", source.origin, source.url),
    );

    let mut value = doc.value.clone();
    crate::model::bridge::set_meta(&mut value, serde_yaml::to_value(&meta)?)?;
    store.save(&Document {
        citekey: doc.citekey.clone(),
        dir: doc.dir.clone(),
        value,
    })
}

/// The `Value` form, for `bib add` which has not created a document yet.
pub fn note_source(meta: &mut crate::model::Meta, name: &str, source: &PdfSource) {
    let path = PathBuf::from(name);
    if !meta.files.contains(&path) {
        meta.files.push(path);
    }
    meta.provenance.insert(
        "files".to_owned(),
        format!("{} ({})", source.origin, source.url),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::bridge;

    fn entry(yaml: &str) -> Entry {
        bridge::to_entry("t", &serde_yaml::from_str(yaml).expect("valid YAML"))
            .expect("valid entry")
    }

    #[test]
    fn an_arxiv_serial_number_is_used_directly() {
        let e = entry("type: article\ntitle: X\nserial-number: {arxiv: 2405.00781}\n");
        assert_eq!(arxiv_of(&e).as_deref(), Some("2405.00781"));
    }

    /// The shape a papis or Zotero import produces: the published DOI as the
    /// serial number, and the preprint only in the URL. Reading the serial
    /// number alone would find no arXiv copy for the entire library.
    #[test]
    fn an_arxiv_link_is_enough_when_the_serial_number_is_the_doi() {
        let e = entry(
            "type: article\ntitle: X\nurl: http://arxiv.org/abs/0802.1919\n\
             serial-number: {doi: 10.1007/s00220-009-0873-6}\n",
        );
        assert_eq!(arxiv_of(&e).as_deref(), Some("0802.1919"));
    }

    /// Zotero leaves it here, and papis carries the field across verbatim.
    #[test]
    fn the_note_is_consulted_last() {
        let e = entry("type: article\ntitle: X\nnote: 'arXiv:0802.1919 [quant-ph]'\n");
        assert_eq!(arxiv_of(&e).as_deref(), Some("0802.1919"));
    }

    #[test]
    fn a_record_with_no_preprint_reports_none() {
        let e = entry(
            "type: article\ntitle: X\nurl: https://www.nature.com/articles/x\n\
             note: published version\nserial-number: {doi: 10.1038/x}\n",
        );
        assert_eq!(arxiv_of(&e), None);
    }
}
