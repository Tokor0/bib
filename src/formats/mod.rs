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

/// Render documents in the requested format, leaving out `exclude`d fields.
///
/// The exclusion applies to every format rather than to hayagriva alone: it
/// says what belongs in a bibliography, and that answer does not change because
/// the file is a `.bib`. Names are checked first, so a typo is an error here
/// rather than a field that quietly stayed in.
pub fn export(docs: &[Document], format: ExportFormat, exclude: &[String]) -> Result<String> {
    bridge::check_fields(exclude)?;
    let entries: Vec<hayagriva::Entry> = docs
        .iter()
        .map(|doc| bridge::to_entry(&doc.citekey, &bridge::prune(&doc.value, exclude)))
        .collect::<Result<_>>()?;
    let library = bridge::library_from(&entries);

    match format {
        ExportFormat::Hayagriva => hayagriva::io::to_yaml_str(&library).map_err(|e| anyhow!("{e}")),
        ExportFormat::Bibtex => bibtex::to_bibtex(&library, bibtex::Flavour::Bibtex),
        ExportFormat::Biblatex => bibtex::to_bibtex(&library, bibtex::Flavour::Biblatex),
    }
}
