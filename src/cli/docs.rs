//! `bib show`, `edit` and `rm`. Listing and search live in `search.rs`.

use crate::config;
use crate::model::Document;
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use std::io::{IsTerminal, Write};

#[derive(Debug, Args)]
pub struct ShowArgs {
    pub citekey: String,
    /// Print the hayagriva entry as it would be exported, without `x-bib`.
    #[arg(long)]
    pub entry: bool,
}

pub fn show(args: ShowArgs, library: Option<&str>) -> Result<()> {
    let doc = open(library)?.get(&args.citekey)?;
    if args.entry {
        let library = crate::model::bridge::library_from([&doc.entry()?]);
        print!(
            "{}",
            hayagriva::io::to_yaml_str(&library).map_err(|e| anyhow!("{e}"))?
        );
    } else {
        print!("{}", serde_yaml::to_string(&doc.value)?);
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct EditArgs {
    pub citekey: String,
}

pub fn edit(args: EditArgs, library: Option<&str>) -> Result<()> {
    let store = open(library)?;
    let doc = store.get(&args.citekey)?;
    let path = doc.info_path();

    let editor = std::env::var_os("VISUAL")
        .or_else(|| std::env::var_os("EDITOR"))
        .ok_or_else(|| anyhow!("neither $VISUAL nor $EDITOR is set"))?;

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("could not launch {}", editor.to_string_lossy()))?;
    if !status.success() {
        bail!("{} exited with {status}", editor.to_string_lossy());
    }

    // Re-read so a hand edit that breaks the schema is reported now, while the
    // user still remembers what they changed.
    let reloaded = store.get(&args.citekey)?;
    reloaded
        .validate()
        .with_context(|| format!("{} is no longer a valid entry", path.display()))?;
    Ok(())
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    pub citekey: String,
    /// Skip the confirmation prompt.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn remove(args: RemoveArgs, library: Option<&str>) -> Result<()> {
    let store = open(library)?;
    let doc = store.get(&args.citekey)?;

    if !args.yes {
        // Deleting a directory is not reversible, so refuse to guess when there
        // is no one to ask.
        if !std::io::stdin().is_terminal() {
            bail!(
                "refusing to remove `{}` without --yes when not on a terminal",
                args.citekey
            );
        }
        summarize(&doc)?;
        print!("remove {} and its files? [y/N] ", doc.dir.display());
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("cancelled");
            return Ok(());
        }
    }

    store.remove(&doc)?;
    println!("removed {}", args.citekey);
    Ok(())
}

fn summarize(doc: &Document) -> Result<()> {
    let entry = doc.entry()?;
    if let Some(title) = entry.title() {
        println!("{}  {}", doc.citekey, title);
    }
    let files = doc.files();
    if !files.is_empty() {
        println!("{} attachment(s)", files.len());
    }
    Ok(())
}

fn open(library: Option<&str>) -> Result<Store> {
    Ok(Store::new(config::load(library)?.library))
}
