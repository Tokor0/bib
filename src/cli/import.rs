//! `bib import` — bring documents in from other formats.

use crate::config;
use crate::formats::{bibtex, papis};
use crate::model::bridge;
use crate::model::citekey::KeyMaker;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum ImportSource {
    /// Import a BibTeX or BibLaTeX file.
    Bibtex(FileArgs),
    /// Import a hayagriva YAML bibliography.
    Hayagriva(FileArgs),
    /// Import an existing papis library.
    Papis(DirArgs),
}

#[derive(Debug, Args)]
pub struct FileArgs {
    pub path: PathBuf,
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args)]
pub struct DirArgs {
    pub dir: PathBuf,
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Re-render cite keys from the configured template instead of keeping the
    /// source's own keys.
    #[arg(long)]
    pub rekey: bool,

    /// Report what would be imported without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

impl ImportSource {
    pub fn run(self, library: Option<&str>) -> Result<()> {
        match self {
            Self::Bibtex(a) => {
                let text = read(&a.path)?;
                let lib = bibtex::from_bibtex(&text)?;
                let items = lib
                    .into_iter()
                    .map(|e| Ok((e.key().to_owned(), bridge::from_entry(&e)?)))
                    .collect::<Result<Vec<_>>>()?;
                ingest(items, &a.common, library)
            }
            Self::Hayagriva(a) => {
                let text = read(&a.path)?;
                let lib = hayagriva::io::from_yaml_str(&text)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", a.path.display()))?;
                let items = lib
                    .into_iter()
                    .map(|e| Ok((e.key().to_owned(), bridge::from_entry(&e)?)))
                    .collect::<Result<Vec<_>>>()?;
                ingest(items, &a.common, library)
            }
            Self::Papis(a) => ingest(read_papis(&a.dir)?, &a.common, library),
        }
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))
}

/// Collect `info.yaml` files from a papis library and map them.
fn read_papis(dir: &Path) -> Result<Vec<(String, Value)>> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut out = Vec::new();
    collect_papis(dir, &mut out)?;
    if out.is_empty() {
        bail!(
            "no papis documents (info.yaml) found under {}",
            dir.display()
        );
    }
    Ok(out)
}

fn collect_papis(dir: &Path, out: &mut Vec<(String, Value)>) -> Result<()> {
    for name in ["info.yaml", "info.yml"] {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        let text = read(&candidate)?;
        let papis: Value = serde_yaml::from_str(&text)
            .with_context(|| format!("{} is not valid YAML", candidate.display()))?;

        let mut body = papis::to_body(&papis);
        bridge::set_meta(&mut body, papis::to_meta(&papis))?;

        // papis's own `ref` is its cite key; fall back to the folder name.
        let key = papis
            .get("ref")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| dir.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "imported".to_owned());
        out.push((key, body));
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            collect_papis(&entry.path(), out)?;
        }
    }
    Ok(())
}

/// Write imported documents into the library, resolving keys and collisions.
fn ingest(items: Vec<(String, Value)>, common: &CommonArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let maker = KeyMaker::new(&loaded.config.citekey);

    // Existing keys plus keys claimed earlier in this same run, so a batch
    // cannot collide with itself.
    let mut taken: Vec<String> = store
        .documents()
        .map(|(d, _)| d)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.citekey)
        .collect();

    let mut imported = 0usize;
    let mut failed = Vec::new();

    for (source_key, body) in items {
        let result = (|| -> Result<String> {
            let probe = bridge::to_entry(&source_key, &body)?;
            let base = if common.rekey {
                maker.render(&probe)?
            } else {
                source_key.clone()
            };
            let key = maker.disambiguate(&base, &|c: &str| taken.iter().any(|t| t == c))?;

            let entry = bridge::to_entry(&key, &body)?;
            let folder = maker.render_folder(&loaded.config.folder.template, &entry, &key)?;
            if !common.dry_run {
                store.create(&key, &folder, &body)?;
            }
            Ok(key)
        })();

        match result {
            Ok(key) => {
                if common.dry_run {
                    println!("would import {source_key} -> {key}");
                }
                taken.push(key);
                imported += 1;
            }
            Err(e) => failed.push((source_key, e)),
        }
    }

    let verb = if common.dry_run {
        "would import"
    } else {
        "imported"
    };
    println!("{verb} {imported} document(s)");
    if !failed.is_empty() {
        eprintln!("{} could not be imported:", failed.len());
        for (key, e) in &failed {
            eprintln!("  {key}: {e:#}");
        }
        // A partial import is a success with warnings — the good entries are
        // on disk. Importing nothing at all is a failure, and scripts must be
        // able to tell the difference from the exit status alone.
        if imported == 0 {
            bail!("no documents were imported");
        }
    }
    Ok(())
}
