//! Import and export.

pub mod bibtex;
pub mod csl;
pub mod papis;

use crate::model::Document;
use crate::model::bridge;
use anyhow::{Result, anyhow};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExportFormat {
    /// Hayagriva YAML — what Typst reads directly.
    Hayagriva,
    /// BibTeX, for LaTeX toolchains.
    Bibtex,
    /// BibLaTeX, which preserves more structure than BibTeX.
    Biblatex,
}

/// Render documents in the requested format.
pub fn export(docs: &[Document], format: ExportFormat) -> Result<String> {
    let entries: Vec<hayagriva::Entry> = docs.iter().map(Document::entry).collect::<Result<_>>()?;
    let library = bridge::library_from(&entries);

    match format {
        ExportFormat::Hayagriva => hayagriva::io::to_yaml_str(&library).map_err(|e| anyhow!("{e}")),
        ExportFormat::Bibtex => bibtex::to_bibtex(&library, bibtex::Flavour::Bibtex),
        ExportFormat::Biblatex => bibtex::to_bibtex(&library, bibtex::Flavour::Biblatex),
    }
}
