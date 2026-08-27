//! The on-disk library: discovery, reading and writing.
//!
//! A library is a directory tree; any directory containing [`INFO_FILE`] is one
//! document, at any depth. Directories without it are ignored, which is what
//! lets a library hold loose files and per-project subtrees.

use crate::config::{LIBRARY_STATE_DIR, ResolvedLibrary};
use crate::model::{Document, bridge, label};
use anyhow::{Context, Result, anyhow, bail};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

/// Metadata filename inside a document directory.
pub const INFO_FILE: &str = "info.yml";

/// Also recognised on read, for libraries migrated from papis.
pub const INFO_FILE_ALT: &str = "info.yaml";

pub struct Store {
    pub library: ResolvedLibrary,
}

impl Store {
    pub fn new(library: ResolvedLibrary) -> Self {
        Self { library }
    }

    pub fn root(&self) -> &Path {
        &self.library.dir
    }

    /// Walk the library, returning every document.
    ///
    /// Invalid documents are collected alongside the good ones rather than
    /// aborting the walk: one malformed file should not make the whole library
    /// unusable.
    pub fn documents(&self) -> Result<(Vec<Document>, Vec<LoadError>)> {
        if !self.root().exists() {
            bail!(
                "library `{}` does not exist at {}",
                self.library.name,
                self.root().display()
            );
        }

        let mut found = Vec::new();
        let mut errors = Vec::new();
        walk(self.root(), &mut |dir: &Path| {
            let Some(info) = info_path_in(dir) else {
                return;
            };
            match load_document(dir, &info) {
                Ok(doc) => found.push(doc),
                Err(e) => errors.push(LoadError {
                    path: info,
                    error: e,
                }),
            }
        })?;

        found.sort_by(|a, b| a.citekey.cmp(&b.citekey));
        Ok((found, errors))
    }

    /// Look up one document by cite key.
    pub fn get(&self, citekey: &str) -> Result<Document> {
        let (docs, _) = self.documents()?;
        docs.into_iter()
            .find(|d| d.citekey == citekey)
            .ok_or_else(|| anyhow!("no document with cite key `{citekey}`"))
    }

    /// Create a document directory and write its `info.yml`.
    ///
    /// `relative_dir` comes from the folder template and may nest.
    pub fn create(&self, citekey: &str, relative_dir: &Path, value: &Value) -> Result<Document> {
        // The key is what the user writes as `@key` in Typst and, via the
        // folder template, usually also a directory name. An unvalidated key
        // corrupts both: a BibTeX key like `DBLP:conf/nips/Vaswani17` creates
        // nested directories, and `citekey_from_dir` then reads the key back
        // as just `Vaswani17` — silently not the entry the user imported.
        if let Err(problem) = label::validate(citekey) {
            bail!(
                "`{citekey}` cannot be used as a cite key: {problem}\n\
                 hint: import with --rekey to generate keys from the template"
            );
        }

        let dir = self.root().join(relative_dir);
        if dir.exists() {
            bail!("{} already exists", dir.display());
        }
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;

        let doc = Document {
            citekey: citekey.to_owned(),
            dir,
            value: value.clone(),
        };
        doc.validate()?;
        write_info(&doc)?;
        Ok(doc)
    }

    /// Overwrite an existing document's `info.yml`, validating first so a bad
    /// write cannot corrupt the library.
    pub fn save(&self, doc: &Document) -> Result<()> {
        doc.validate()?;
        write_info(doc)
    }

    /// Delete a document directory and everything in it.
    pub fn remove(&self, doc: &Document) -> Result<()> {
        // Refuse to recurse outside the library, however we got here.
        if !doc.dir.starts_with(self.root()) {
            bail!(
                "{} is outside library `{}`",
                doc.dir.display(),
                self.library.name
            );
        }
        std::fs::remove_dir_all(&doc.dir)
            .with_context(|| format!("could not remove {}", doc.dir.display()))
    }

    /// Locate every document directory without parsing any `info.yml`.
    ///
    /// The index needs this to decide what actually changed: stat is orders of
    /// magnitude cheaper than a YAML parse, and on a library where nothing has
    /// changed it turns a full reindex into a directory walk.
    pub fn document_paths(&self) -> Result<Vec<DocumentPath>> {
        if !self.root().exists() {
            bail!(
                "library `{}` does not exist at {}",
                self.library.name,
                self.root().display()
            );
        }

        let mut found = Vec::new();
        walk(self.root(), &mut |dir: &Path| {
            let Some(info) = info_path_in(dir) else {
                return;
            };
            // A directory name that is not usable as a cite key cannot be
            // indexed, but it also cannot be loaded, so `documents()` will
            // report it. Skipping here keeps the two consistent.
            if let Ok(citekey) = citekey_from_dir(dir) {
                found.push(DocumentPath {
                    citekey,
                    dir: dir.to_path_buf(),
                    info,
                });
            }
        })?;
        Ok(found)
    }

    /// Read and validate the document in `dir`.
    pub fn load(&self, dir: &Path) -> Result<Document> {
        let info = info_path_in(dir)
            .ok_or_else(|| anyhow!("{} contains no {INFO_FILE}", dir.display()))?;
        load_document(dir, &info)
    }
}

impl Document {
    /// Check the body parses as a hayagriva entry.
    pub fn validate(&self) -> Result<()> {
        self.entry().map(|_| ())
    }
}

#[derive(Debug)]
pub struct LoadError {
    pub path: PathBuf,
    pub error: anyhow::Error,
}

/// `info.yml`, or a papis-style `info.yaml`, whichever is present.
fn info_path_in(dir: &Path) -> Option<PathBuf> {
    for name in [INFO_FILE, INFO_FILE_ALT] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn load_document(dir: &Path, info: &Path) -> Result<Document> {
    let text = std::fs::read_to_string(info)
        .with_context(|| format!("could not read {}", info.display()))?;
    let value: Value = serde_yaml::from_str(&text)
        .with_context(|| format!("{} is not valid YAML", info.display()))?;

    if !value.is_mapping() {
        bail!("{} should contain a mapping of fields", info.display());
    }

    let citekey = citekey_from_dir(dir)?;
    // Surface schema errors at load time, naming the file rather than the key.
    bridge::to_entry(&citekey, &value).with_context(|| format!("in {}", info.display()))?;

    Ok(Document {
        citekey,
        dir: dir.to_path_buf(),
        value,
    })
}

/// The cite key is the document's directory name. Keeping it out of `info.yml`
/// means there is exactly one source of truth and the two cannot disagree.
fn citekey_from_dir(dir: &Path) -> Result<String> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} has no usable directory name", dir.display()))
}

fn write_info(doc: &Document) -> Result<()> {
    let path = doc.info_path();
    let yaml = serde_yaml::to_string(&doc.value)
        .with_context(|| format!("could not serialize {}", path.display()))?;

    // Write via a temporary file in the same directory so an interrupted write
    // cannot truncate a good `info.yml`.
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

/// Depth-first walk, skipping the library state directory and anything hidden.
fn walk(dir: &Path, visit: &mut dyn FnMut(&Path)) -> Result<()> {
    visit(dir);

    let entries =
        std::fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))?;

    for entry in entries {
        let entry =
            entry.with_context(|| format!("could not read an entry in {}", dir.display()))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `.bib` holds the index and caches; other dot-directories are the
        // user's business (`.git` above all) and are not documents.
        if name == LIBRARY_STATE_DIR || name.starts_with('.') {
            continue;
        }
        walk(&entry.path(), visit)?;
    }
    Ok(())
}

/// Where a document lives, before anything has been parsed.
#[derive(Debug, Clone)]
pub struct DocumentPath {
    pub citekey: String,
    pub dir: PathBuf,
    pub info: PathBuf,
}
