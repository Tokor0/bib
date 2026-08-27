//! Index behaviour: syncing, incremental updates, and query execution.

use bib::config::ResolvedLibrary;
use bib::index::{Index, query};
use bib::store::Store;
use serde_yaml::Value;
use std::path::Path;

fn library(dir: &Path) -> Store {
    Store::new(ResolvedLibrary {
        name: "test".into(),
        dir: dir.to_path_buf(),
    })
}

fn add(store: &Store, citekey: &str, yaml: &str) {
    let body: Value = serde_yaml::from_str(yaml).expect("fixture should be valid YAML");
    store
        .create(citekey, Path::new(citekey), &body)
        .expect("fixture should be storable");
}

fn fixture(store: &Store) {
    add(
        store,
        "einstein1905",
        r#"
type: article
title: On the Electrodynamics of Moving Bodies
author: ["Einstein, Albert"]
date: 1905
serial-number: {doi: 10.1002/andp.19053221004}
parent: {type: periodical, title: Annalen der Physik}
x-bib: {tags: [relativity, classic], files: [paper.pdf]}
"#,
    );
    add(
        store,
        "knuth1997",
        r#"
type: book
title: The Art of Computer Programming
author: ["Knuth, Donald E."]
date: 1997
publisher: {name: Addison-Wesley, location: "Reading, MA"}
serial-number: {isbn: "0201896834"}
x-bib: {tags: [algorithms]}
"#,
    );
    add(
        store,
        "turing1950",
        r#"
type: article
title: Computing Machinery and Intelligence
author: ["Turing, Alan M."]
editor: ["Copeland, B. Jack"]
date: 1950
parent: {type: periodical, title: Mind}
"#,
    );
}

/// Search by query string, returning cite keys.
fn find(index: &Index, q: &str) -> Vec<String> {
    let parsed = query::parse(q).unwrap_or_else(|e| panic!("query {q:?} failed to parse: {e:#}"));
    index
        .search(&parsed)
        .unwrap_or_else(|e| panic!("query {q:?} failed to run: {e:#}"))
        .into_iter()
        .map(|h| h.citekey)
        .collect()
}

#[test]
fn sync_indexes_the_library_and_queries_find_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);

    let mut index = Index::open(&store).unwrap();
    let report = index.sync(&store).unwrap();
    assert_eq!(report.indexed, 3);
    assert!(report.failed.is_empty());
    assert_eq!(index.len().unwrap(), 3);

    assert_eq!(
        find(&index, ""),
        ["einstein1905", "knuth1997", "turing1950"]
    );
    assert_eq!(find(&index, "author:einstein"), ["einstein1905"]);
    assert_eq!(find(&index, "type:book"), ["knuth1997"]);
    assert_eq!(find(&index, "tag:relativity"), ["einstein1905"]);
    assert_eq!(
        find(&index, "year:1900-1960"),
        ["einstein1905", "turing1950"]
    );
    assert_eq!(find(&index, "journal:Mind"), ["turing1950"]);
    assert_eq!(find(&index, "publisher:addison"), ["knuth1997"]);
    assert_eq!(find(&index, "editor:copeland"), ["turing1950"]);
    // `person` spans both roles; `author` does not.
    assert_eq!(find(&index, "person:copeland"), ["turing1950"]);
    assert!(find(&index, "author:copeland").is_empty());
    assert_eq!(find(&index, "file:yes"), ["einstein1905"]);
}

#[test]
fn boolean_operators_combine_terms() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();

    assert_eq!(
        find(&index, "year:>1900 -author:einstein"),
        ["knuth1997", "turing1950"]
    );
    assert_eq!(
        find(&index, "author:knuth OR author:turing"),
        ["knuth1997", "turing1950"]
    );
    assert_eq!(
        find(&index, "(author:knuth OR author:turing) year:<1990"),
        ["turing1950"]
    );
    assert_eq!(find(&index, "NOT type:article"), ["knuth1997"]);
}

#[test]
fn full_text_search_covers_titles_authors_and_tags() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();

    // Bare words are prefix matches.
    assert_eq!(find(&index, "electro"), ["einstein1905"]);
    assert_eq!(find(&index, "algorith"), ["knuth1997"]);
    // Quoted text is a phrase: the words must be adjacent and in order.
    assert_eq!(find(&index, "\"Computer Programming\""), ["knuth1997"]);
    assert!(find(&index, "\"Programming Computer\"").is_empty());
}

/// A second sync must not reparse anything, and must not lose rows.
#[test]
fn sync_is_incremental() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();

    assert_eq!(index.sync(&store).unwrap().indexed, 3);
    let second = index.sync(&store).unwrap();
    assert_eq!(second.indexed, 0);
    assert_eq!(second.unchanged, 3);
    assert_eq!(index.len().unwrap(), 3);
}

#[test]
fn edits_and_deletions_reach_the_index() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();
    assert_eq!(find(&index, "title:electrodynamics"), ["einstein1905"]);

    // Rewrite one document.
    std::fs::write(
        temp.path().join("einstein1905/info.yml"),
        "type: article\ntitle: Something Else Entirely Now\ndate: 1905\n",
    )
    .unwrap();

    let report = index.sync(&store).unwrap();
    assert_eq!(report.indexed, 1);
    assert!(find(&index, "title:electrodynamics").is_empty());
    assert_eq!(find(&index, "title:entirely"), ["einstein1905"]);

    std::fs::remove_dir_all(temp.path().join("knuth1997")).unwrap();
    let report = index.sync(&store).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(index.len().unwrap(), 2);
    assert!(find(&index, "author:knuth").is_empty());
}

/// A document that stops parsing must lose its row, not keep serving stale
/// data from before the edit that broke it.
#[test]
fn a_document_that_breaks_is_dropped_and_reported() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();
    assert_eq!(find(&index, "author:knuth"), ["knuth1997"]);

    std::fs::write(
        temp.path().join("knuth1997/info.yml"),
        "type: notarealtype\ntitle: Broken\n",
    )
    .unwrap();

    let report = index.sync(&store).unwrap();
    assert_eq!(report.failed.len(), 1);
    assert!(find(&index, "author:knuth").is_empty());
    assert_eq!(index.len().unwrap(), 2);
}

/// Milestone 5 detects duplicates by exact serial lookup, so this is a
/// first-class query rather than a text search.
#[test]
fn serial_numbers_are_looked_up_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();

    assert_eq!(
        index.by_serial("doi", "10.1002/andp.19053221004").unwrap(),
        ["einstein1905"]
    );
    // Stored lowercased, since DOIs are case-insensitive.
    assert_eq!(
        index.by_serial("doi", "10.1002/ANDP.19053221004").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        index.by_serial("isbn", "0201896834").unwrap(),
        ["knuth1997"]
    );
    assert!(index.by_serial("doi", "10.9999/nope").unwrap().is_empty());
}

/// `_` and `%` are `LIKE` wildcards. A DOI search containing one must be a
/// literal search, or `doi:10_1002` would match `10.1002` by accident.
#[test]
fn like_wildcards_in_a_query_are_literal() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();

    assert_eq!(find(&index, "doi:10.1002"), ["einstein1905"]);
    assert!(find(&index, "doi:10_1002").is_empty());
    assert!(find(&index, "title:%").is_empty());
}

/// FTS5 has its own expression syntax. User input goes in as a quoted literal,
/// so punctuation is searched for rather than parsed as operators.
#[test]
fn fts_metacharacters_do_not_break_the_query() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);
    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();

    for probe in ["foo(bar", "\"quoted\"", "a*b", "NEAR", "^start", "a:b"] {
        let parsed = query::parse(&format!("{probe:?}")).unwrap();
        index
            .search(&parsed)
            .unwrap_or_else(|e| panic!("searching for {probe:?} errored: {e:#}"));
    }
}

/// The index is a cache: a schema change must rebuild it rather than fail.
#[test]
fn a_stale_schema_is_discarded_rather_than_migrated() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    fixture(&store);

    let path = {
        let mut index = Index::open(&store).unwrap();
        index.sync(&store).unwrap();
        index.path().to_path_buf()
    };

    // Pretend the file was written by a different version.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    drop(conn);

    let mut index = Index::open(&store).unwrap();
    assert_eq!(
        index.len().unwrap(),
        0,
        "stale index should have been dropped"
    );
    assert_eq!(index.sync(&store).unwrap().indexed, 3);
}

/// An edit that happens to preserve the file size must still be noticed.
/// Fingerprinting on `(mtime, size)` alone would miss it whenever the
/// timestamp resolution is coarser than the gap between the two writes.
#[test]
fn a_same_size_edit_is_still_detected() {
    let temp = tempfile::tempdir().unwrap();
    let store = library(temp.path());
    let info = temp.path().join("doc/info.yml");
    add(&store, "doc", "type: article\ntitle: AAAA\ndate: 1900\n");

    let mut index = Index::open(&store).unwrap();
    index.sync(&store).unwrap();
    assert_eq!(find(&index, "title:AAAA"), ["doc"]);

    let before = std::fs::metadata(&info).unwrap().len();
    std::fs::write(&info, "type: article\ntitle: BBBB\ndate: 1900\n").unwrap();
    assert_eq!(
        std::fs::metadata(&info).unwrap().len(),
        before,
        "premise: the rewrite must keep the same size"
    );

    index.sync(&store).unwrap();
    assert!(find(&index, "title:AAAA").is_empty(), "stale row survived");
    assert_eq!(find(&index, "title:BBBB"), ["doc"]);
}
