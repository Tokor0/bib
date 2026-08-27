//! `bib list`, `bib search` and `bib index`.
//!
//! Both listing commands are the same code path: `list` is `search` with an
//! empty query. Keeping them together means output flags, index freshness and
//! error reporting cannot drift apart between the two.

use crate::config;
use crate::index::{Index, Query, query};
use crate::model::Document;
use crate::store::Store;
use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct OutputArgs {
    /// Print each match as a JSON object, including the whole `info.yml`.
    #[arg(long, conflicts_with_all = ["format", "keys"])]
    pub json: bool,

    /// Render each match with a minijinja template, e.g.
    /// `--format '{{ citekey }} {{ title }}'`.
    #[arg(long, short = 'F', conflicts_with = "keys")]
    pub format: Option<String>,

    /// Print only cite keys, one per line — for piping into `bib export`.
    #[arg(long, short = 'k')]
    pub keys: bool,

    /// Report documents that could not be indexed.
    #[arg(long)]
    pub problems: bool,

    /// Query the index as it stands instead of refreshing it first.
    #[arg(long)]
    pub no_sync: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Query, e.g. `author:einstein year:1905-1910 tag:relativity`.
    ///
    /// Words are joined with spaces, so quoting the whole query is optional.
    //
    // Note `allow_hyphen_values` is deliberately NOT set: it would swallow
    // `--json` and friends into the query. A `-term` negation therefore works
    // anywhere except as the very first word, where `NOT term` says the same.
    pub query: Vec<String>,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Discard the index and build it again from the files.
    #[arg(long)]
    pub rebuild: bool,
}

pub fn list(args: ListArgs, library: Option<&str>) -> Result<()> {
    run_query(Query::All, &args.output, library)
}

pub fn search(args: SearchArgs, library: Option<&str>) -> Result<()> {
    let text = args.query.join(" ");
    let parsed = query::parse(&text).with_context(|| format!("in query `{text}`"))?;
    run_query(parsed, &args.output, library)
}

pub fn index(args: IndexArgs, library: Option<&str>) -> Result<()> {
    let store = Store::new(config::load(library)?.library);
    let mut index = Index::open(&store)?;
    if args.rebuild {
        index.reset()?;
    }

    let report = index.sync(&store)?;
    println!(
        "{} indexed, {} unchanged, {} removed ({} total)",
        report.indexed,
        report.unchanged,
        report.removed,
        index.len()?
    );
    report_failures(&report, true);
    Ok(())
}

fn run_query(query: Query, output: &OutputArgs, library: Option<&str>) -> Result<()> {
    let store = Store::new(config::load(library)?.library);
    let mut index = Index::open(&store)?;

    if !output.no_sync {
        let report = index.sync(&store)?;
        report_failures(&report, output.problems);
    }

    let hits = index.search(&query)?;

    if output.keys {
        for hit in &hits {
            println!("{}", hit.citekey);
        }
        return Ok(());
    }

    // The table view answers from the index alone. Only the modes that can show
    // arbitrary fields pay for reading and parsing the files.
    if !output.json && output.format.is_none() {
        for hit in &hits {
            let year = hit
                .year
                .map(|y| y.to_string())
                .unwrap_or_else(|| "----".into());
            let title = hit.title.clone().unwrap_or_else(|| "—".into());
            println!("{:<28}  {year}  {title}", hit.citekey);
        }
        return Ok(());
    }

    let docs: Vec<Document> = hits
        .iter()
        .map(|hit| store.load(&hit.dir))
        .collect::<Result<_>>()?;

    let rendered = match &output.format {
        Some(template) => render_template(&docs, template)?,
        None => render_json(&docs)?,
    };
    print!("{rendered}");
    Ok(())
}

/// Render library documents with a `--format` template.
///
/// Goes through the shared renderer so `bib list --format` and `bib find
/// --format` accept the same variables — a launcher recipe written for one
/// works for the other.
pub fn render_template(docs: &[Document], template: &str) -> Result<String> {
    let results: Vec<crate::cli::result::SearchResult> = docs
        .iter()
        .map(crate::cli::result::SearchResult::from_document)
        .collect::<Result<_>>()?;
    let entries: Vec<Option<hayagriva::Entry>> = docs.iter().map(|d| d.entry().ok()).collect();
    crate::cli::result::render_results(&results, &entries, template)
}

/// Serialize documents in the schema shared with `bib find --json`.
///
/// Both commands emit the same objects so a caller merging library hits with
/// web hits needs one field mapping, not two. That is the property a launcher
/// plugin depends on, and `tests/composability.rs` pins it.
pub fn render_json(docs: &[Document]) -> Result<String> {
    let results: Vec<crate::cli::result::SearchResult> = docs
        .iter()
        .map(crate::cli::result::SearchResult::from_document)
        .collect::<Result<_>>()?;
    crate::cli::result::to_json(&results)
}

fn report_failures(report: &crate::index::SyncReport, verbose: bool) {
    if report.failed.is_empty() {
        return;
    }
    eprintln!(
        "warning: {} document(s) could not be indexed",
        report.failed.len()
    );
    if verbose {
        for (path, error) in &report.failed {
            eprintln!("  {}: {error:#}", path.display());
        }
    } else {
        eprintln!("  run with --problems to see them");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::bridge;

    fn document(citekey: &str, yaml: &str) -> Document {
        Document {
            citekey: citekey.to_owned(),
            dir: std::path::PathBuf::from("/library").join(citekey),
            value: serde_yaml::from_str(yaml).expect("fixture should be valid YAML"),
        }
    }

    const EINSTEIN: &str = r#"
type: article
title: On the Electrodynamics of Moving Bodies
author: ["Einstein, Albert"]
date: 1905
x-bib:
  tags: [relativity]
  files: [paper.pdf]
"#;

    /// `--json` emits the schema shared with `bib find`, not the raw
    /// `info.yml`: a launcher merging library and web hits needs one mapping.
    /// The whole document is still reachable through `bib show`.
    #[test]
    fn json_output_uses_the_shared_launcher_schema() {
        let rendered = render_json(&[document("einstein1905", EINSTEIN)]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        let first = &parsed[0];

        assert_eq!(first["citekey"], "einstein1905");
        assert_eq!(first["source"], "library");
        assert_eq!(first["cite"], "@einstein1905");
        assert_eq!(first["in_library"], true);
        assert_eq!(first["title"], "On the Electrodynamics of Moving Bodies");
        // `x-bib` tags survive, since a list view is where they are useful.
        assert_eq!(first["tags"][0], "relativity");
        assert_eq!(
            first["dir"],
            serde_json::Value::Null,
            "dir is not in the schema"
        );
    }

    #[test]
    fn format_output_uses_the_same_filters_as_cite_keys() {
        let docs = [document("einstein1905", EINSTEIN)];
        let rendered = render_template(
            &docs,
            "{{ citekey }}\t{{ date.year }}\t{{ author[0].family }}",
        )
        .unwrap();
        assert_eq!(rendered, "einstein1905\t1905\tEinstein\n");

        // Custom filters registered for cite keys are available here too.
        let slugged = render_template(&docs, "{{ title | nostop | words(1) | lower }}").unwrap();
        assert_eq!(slugged, "electrodynamics\n");
    }

    /// A template naming a field the entry lacks must say so, not render an
    /// empty column that looks like missing data.
    #[test]
    fn a_format_referencing_a_missing_field_is_an_error() {
        let docs = [document("x", "type: article\ntitle: T\n")];
        let err = render_template(&docs, "{{ date.year }}").unwrap_err();
        assert!(
            format!("{err:#}").contains('x'),
            "the error should name the document: {err:#}"
        );
    }

    #[test]
    fn entries_that_are_not_mappings_are_rejected_rather_than_panicking() {
        let doc = Document {
            citekey: "bad".into(),
            dir: std::path::PathBuf::from("/library/bad"),
            value: serde_yaml::Value::String("not a mapping".into()),
        };
        assert!(render_json(&[doc]).is_err());
    }

    #[test]
    fn bridge_round_trip_is_unaffected_by_rendering() {
        // Guards the assumption `render_json` relies on: `doc.value` is exactly
        // what is on disk, so serializing it cannot lose fields.
        let doc = document("einstein1905", EINSTEIN);
        let entry = bridge::to_entry(&doc.citekey, &doc.value).unwrap();
        assert_eq!(
            entry.title().unwrap().to_string(),
            "On the Electrodynamics of Moving Bodies"
        );
    }
}
