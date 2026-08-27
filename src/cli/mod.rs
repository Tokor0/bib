//! Command-line surface.

pub mod add;
pub mod config_cmd;
pub mod docs;
pub mod export;
pub mod fetch_cmd;
pub mod find;
pub mod identify_cmd;
pub mod import;
pub mod open_cmd;
pub mod resolve;
pub mod result;
pub mod search;
pub mod update;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "bib", version, about, long_about = None)]
pub struct Cli {
    /// Library to operate on (defaults to `default_library`).
    #[arg(long, short = 'L', global = true)]
    pub library: Option<String>,

    /// Increase logging verbosity; repeat for more detail.
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect and modify configuration.
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },

    /// Add a document.
    // Boxed: `AddArgs` carries many optional fields and would otherwise set the
    // size of every `Command` variant.
    Add(Box<add::AddArgs>),
    /// List documents in the library.
    List(search::ListArgs),
    /// Identify a PDF: DOI, arXiv ID or ISBN, without writing anything.
    Identify(identify_cmd::IdentifyArgs),
    /// Show one document's metadata.
    Show(docs::ShowArgs),
    /// Open a document's attachment in the configured viewer.
    Open(open_cmd::OpenArgs),
    /// Open a document's `info.yml` in $EDITOR.
    Edit(docs::EditArgs),
    /// Remove a document and its files.
    #[command(alias = "remove")]
    Rm(docs::RemoveArgs),

    /// Search the library.
    Search(search::SearchArgs),
    /// Search the web for documents not in the library yet.
    Find(find::FindArgs),
    /// Download the document for entries that have none.
    Fetch(fetch_cmd::FetchArgs),
    /// Export a bibliography.
    Export(export::ExportArgs),
    /// Import from BibTeX, a papis library, or hayagriva YAML.
    Import {
        #[command(subcommand)]
        source: import::ImportSource,
    },
    /// Build or refresh the search index.
    Index(search::IndexArgs),
    /// Re-fetch metadata for documents already in the library.
    Update(update::UpdateArgs),
    /// Check the library for problems.
    Doctor,
}

impl Cli {
    pub fn run(self) -> Result<()> {
        let lib = self.library.as_deref();
        match self.command {
            Command::Config { action } => action.run(lib),
            Command::Add(args) => add::run(*args, lib),
            Command::List(args) => search::list(args, lib),
            Command::Identify(args) => identify_cmd::run(args, lib),
            Command::Show(args) => docs::show(args, lib),
            Command::Open(args) => open_cmd::run(args, lib),
            Command::Edit(args) => docs::edit(args, lib),
            Command::Rm(args) => docs::remove(args, lib),

            Command::Search(args) => search::search(args, lib),
            Command::Find(args) => find::run(args, lib),
            Command::Fetch(args) => fetch_cmd::run(args, lib),
            Command::Export(args) => export::run(args, lib),
            Command::Import { source } => source.run(lib),
            Command::Index(args) => search::index(args, lib),
            Command::Update(args) => update::run(args, lib),
            Command::Doctor => unimplemented("doctor", 6),
        }
    }
}

/// Placeholder for commands whose milestone has not landed yet. Failing loudly
/// with the milestone number beats a command that silently does nothing.
fn unimplemented(name: &str, milestone: u8) -> Result<()> {
    bail!("`bib {name}` is not implemented yet (planned for milestone {milestone})")
}
