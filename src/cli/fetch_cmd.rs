//! `bib fetch` — retrieve the document for entries that have none.
//!
//! Shared with `bib add --fetch` so the two cannot disagree about where a PDF
//! may come from or what counts as one.

use crate::cli::{resolve, update};
use crate::config::{self, Config};
use crate::identify::patterns::Identifier;
use crate::model::Document;
use crate::providers::Http;
use crate::providers::fetch::{self, PdfSource};
use crate::store::Store;
use anyhow::{Result, bail};
use clap::Args;
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
        if !args.force && !doc.files().is_empty() {
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
    let id = entry.doi().and_then(Identifier::parse_doi).or_else(|| {
        entry
            .arxiv()
            .and_then(|a| crate::identify::patterns::normalize_arxiv(a).map(Identifier::ArXiv))
    });

    let sources = allowed(
        config,
        fetch::candidates(
            id.as_ref(),
            doc.meta().provenance.get("oa_url").map(String::as_str),
            entry.url().map(|u| u.value.to_string()).as_deref(),
        ),
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
        config.fetch.max_size,
        config.fetch.timeout,
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
