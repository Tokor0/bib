//! `bib export` — render a bibliography.

use crate::config;
use crate::formats::{self, ExportFormat};
use crate::index::{Index, query};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Cite keys to include. Omit to export the whole library.
    pub citekeys: Vec<String>,

    /// Export everything matching a query instead, e.g.
    /// `-q 'author:einstein year:1905-1910'`.
    ///
    /// Kept separate from the positional cite keys rather than overloading
    /// them: a bare word is a valid cite key *and* a valid full-text search,
    /// and silently guessing wrong would produce a bibliography with the
    /// wrong entries in it.
    #[arg(long, short = 'q', conflicts_with = "citekeys")]
    pub query: Option<String>,

    #[arg(long, short = 'f', value_enum, default_value_t = ExportFormat::Hayagriva)]
    pub format: ExportFormat,

    /// Leave a field out of the bibliography, e.g. `--exclude abstract`.
    /// Repeatable.
    ///
    /// Replaces the configured list rather than adding to it, so a command line
    /// that mentions exclusions describes all of them.
    #[arg(long, value_name = "FIELD")]
    pub exclude: Vec<String>,

    /// Export every field, including the ones configuration excludes.
    #[arg(long, conflicts_with = "exclude")]
    pub all_fields: bool,

    /// Write to a file instead of stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
}

pub fn run(args: ExportArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let (all, errors) = store.documents()?;

    let selected = if let Some(text) = &args.query {
        // The index is the only thing that understands the query language, so
        // it is refreshed first — an export built from a stale index would
        // silently omit entries the user just added.
        let mut index = Index::open(&store)?;
        index.sync(&store)?;
        let parsed = query::parse(text).with_context(|| format!("in query `{text}`"))?;
        let hits = index.search(&parsed)?;
        if hits.is_empty() {
            bail!("no documents match `{text}`");
        }
        hits.iter()
            .map(|hit| store.load(&hit.dir))
            .collect::<Result<Vec<_>>>()?
    } else if args.citekeys.is_empty() {
        all
    } else {
        // Every requested key must exist: silently omitting one would produce a
        // bibliography with dangling citations, which fails far away from here.
        let mut picked = Vec::new();
        for key in &args.citekeys {
            match all.iter().find(|d| &d.citekey == key) {
                Some(doc) => picked.push(doc.clone()),
                None => bail!("no document with cite key `{key}`"),
            }
        }
        picked
    };

    if !errors.is_empty() {
        eprintln!(
            "warning: {} document(s) could not be loaded and are not in this export",
            errors.len()
        );
    }

    // Command line first, then configuration: `--all-fields` is how a one-off
    // export gets back what the configured list normally drops.
    let exclude: &[String] = match () {
        () if args.all_fields => &[],
        () if !args.exclude.is_empty() => &args.exclude,
        () => &loaded.config.export.exclude,
    };
    let rendered = formats::export(&selected, args.format, exclude)?;

    match &args.output {
        Some(path) => {
            std::fs::write(path, &rendered)
                .with_context(|| format!("could not write {}", path.display()))?;
            eprintln!("{} entries -> {}", selected.len(), path.display());
        }
        None => print!("{rendered}"),
    }
    Ok(())
}
