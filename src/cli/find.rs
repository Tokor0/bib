//! `bib find` — search the web for documents that are not in the library yet.
//!
//! Deliberately separate from `bib search`, which queries the local index and
//! must stay instant and offline. Keeping them apart means a fast local query
//! can never silently make an HTTP request, and each gets the flags that
//! actually apply to it.

use crate::cli::resolve;
use crate::cli::result::{SearchResult, to_json};
use crate::config;
use crate::identify::patterns::{self, Identifier};
use crate::index::Index;
use crate::providers::search::{Candidate, SearchQuery, rank};
use crate::providers::{self};
use crate::store::Store;
use anyhow::{Result, anyhow, bail};
use clap::Args;
use std::io::{IsTerminal, Write};
use std::time::Duration;

#[derive(Debug, Args)]
pub struct FindArgs {
    /// What to look for — normally a title. Words are joined with spaces.
    pub query: Vec<String>,

    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub year: Option<i64>,
    /// Restrict to a CSL type, e.g. `book`.
    #[arg(long = "type")]
    pub kind: Option<String>,

    /// Providers to ask, overriding the configured order.
    #[arg(long, value_delimiter = ',')]
    pub provider: Vec<String>,

    #[arg(long, short = 'n', default_value_t = 10)]
    pub limit: usize,

    /// Give up on providers that have not answered within this budget and
    /// return what did arrive.
    #[arg(long, default_value = "8s", value_parser = humantime::parse_duration)]
    pub timeout: Duration,

    /// File one result: either its position in the list or its identifier.
    ///
    /// The identifier form is the stable one — a position is only meaningful
    /// against a list you have just seen.
    #[arg(long, value_name = "N|ID")]
    pub add: Option<String>,

    /// Pick a result interactively.
    #[arg(long, short = 'i', conflicts_with = "add")]
    pub interactive: bool,

    /// Also download the PDF for whatever is added.
    #[arg(long, requires = "add")]
    pub fetch: bool,

    #[arg(long, conflicts_with = "format")]
    pub json: bool,

    /// Render each result with a minijinja template.
    #[arg(long, short = 'F')]
    pub format: Option<String>,

    /// Answer from the cache only; never call a provider.
    #[arg(long)]
    pub offline: bool,
}

pub fn run(args: FindArgs, library: Option<&str>) -> Result<()> {
    let text = args.query.join(" ");
    if text.trim().is_empty() {
        bail!("nothing to search for");
    }

    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let state = loaded.library.state_dir();

    let mut providers = loaded.config.providers.clone();
    if !args.provider.is_empty() {
        providers.order = args.provider.clone();
    }

    let query = SearchQuery {
        text: text.clone(),
        author: args.author.clone(),
        year: args.year,
        kind: args.kind.clone(),
        limit: args.limit.max(1),
        citation_like: false,
    };

    let http = resolve::http(&loaded.config, state.join("cache/http"), args.offline)
        // No single stalled connection may consume the whole budget; the rest
        // of the providers still get their turn.
        .with_request_timeout(args.timeout);
    let run = providers::search_all(&http, &providers, &query, args.timeout);

    // Notes are diagnostics: stderr, always, so `--json` stdout stays parseable.
    for note in &run.notes {
        eprintln!("  {note}");
    }
    if run.partial {
        eprintln!("  (partial results: raise --timeout to wait longer)");
    }

    let candidates = rank(&query, run.results);
    let results = present(&store, &candidates)?;

    if let Some(selector) = &args.add {
        return add_one(&loaded, &store, &candidates, selector, args.fetch);
    }
    if args.interactive {
        let Some(chosen) = prompt(&results)? else {
            return Ok(());
        };
        return add_one(&loaded, &store, &candidates, &chosen, args.fetch);
    }

    if args.json {
        print!("{}", to_json(&results)?);
        return Ok(());
    }
    if let Some(template) = &args.format {
        let no_entries = vec![None; results.len()];
        print!(
            "{}",
            crate::cli::result::render_results(&results, &no_entries, template)?
        );
        return Ok(());
    }

    if results.is_empty() {
        eprintln!("no results");
        // Nothing matched is an answer, not a failure: a caller should not have
        // to tell "no results" from "bib is broken" by exit code alone.
        return Ok(());
    }
    for (index, result) in results.iter().enumerate() {
        let badge = if result.in_library {
            "  [in library]"
        } else {
            ""
        };
        println!("{:>2}  {:<22}  {}", index + 1, result.id, result.title);
        println!("    {}{badge}", result.subtitle);
    }
    Ok(())
}

/// Turn ranked candidates into results, marking the ones already filed.
///
/// The `[in library]` check is an exact `(kind, value)` index lookup, which is
/// what indexing serial numbers as columns in milestone 4 was for.
fn present(store: &Store, candidates: &[Candidate]) -> Result<Vec<SearchResult>> {
    let index = Index::open(store).ok();
    let mut results = Vec::new();
    for candidate in candidates {
        let citekey = match (&index, &candidate.id) {
            (Some(index), Some(id)) => index
                .by_serial(id.kind(), id.value())
                .unwrap_or_default()
                .into_iter()
                .next(),
            _ => None,
        };
        if let Some(result) = SearchResult::from_candidate(candidate, citekey) {
            results.push(result);
        }
    }
    Ok(results)
}

/// Resolve `--add`, which accepts either a 1-based position or an identifier.
fn add_one(
    loaded: &config::Loaded,
    store: &Store,
    candidates: &[Candidate],
    selector: &str,
    fetch: bool,
) -> Result<()> {
    let id = select(candidates, selector)?;
    let _ = store;
    crate::cli::add::run(
        crate::cli::add::AddArgs {
            source: Some(id.to_string()),
            fetch,
            ..Default::default()
        },
        Some(&loaded.library.name),
    )
}

fn select(candidates: &[Candidate], selector: &str) -> Result<Identifier> {
    // A bare small integer is a position in the list just printed; anything
    // else must be an identifier.
    if let Ok(position) = selector.parse::<usize>() {
        let usable: Vec<&Candidate> = candidates.iter().filter(|c| c.id.is_some()).collect();
        let candidate = usable
            .get(
                position
                    .checked_sub(1)
                    .ok_or_else(|| anyhow!("positions start at 1"))?,
            )
            .ok_or_else(|| anyhow!("no result at position {position}"))?;
        return Ok(candidate.id.clone().expect("filtered to Some above"));
    }
    patterns::parse_identifier(selector)
        .ok_or_else(|| anyhow!("`{selector}` is neither a position nor an identifier"))
}

fn prompt(results: &[SearchResult]) -> Result<Option<String>> {
    if results.is_empty() {
        eprintln!("no results");
        return Ok(None);
    }
    if !std::io::stdin().is_terminal() {
        bail!("--interactive needs a terminal; use --add instead");
    }
    for (index, result) in results.iter().enumerate() {
        eprintln!("{:>2}  {}", index + 1, result.title);
        eprintln!("    {}", result.subtitle);
    }
    eprint!("add which? [1-{}, or blank to cancel] ", results.len());
    std::io::stderr().flush()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }
    Ok(Some(answer.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::csl::{CslItem, Flexible};

    fn candidate(doi: &str) -> Candidate {
        let item = CslItem {
            title: Some(Flexible::Text("A Paper".into())),
            doi: Some(doi.to_owned()),
            ..CslItem::default()
        };
        Candidate {
            id: crate::providers::search::identifier_of(&item),
            item,
            score: 1.0,
        }
    }

    #[test]
    fn a_position_selects_from_the_list() {
        let candidates = [candidate("10.1234/a"), candidate("10.1234/b")];
        assert_eq!(select(&candidates, "2").unwrap().value(), "10.1234/b");
    }

    /// The stable form: a launcher activates a row by identifier, because the
    /// result set may have been re-queried since it was displayed.
    #[test]
    fn an_identifier_selects_regardless_of_position() {
        let candidates = [candidate("10.1234/a")];
        assert_eq!(
            select(&candidates, "doi:10.9999/elsewhere")
                .unwrap()
                .value(),
            "10.9999/elsewhere"
        );
    }

    #[test]
    fn out_of_range_positions_are_rejected() {
        let candidates = [candidate("10.1234/a")];
        assert!(select(&candidates, "5").is_err());
        assert!(select(&candidates, "0").is_err());
        assert!(select(&candidates, "nonsense").is_err());
    }
}
