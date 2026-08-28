//! `bib add` — identify, fetch, confirm, file.
//!
//! One command covers three routes that must not diverge: a PDF (identified by
//! the pipeline), an explicit identifier, and hand-typed fields. Provider
//! metadata is assembled first and explicit flags are layered on top, so a flag
//! the user typed always beats a fetched value.

use crate::cli::resolve;
use crate::config;
use crate::identify::backend::Poppler;
use crate::identify::patterns::Identifier;
use crate::identify::{self, patterns};
use crate::index::Index;
use crate::model::citekey::KeyMaker;
use crate::model::{Meta, bridge};
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use serde_yaml::{Mapping, Value};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

#[derive(Debug, Default, Args)]
pub struct AddArgs {
    /// What to add: a PDF, a DOI, an arXiv ID, an ISBN, or a URL.
    ///
    /// A PDF is identified automatically — see `bib identify`. Omit this and
    /// supply the fields by hand for a purely offline entry.
    pub source: Option<String>,

    /// Identify and fetch, but do not write anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Answer from the cache only; never call a provider.
    #[arg(long)]
    pub offline: bool,

    /// Also download the open-access PDF, where one is available.
    #[arg(long)]
    pub fetch: bool,

    /// Skip identification and use this identifier directly.
    #[arg(long, value_name = "ID")]
    pub from: Option<String>,

    /// Entry type: article, book, chapter, thesis, web, …
    ///
    /// Overrides whatever the provider says.
    #[arg(long = "type")]
    pub entry_type: Option<String>,

    #[arg(long)]
    pub title: Option<String>,

    /// Author, repeatable. `Family, Given` is parsed most reliably.
    #[arg(long = "author")]
    pub authors: Vec<String>,

    #[arg(long = "editor")]
    pub editors: Vec<String>,

    /// Publication date: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    #[arg(long)]
    pub date: Option<String>,

    #[arg(long)]
    pub doi: Option<String>,
    #[arg(long)]
    pub isbn: Option<String>,
    #[arg(long)]
    pub arxiv: Option<String>,
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub publisher: Option<String>,

    /// Tag, repeatable.
    #[arg(long = "tag")]
    pub tags: Vec<String>,

    /// Attach a file, repeatable. Copied into the document directory.
    #[arg(long = "file")]
    pub files: Vec<PathBuf>,

    /// Use this cite key instead of rendering one from the template.
    #[arg(long)]
    pub key: Option<String>,
}

pub fn run(mut args: AddArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let state = loaded.library.state_dir();

    // A positional PDF becomes an attachment as well as the thing identified.
    let pdf = args
        .source
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_file());
    if let Some(pdf) = &pdf
        && !args.files.contains(pdf)
    {
        args.files.push(pdf.clone());
    }

    let mut notes = Vec::new();
    let identifier = match (&args.from, &args.source, &pdf) {
        (Some(explicit), _, _) => Some(
            patterns::parse_identifier(explicit)
                .ok_or_else(|| anyhow!("`{explicit}` is not a DOI, arXiv ID or ISBN"))?,
        ),
        // A PDF: run the identification pipeline over it.
        (None, _, Some(pdf)) => {
            let backend =
                Poppler::new(loaded.config.pdf.clone()).with_cache(state.join("cache/text"));
            let found = identify::identify(&backend, pdf, &loaded.config.pdf);
            notes.extend(found.notes.iter().cloned());
            match found.best() {
                Some(best) => {
                    // Diagnostics go to stderr so stdout carries only the cite
                    // key: `key=$(bib add paper.pdf)` has to work, and a caller
                    // parsing output must not have to filter progress out of it.
                    eprintln!(
                        "identified {} ({}, {})",
                        best.id,
                        best.confidence.name(),
                        best.tier.name()
                    );
                    Some(best.id.clone())
                }
                None => {
                    eprintln!("no identifier found in {}", pdf.display());
                    // The title is all that is left to go on, and a title is
                    // enough to search with. This is the same machinery as
                    // `bib find`, so a PDF with no DOI still reaches the same
                    // catalogues by the same route.
                    let by_title = found.title.as_deref().and_then(|title| {
                        search_by_title(&loaded, &state, title, args.yes, args.offline)
                    });
                    if by_title.is_none() && args.title.is_none() {
                        args.title = found.title.clone();
                    }
                    by_title
                }
            }
        }
        // A bare argument that is not a file must be an identifier.
        (None, Some(text), None) => Some(
            patterns::parse_identifier(text)
                .ok_or_else(|| anyhow!("`{text}` is neither a file nor an identifier"))?,
        ),
        (None, None, None) => None,
    };

    // Provider metadata first, then explicit flags on top: a flag the user
    // typed is always more authoritative than a fetched field.
    let mut body = Value::Mapping(Mapping::new());
    let mut provenance = std::collections::BTreeMap::new();
    if let Some(id) = &identifier {
        let http = resolve::http(&loaded.config, state.join("cache/http"), args.offline);
        match resolve::resolve(&http, &loaded.config, id) {
            Ok(resolved) => {
                notes.extend(resolved.notes.iter().cloned());
                body = resolved.body;
                provenance = resolved.provenance;
            }
            Err(e) => {
                // A failed lookup should not lose the file: fall back to
                // recording the identifier and whatever flags were given.
                eprintln!("warning: {e:#}");
                body = identifier_only(id);
            }
        }
    }

    overlay(&mut body, &args)?;
    attach_meta(&mut body, &args, &provenance)?;

    let probe = bridge::to_entry("probe", &body).context("the assembled entry is not valid")?;
    let maker = KeyMaker::new(&loaded.config.citekey);
    let citekey = match &args.key {
        Some(explicit) => explicit.clone(),
        None => {
            let base = maker.render(&probe)?;
            let existing = store.documents().map(|(d, _)| d).unwrap_or_default();
            let taken = |candidate: &str| existing.iter().any(|d| d.citekey == candidate);
            maker.disambiguate(&base, &taken)?
        }
    };

    // A document already carrying this identifier is almost always the same
    // paper filed twice, which is worth catching before it happens.
    if let Some(id) = &identifier
        && let Some(existing) = duplicate_of(&store, id)?
    {
        bail!(
            "`{existing}` already has {id}\n\
             hint: `bib update {existing}` to re-fetch it, or --key to file a second copy"
        );
    }

    let entry = bridge::to_entry(&citekey, &body)?;
    let folder = maker.render_folder(&loaded.config.folder.template, &entry, &citekey)?;

    if args.dry_run {
        for note in &notes {
            eprintln!("  {note}");
        }
        println!("cite key : {citekey}");
        println!("folder   : {}", store.root().join(&folder).display());
        println!("---");
        print!("{}", serde_yaml::to_string(&body)?);
        return Ok(());
    }

    // Attachments are staged before the directory exists, so a missing source
    // file fails without leaving a half-built document behind.
    for source in &args.files {
        if !source.is_file() {
            bail!("no such file: {}", source.display());
        }
    }

    let doc = store.create(&citekey, &folder, &body)?;
    for source in &args.files {
        let name = source
            .file_name()
            .with_context(|| format!("{} has no filename", source.display()))?;
        std::fs::copy(source, doc.dir.join(name))
            .with_context(|| format!("could not copy {}", source.display()))?;
    }

    // A failed fetch never fails the add: the metadata is the deliverable and
    // the document is a bonus that many records simply do not have.
    if (args.fetch || loaded.config.fetch.auto) && doc.attachments().is_empty() {
        let http = resolve::http(&loaded.config, state.join("cache/http"), args.offline);
        match crate::cli::fetch_cmd::attach(&store, &http, &loaded.config, &doc, false) {
            Ok(Some(source)) => eprintln!("fetched {}", source.url),
            Ok(None) => {}
            Err(e) => eprintln!("warning: could not fetch a document: {e:#}"),
        }
    }

    println!("{citekey}  {}", doc.dir.display());
    Ok(())
}

/// Look for an existing document carrying the same identifier.
///
/// Uses the index, where serial numbers are `(kind, value)` lookup columns, so
/// this is an exact match rather than a walk of the whole library.
fn duplicate_of(store: &Store, id: &Identifier) -> Result<Option<String>> {
    let mut index = Index::open(store)?;
    index.sync(store)?;
    Ok(index.by_serial(id.kind(), id.value())?.into_iter().next())
}

/// A body carrying nothing but the identifier, for when every provider failed.
fn identifier_only(id: &Identifier) -> Value {
    let mut serial = Mapping::new();
    serial.insert(
        Value::String(id.kind().to_owned()),
        Value::String(id.value().to_owned()),
    );
    let mut map = Mapping::new();
    map.insert(Value::String("type".into()), Value::String("misc".into()));
    map.insert(
        Value::String("serial-number".into()),
        Value::Mapping(serial),
    );
    Value::Mapping(map)
}

/// Apply explicit flags over whatever the providers supplied.
fn overlay(body: &mut Value, args: &AddArgs) -> Result<()> {
    let Value::Mapping(map) = body else {
        bail!("the entry body is not a mapping");
    };
    macro_rules! put {
        ($key:expr, $value:expr) => {
            map.insert(Value::String($key.to_owned()), $value)
        };
    }

    if let Some(kind) = &args.entry_type {
        put!("type", Value::String(kind.clone()));
    } else if !map.contains_key(Value::String("type".into())) {
        // Providers always supply a type; this only fires on the offline path.
        put!("type", Value::String("article".into()));
    }
    if let Some(title) = &args.title {
        put!("title", Value::String(title.clone()));
    }
    if !args.authors.is_empty() {
        put!("author", strings(&args.authors));
    }
    if !args.editors.is_empty() {
        put!("editor", strings(&args.editors));
    }
    if let Some(date) = &args.date {
        put!("date", Value::String(date.clone()));
    }
    if let Some(url) = &args.url {
        put!("url", Value::String(url.clone()));
    }
    if let Some(publisher) = &args.publisher {
        put!("publisher", Value::String(publisher.clone()));
    }

    let explicit = [
        ("doi", &args.doi),
        ("isbn", &args.isbn),
        ("arxiv", &args.arxiv),
    ];
    if explicit.iter().any(|(_, v)| v.is_some()) {
        let serial = map
            .entry(Value::String("serial-number".into()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(serial) = serial {
            for (key, value) in explicit {
                if let Some(value) = value {
                    serial.insert(Value::String(key.to_owned()), Value::String(value.clone()));
                }
            }
        }
    }
    Ok(())
}

fn attach_meta(
    body: &mut Value,
    args: &AddArgs,
    provenance: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let meta = Meta {
        files: args
            .files
            .iter()
            .filter_map(|f| f.file_name().map(PathBuf::from))
            .collect(),
        tags: args.tags.clone(),
        added: Some(
            jiff::Timestamp::now()
                .strftime("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
        ),
        provenance: provenance.clone(),
        ..Meta::default()
    };
    bridge::set_meta(body, serde_yaml::to_value(meta)?)
}

fn strings(items: &[String]) -> Value {
    Value::Sequence(items.iter().cloned().map(Value::String).collect())
}

/// Offer catalogue matches for a PDF whose identifier could not be extracted.
///
/// Returns the chosen identifier, or `None` to file the document with whatever
/// metadata was recovered from the file itself. Never fails the add: a lookup
/// that finds nothing is a normal outcome for grey literature, lecture notes
/// and anything unpublished.
fn search_by_title(
    loaded: &config::Loaded,
    state: &std::path::Path,
    title: &str,
    yes: bool,
    offline: bool,
) -> Option<Identifier> {
    use crate::providers::search::{SearchQuery, rank};

    let query = SearchQuery {
        limit: 5,
        ..SearchQuery::new(title)
    };
    let http = resolve::http(&loaded.config, state.join("cache/http"), offline)
        .with_request_timeout(std::time::Duration::from_secs(8));
    let run = crate::providers::search_all(
        &http,
        &loaded.config.providers,
        &query,
        std::time::Duration::from_secs(8),
    );
    let candidates = rank(&query, run.results);
    let usable: Vec<_> = candidates.iter().filter(|c| c.id.is_some()).collect();
    if usable.is_empty() {
        return None;
    }

    let best = usable[0];
    // An exact title match is safe to take unattended; anything less needs a
    // human, because filing a paper under the wrong DOI is worse than filing it
    // under none.
    if yes {
        return (best.score >= 0.98).then(|| best.id.clone()).flatten();
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("  (not a terminal: pass --yes to accept an exact title match)");
        return None;
    }

    eprintln!("searched by title \"{title}\":");
    for (index, candidate) in usable.iter().enumerate() {
        eprintln!(
            "{:>2}  {}  {}",
            index + 1,
            candidate.id.as_ref().expect("filtered to Some"),
            candidate.title().unwrap_or_default()
        );
    }
    eprint!("use which? [1-{}, or blank for none] ", usable.len());
    std::io::stderr().flush().ok()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok()?;
    let chosen: usize = answer.trim().parse().ok()?;
    usable.get(chosen.checked_sub(1)?)?.id.clone()
}
