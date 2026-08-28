//! `bib import` — bring documents in from other formats.

use crate::config;
use crate::formats::{bibtex, papis};
use crate::model::citekey::KeyMaker;
use crate::model::{Document, bridge};
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

/// One document on its way into the library.
///
/// `dir` is where the source kept this document's files, when the source is a
/// directory of documents rather than a single bibliography file. It is what
/// lets an import bring the PDFs with it instead of recording the names of
/// files that are not there.
struct Incoming {
    key: String,
    body: Value,
    dir: Option<PathBuf>,
}

impl ImportSource {
    pub fn run(self, library: Option<&str>) -> Result<()> {
        match self {
            Self::Bibtex(a) => {
                let text = read(&a.path)?;
                let lib = bibtex::from_bibtex(&text)?;
                let items = lib
                    .into_iter()
                    .map(|e| {
                        Ok(Incoming {
                            key: e.key().to_owned(),
                            body: bridge::from_entry(&e)?,
                            dir: None,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                ingest(items, &a.common, library)
            }
            Self::Hayagriva(a) => {
                let text = read(&a.path)?;
                let lib = hayagriva::io::from_yaml_str(&text)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", a.path.display()))?;
                let items = lib
                    .into_iter()
                    .map(|e| {
                        Ok(Incoming {
                            key: e.key().to_owned(),
                            body: bridge::from_entry(&e)?,
                            dir: None,
                        })
                    })
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
fn read_papis(dir: &Path) -> Result<Vec<Incoming>> {
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

fn collect_papis(dir: &Path, out: &mut Vec<Incoming>) -> Result<()> {
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
        out.push(Incoming {
            key,
            body,
            dir: Some(dir.to_path_buf()),
        });
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

/// What became of one document's attachments.
struct Brought {
    copied: usize,
    /// Files the record named that the source did not have, as
    /// `citekey/filename`, already removed from the record.
    missing: Vec<String>,
}

/// Copy a document's attachments out of the library it came from.
///
/// The record is corrected to match what actually arrived. A `files:` list
/// naming something that is not on disk is worse than no list at all: it is
/// what makes `bib fetch` report "already has an attachment" for a document
/// with no attachment, so the entries that most need a PDF are the ones it
/// skips. A file the source has lost is therefore dropped from the record and
/// reported, not carried over on faith.
fn take_files(store: &Store, doc: &Document, from: &Path) -> Result<Brought> {
    let mut meta = doc.meta();
    if meta.files.is_empty() {
        return Ok(Brought {
            copied: 0,
            missing: Vec::new(),
        });
    }

    let mut kept = Vec::new();
    let mut missing = Vec::new();
    for file in &meta.files {
        let Some(name) = file.file_name() else {
            continue;
        };
        let source = from.join(file);
        if !source.is_file() {
            missing.push(format!("{}/{}", doc.citekey, file.display()));
            continue;
        }
        std::fs::copy(&source, doc.dir.join(name))
            .with_context(|| format!("could not copy {}", source.display()))?;
        // Flattened to a bare name: the file now lives beside its `info.yml`,
        // whatever nesting the source used.
        kept.push(PathBuf::from(name));
    }

    let copied = kept.len();
    // Rewritten only when the list actually changed, so an import that brought
    // everything leaves the file it just wrote alone.
    if kept != meta.files {
        meta.files = kept;
        let mut value = doc.value.clone();
        bridge::set_meta(&mut value, serde_yaml::to_value(&meta)?)?;
        store.save(&Document {
            citekey: doc.citekey.clone(),
            dir: doc.dir.clone(),
            value,
        })?;
    }

    Ok(Brought { copied, missing })
}

/// Write imported documents into the library, resolving keys and collisions.
fn ingest(items: Vec<Incoming>, common: &CommonArgs, library: Option<&str>) -> Result<()> {
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
    let mut copied = 0usize;
    let mut lost = Vec::new();
    let mut failed = Vec::new();

    for item in items {
        let Incoming {
            key: source_key,
            body,
            dir: source_dir,
        } = item;
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
                let doc = store.create(&key, &folder, &body)?;
                if let Some(source_dir) = &source_dir {
                    let brought = take_files(&store, &doc, source_dir)?;
                    copied += brought.copied;
                    lost.extend(brought.missing);
                }
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
    if copied > 0 {
        eprintln!("copied {copied} attachment(s)");
    }
    if !lost.is_empty() {
        eprintln!(
            "{} attachment(s) named by the source were not there and have been \
             dropped from the record; `bib fetch` can look for them:",
            lost.len()
        );
        for name in &lost {
            eprintln!("  {name}");
        }
    }
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
