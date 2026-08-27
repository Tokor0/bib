//! The in-memory document model.

pub mod bridge;
pub mod citekey;
pub mod label;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One document: a directory holding `info.yml` plus its attachments.
#[derive(Debug, Clone)]
pub struct Document {
    /// Cite key. Derived from the directory name, not stored in `info.yml`.
    pub citekey: String,
    /// Directory containing `info.yml`, absolute.
    pub dir: PathBuf,
    /// Whole parsed `info.yml`, including the `x-bib` block. Authoritative:
    /// hayagriva validates and exports it but never owns it.
    pub value: Value,
}

impl Document {
    /// Validate the body as a hayagriva entry.
    pub fn entry(&self) -> Result<hayagriva::Entry> {
        bridge::to_entry(&self.citekey, &self.value)
    }

    /// Read the `x-bib` block as a typed view. Unknown keys inside it are
    /// ignored here but preserved on disk, since writes go through
    /// [`Document::value`] rather than being rebuilt from this struct.
    pub fn meta(&self) -> Meta {
        bridge::meta_of(&self.value)
            .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Absolute paths of the document's attachments.
    pub fn files(&self) -> Vec<PathBuf> {
        self.meta().files.iter().map(|f| self.dir.join(f)).collect()
    }

    pub fn info_path(&self) -> PathBuf {
        self.dir.join(crate::store::INFO_FILE)
    }
}

/// Our half of `info.yml` — everything hayagriva does not model.
///
/// Deliberately not `deny_unknown_fields`: users may keep their own keys here,
/// papis-style, and those must survive a load/save cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    /// Attachment filenames, relative to the document directory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PathBuf>,
    /// Note file, relative to the document directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// RFC 3339 timestamp recording when the document was added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<String>,
    /// Which provider supplied each field, so `bib update` can re-fetch
    /// selectively and a wrong value can be traced to its source.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, String>,
}
