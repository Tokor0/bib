//! `bib update` — re-fetch metadata for documents already in the library.
//!
//! Provenance is what makes this safe: a field a provider supplied can be
//! replaced by a fresher fetch, while a field the user typed or edited by hand
//! has no provenance entry and is left alone. Without that distinction, an
//! update would silently discard every correction anyone had made.

use crate::cli::resolve;
use crate::config;
use crate::identify::patterns::{self, Identifier};
use crate::index::{Index, query};
use crate::model::{Document, bridge};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::Args;
use serde_yaml::Value;
use std::collections::BTreeMap;

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Cite key, or a query selecting several documents.
    pub target: Vec<String>,

    /// Replace hand-edited fields too, not only ones a provider supplied.
    #[arg(long)]
    pub overwrite_local: bool,

    /// Show the changes without writing them.
    #[arg(long)]
    pub dry_run: bool,

    /// Answer from the cache only; never call a provider.
    #[arg(long)]
    pub offline: bool,

    /// Ignore the cache and re-ask every provider.
    #[arg(long, conflicts_with = "offline")]
    pub refresh: bool,
}

pub fn run(args: UpdateArgs, library: Option<&str>) -> Result<()> {
    let loaded = config::load(library)?;
    let store = Store::new(loaded.library.clone());
    let state = loaded.library.state_dir();

    let targets = select(&store, &args.target)?;
    if targets.is_empty() {
        bail!("nothing to update");
    }

    if args.refresh {
        // The cache is keyed by URL, so clearing it is how a re-ask happens.
        let _ = std::fs::remove_dir_all(state.join("cache/http"));
    }
    let http = resolve::http(&loaded.config, state.join("cache/http"), args.offline);

    let mut updated = 0usize;
    let mut failed = Vec::new();

    for doc in &targets {
        match update_one(&store, &http, &loaded.config, doc, &args) {
            Ok(true) => updated += 1,
            Ok(false) => {}
            Err(e) => failed.push((doc.citekey.clone(), e)),
        }
    }

    // Summary to stderr; the per-document lines above are the pipeable output.
    eprintln!(
        "{} of {} document(s) {}",
        updated,
        targets.len(),
        if args.dry_run {
            "would change"
        } else {
            "updated"
        }
    );
    for (citekey, error) in &failed {
        eprintln!("  {citekey}: {error:#}");
    }
    if updated == 0 && !failed.is_empty() {
        bail!("no documents could be updated");
    }
    Ok(())
}

fn update_one(
    store: &Store,
    http: &resolve::Http,
    config: &config::Config,
    doc: &Document,
    args: &UpdateArgs,
) -> Result<bool> {
    let id = identifier_of(doc)
        .with_context(|| format!("`{}` has no DOI, arXiv ID or ISBN to look up", doc.citekey))?;
    let resolved = resolve::resolve(http, config, &id)?;

    let meta = doc.meta();
    let mut body = doc.value.clone();

    let changed = apply_update(
        &mut body,
        &resolved.body,
        &meta.provenance,
        args.overwrite_local,
    );

    if changed.is_empty() {
        return Ok(false);
    }

    // Provenance is rewritten for exactly the fields that moved.
    let mut meta = meta;
    for name in &changed {
        if let Some(source) = resolved.provenance.get(name) {
            meta.provenance.insert(name.clone(), source.clone());
        }
    }
    bridge::set_meta(&mut body, serde_yaml::to_value(&meta)?)?;

    println!("{}: {}", doc.citekey, changed.join(", "));
    if args.dry_run {
        return Ok(true);
    }

    let updated = Document {
        citekey: doc.citekey.clone(),
        dir: doc.dir.clone(),
        value: body,
    };
    store.save(&updated)?;
    Ok(true)
}

/// Merge fresh provider fields into an existing body, returning what changed.
///
/// The rule that matters: a field with no provenance entry was written by hand,
/// and is left alone unless `overwrite_local`. Without that, `bib update` would
/// silently discard every correction anyone had ever made — which would make it
/// a command nobody could safely run.
pub fn apply_update(
    target: &mut Value,
    fresh: &Value,
    provenance: &BTreeMap<String, String>,
    overwrite_local: bool,
) -> Vec<String> {
    let (Value::Mapping(target), Value::Mapping(fresh)) = (target, fresh) else {
        return Vec::new();
    };
    let mut changed = Vec::new();

    for (key, value) in fresh {
        let Some(name) = key.as_str() else { continue };
        // `x-bib` is ours; no provider supplies it.
        if name == bridge::META_KEY {
            continue;
        }
        let is_local = !provenance.contains_key(name);
        if target.contains_key(key) && is_local && !overwrite_local {
            continue;
        }
        if target.get(key) == Some(value) {
            continue;
        }
        changed.push(name.to_owned());
        target.insert(key.clone(), value.clone());
    }
    changed
}

/// The identifier to re-fetch by, preferring the most specific catalogue.
fn identifier_of(doc: &Document) -> Option<Identifier> {
    let entry = doc.entry().ok()?;
    // DOI first: it reaches the published record, where arXiv reaches only the
    // preprint and an ISBN only the edition.
    if let Some(doi) = entry.doi()
        && let Some(id) = patterns::parse_identifier(&format!("doi:{doi}"))
    {
        return Some(id);
    }
    if let Some(arxiv) = entry.arxiv()
        && let Some(id) = patterns::parse_identifier(&format!("arxiv:{arxiv}"))
    {
        return Some(id);
    }
    let isbn = entry.isbn()?;
    patterns::parse_identifier(&format!("isbn:{isbn}"))
}

/// Resolve cite keys, or a query, to documents.
///
/// Cite keys win over the query language whenever *every* argument is one, so
/// `bib list -k | xargs bib fetch` addresses those documents and nothing else.
/// That composition is the only way to act on a whole library, and reading the
/// keys as one long full-text query — which is what joining them amounts to —
/// silently matches something else entirely, or nothing at all.
pub fn select(store: &Store, target: &[String]) -> Result<Vec<Document>> {
    if target.is_empty() {
        bail!("name a cite key, or a query selecting what to update");
    }
    let by_key: Option<Vec<Document>> = target.iter().map(|t| store.get(t).ok()).collect();
    if let Some(docs) = by_key {
        return Ok(docs);
    }

    let text = target.join(" ");
    let parsed = query::parse(&text).with_context(|| format!("in query `{text}`"))?;
    let mut index = Index::open(store)?;
    index.sync(store)?;
    index
        .search(&parsed)?
        .iter()
        .map(|hit| store.load(&hit.dir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).expect("fixture should be valid YAML")
    }

    fn provenance(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A library holding two documents, for the selection rules below.
    fn library() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(crate::config::ResolvedLibrary {
            name: "t".into(),
            dir: temp.path().to_path_buf(),
        });
        for (key, title) in [("smith2020a", "Alpha"), ("jones2021b", "Beta")] {
            store
                .create(
                    key,
                    std::path::Path::new(key),
                    &value(&format!("type: article\ntitle: {title}\n")),
                )
                .unwrap();
        }
        (temp, store)
    }

    /// `bib list -k | xargs bib fetch` is the only way to act on a whole
    /// library, and it hands over every key at once.
    #[test]
    fn several_cite_keys_select_those_documents() {
        let (_temp, store) = library();
        let target = ["smith2020a".to_owned(), "jones2021b".to_owned()];
        let selected = select(&store, &target).expect("both keys exist");

        let keys: Vec<_> = selected.iter().map(|d| d.citekey.as_str()).collect();
        assert_eq!(keys, ["smith2020a", "jones2021b"]);
    }

    #[test]
    fn a_single_cite_key_is_not_read_as_a_search() {
        let (_temp, store) = library();
        let selected = select(&store, &["smith2020a".to_owned()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].citekey, "smith2020a");
    }

    /// Anything that is not entirely cite keys is still a query, so
    /// `bib update author:smith` keeps working.
    #[test]
    fn words_that_are_not_all_keys_are_a_query() {
        let (_temp, store) = library();
        let selected = select(&store, &["title:Alpha".to_owned()]).expect("query should run");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].citekey, "smith2020a");
    }

    #[test]
    fn a_provider_supplied_field_is_refreshed() {
        let mut body = value("type: article\ntitle: Old Title\n");
        let fresh = value("type: article\ntitle: New Title\n");
        let changed = apply_update(
            &mut body,
            &fresh,
            &provenance(&[("title", "crossref")]),
            false,
        );

        assert_eq!(changed, ["title"]);
        assert_eq!(body["title"].as_str(), Some("New Title"));
    }

    /// The rule the whole command rests on.
    #[test]
    fn a_hand_edited_field_is_left_alone() {
        let mut body = value("type: article\ntitle: My Correction\n");
        let fresh = value("type: article\ntitle: Provider Title\n");
        // No provenance for `title` means a human wrote it.
        let changed = apply_update(&mut body, &fresh, &provenance(&[]), false);

        assert!(changed.is_empty(), "changed {changed:?}");
        assert_eq!(body["title"].as_str(), Some("My Correction"));
    }

    #[test]
    fn overwrite_local_takes_the_provider_value_anyway() {
        let mut body = value("type: article\ntitle: My Correction\n");
        let fresh = value("type: article\ntitle: Provider Title\n");
        let changed = apply_update(&mut body, &fresh, &provenance(&[]), true);

        assert_eq!(changed, ["title"]);
        assert_eq!(body["title"].as_str(), Some("Provider Title"));
    }

    /// A field the document does not have yet is always added: there is no
    /// local edit to protect.
    #[test]
    fn a_missing_field_is_filled_in() {
        let mut body = value("type: article\ntitle: T\n");
        let fresh = value("type: article\ntitle: T\nabstract: Newly available\n");
        let changed = apply_update(&mut body, &fresh, &provenance(&[]), false);

        assert_eq!(changed, ["abstract"]);
    }

    /// Our own metadata block must never be touched by a provider response.
    #[test]
    fn the_x_bib_block_is_never_overwritten() {
        let mut body = value("type: article\nx-bib:\n  tags: [mine]\n");
        let fresh = value("type: article\nx-bib:\n  tags: [theirs]\n");
        let changed = apply_update(
            &mut body,
            &fresh,
            &provenance(&[("x-bib", "crossref")]),
            true,
        );

        assert!(changed.is_empty(), "changed {changed:?}");
        assert_eq!(body["x-bib"]["tags"][0].as_str(), Some("mine"));
    }

    /// An unchanged value is not a change, or every update would rewrite every
    /// file and churn the git history.
    #[test]
    fn identical_values_are_not_reported_as_changes() {
        let mut body = value("type: article\ntitle: Same\n");
        let fresh = value("type: article\ntitle: Same\n");
        assert!(
            apply_update(
                &mut body,
                &fresh,
                &provenance(&[("title", "crossref")]),
                false
            )
            .is_empty()
        );
    }
}
