//! `bib identify` — run the identification pipeline and explain the result.
//!
//! Read-only and offline. It exists so the pipeline is inspectable from the
//! shell: when `bib add` files something under the wrong DOI, this shows which
//! tier produced it and what text it was looking at.

use crate::config;
use crate::identify::{self, backend::Poppler};
use anyhow::{Context, Result, bail};
use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct IdentifyArgs {
    /// PDF to inspect.
    pub path: PathBuf,

    /// Show every candidate with its tier, confidence and surrounding text,
    /// plus the tiers that produced nothing.
    #[arg(long)]
    pub explain: bool,
}

pub fn run(args: IdentifyArgs, library: Option<&str>) -> Result<()> {
    if !args.path.is_file() {
        bail!("{} is not a file", args.path.display());
    }
    let loaded = config::load(library)?;

    let cache = loaded.library.state_dir().join("cache/text");
    let backend = Poppler::new(loaded.config.pdf.clone()).with_cache(cache);
    let found = identify::identify(&backend, &args.path, &loaded.config.pdf);

    if args.explain {
        for note in &found.notes {
            println!("  {note}");
        }
        if let Some(title) = &found.title {
            println!("  title: {title}");
        }
        println!();
    }

    if found.candidates.is_empty() {
        println!("no identifier found");
        if !args.explain {
            println!("run with --explain to see what was tried");
        }
        // Not an error: an unidentifiable PDF is a normal outcome, and the
        // caller may still want the title-search fallback.
        return Ok(());
    }

    // Without --explain this is an answer, not a report: print the identifier a
    // caller should act on. A paper's bibliography can contribute dozens of
    // low-confidence candidates, and listing them by default buries the one
    // that matters.
    if !args.explain {
        println!("{}", found.candidates[0].id);
        return Ok(());
    }

    for candidate in &found.candidates {
        println!(
            "{:<9} {:<8} {:<17} {}",
            candidate.id.kind(),
            candidate.confidence.name(),
            candidate.tier.name(),
            candidate.id.value()
        );
        println!("          {}", candidate.context);
    }
    Ok(())
}

/// Resolve a PDF path relative to the working directory, for `bib add`.
pub fn canonical(path: &std::path::Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("could not resolve {}", path.display()))
}
