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

    /// Absolute paths of the document's attachments, as recorded.
    pub fn files(&self) -> Vec<PathBuf> {
        self.meta().files.iter().map(|f| self.dir.join(f)).collect()
    }

    /// The attachments that are actually on disk.
    ///
    /// The `files:` list is a record, not a guarantee: an import can carry the
    /// names of files it did not copy, and a file can be moved out from under
    /// the library. Anything deciding whether a document *has* its document —
    /// `bib fetch` above all — has to ask the filesystem, or it skips precisely
    /// the entries that are missing one.
    pub fn attachments(&self) -> Vec<PathBuf> {
        self.files().into_iter().filter(|p| p.is_file()).collect()
    }

    pub fn info_path(&self) -> PathBuf {
        self.dir.join(crate::store::INFO_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(dir: PathBuf, files: &[&str]) -> Document {
        let list = files
            .iter()
            .map(|f| format!("  - {f}\n"))
            .collect::<String>();
        Document {
            citekey: "t".to_owned(),
            dir,
            value: serde_yaml::from_str(&format!(
                "type: article\ntitle: X\nx-bib:\n  files:\n{list}"
            ))
            .expect("valid YAML"),
        }
    }

    /// The distinction `bib fetch` turns on: what the record says, and what is
    /// actually there.
    #[test]
    fn only_files_on_disk_count_as_attachments() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("here.pdf"), b"%PDF-1.7\n").unwrap();
        let doc = document(temp.path().to_path_buf(), &["here.pdf", "gone.pdf"]);

        assert_eq!(doc.files().len(), 2, "the record names both");
        assert_eq!(doc.attachments().len(), 1, "only one is there");
        assert!(doc.attachments()[0].ends_with("here.pdf"));
    }

    #[test]
    fn a_record_naming_a_file_that_is_not_there_has_no_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let doc = document(temp.path().to_path_buf(), &["gone.pdf"]);
        assert!(doc.attachments().is_empty());
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
